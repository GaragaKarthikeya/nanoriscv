//! Soft-float for the F and D extensions.
//!
//! Every operation is computed exactly in integers and rounded exactly once,
//! rather than handed to the host's `f32`/`f64`. Host arithmetic would be
//! wrong in three ways that matter here: it rounds to nearest-even only, so
//! the four other rounding modes would be unimplementable; it does not report
//! the five exception flags `fcsr` accumulates; and it is free to produce any
//! NaN it likes, where RISC-V pins down a single canonical one.
//!
//! The shape of the implementation is one rounding routine and a handful of
//! exact operations feeding it. A value is carried as `m * 2^e` with `m` an
//! integer significand held in a `u128`, which is wide enough for a
//! double-precision product (106 bits) and for a quotient or square root
//! computed to far more bits than the result needs. Whenever an operation has
//! to discard bits it ORs a one into the low bit of what it keeps -- the
//! round-to-odd trick -- so `round_pack` can round a truncated significand as
//! correctly as an exact one, in any mode, without being told which it has.
//!
//! Single and double precision share all of it, parameterised by `Fmt`, for
//! the same reason the ALU is parameterised by width: two implementations of
//! the same arithmetic drift apart.

/// Accrued exception flags, in their `fflags` bit positions.
pub const NX: u32 = 1; // inexact
pub const UF: u32 = 2; // underflow
pub const OF: u32 = 4; // overflow
pub const DZ: u32 = 8; // divide by zero
pub const NV: u32 = 16; // invalid operation

/// A rounding mode, as encoded in `frm` and in an instruction's rm field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rm {
    /// Round to nearest, ties to even.
    Rne,
    /// Round towards zero.
    Rtz,
    /// Round down, towards minus infinity.
    Rdn,
    /// Round up, towards plus infinity.
    Rup,
    /// Round to nearest, ties away from zero.
    Rmm,
}

impl Rm {
    /// Decodes a three-bit mode. The two reserved encodings and the dynamic
    /// one are `None`; an instruction naming a reserved mode is illegal, and
    /// the dynamic one has to be resolved against `frm` before it gets here.
    pub fn from_bits(v: u32) -> Option<Rm> {
        match v {
            0 => Some(Rm::Rne),
            1 => Some(Rm::Rtz),
            2 => Some(Rm::Rdn),
            3 => Some(Rm::Rup),
            4 => Some(Rm::Rmm),
            _ => None,
        }
    }
}

/// A binary floating-point format, described by its two widths. Everything
/// else about it -- the bias, the exponent range, the canonical NaN -- follows
/// from these and is derived rather than tabulated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fmt {
    /// Total width in bits.
    pub bits: u32,
    /// Stored significand bits, excluding the hidden one.
    pub sig: u32,
}

/// Single precision.
pub const S: Fmt = Fmt { bits: 32, sig: 23 };
/// Double precision.
pub const D: Fmt = Fmt { bits: 64, sig: 52 };

impl Fmt {
    const fn exp_bits(self) -> u32 {
        self.bits - self.sig - 1
    }
    const fn bias(self) -> i32 {
        (1 << (self.exp_bits() - 1)) - 1
    }
    /// The all-ones exponent field, which encodes infinity and NaN.
    const fn max_field(self) -> u64 {
        (1 << self.exp_bits()) - 1
    }
    const fn sig_mask(self) -> u64 {
        (1 << self.sig) - 1
    }
    /// The significand's top bit, which is what distinguishes a quiet NaN
    /// from a signalling one.
    const fn quiet_bit(self) -> u64 {
        1 << (self.sig - 1)
    }
    /// Every bit of the format, for trimming a wider register down.
    pub const fn mask(self) -> u64 {
        if self.bits >= 64 {
            u64::MAX
        } else {
            (1 << self.bits) - 1
        }
    }
    pub const fn sign_mask(self) -> u64 {
        1 << (self.bits - 1)
    }
    /// The canonical quiet NaN. RISC-V produces exactly this one for every
    /// operation whose result is NaN, rather than propagating an operand's
    /// payload, so that the result does not depend on the microarchitecture.
    pub const fn nan(self) -> u64 {
        (self.max_field() << self.sig) | self.quiet_bit()
    }
    pub const fn inf(self, neg: bool) -> u64 {
        (self.max_field() << self.sig) | if neg { self.sign_mask() } else { 0 }
    }
    pub const fn zero(self, neg: bool) -> u64 {
        if neg {
            self.sign_mask()
        } else {
            0
        }
    }
    /// The largest finite value, which directed rounding returns in place of
    /// an infinity on overflow.
    const fn max_finite(self, neg: bool) -> u64 {
        ((self.max_field() - 1) << self.sig) | self.sig_mask() | self.zero(neg)
    }
    /// The exponent of the least significant bit of a subnormal, in the
    /// `m * 2^e` form this module carries values in.
    const fn min_e(self) -> i32 {
        1 - self.bias() - self.sig as i32
    }
}

/// What a bit pattern denotes, with the significand of a finite non-zero
/// normalised so that every number reaching an operation has exactly
/// `sig + 1` significant bits -- subnormals included. Division and square
/// root both need that: their working precision is relative to the operand,
/// so an unnormalised subnormal would yield a quotient with too few bits to
/// round correctly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cls {
    Zero,
    Inf,
    Nan { signalling: bool },
    Num { m: u128, e: i32 },
}

pub fn unpack(f: Fmt, x: u64) -> (bool, Cls) {
    let x = x & f.mask();
    let sign = x & f.sign_mask() != 0;
    let field = (x >> f.sig) & f.max_field();
    let frac = x & f.sig_mask();
    let cls = if field == f.max_field() {
        if frac == 0 {
            Cls::Inf
        } else {
            Cls::Nan {
                signalling: frac & f.quiet_bit() == 0,
            }
        }
    } else if field == 0 {
        if frac == 0 {
            Cls::Zero
        } else {
            // Subnormal: shift the significand up until the hidden bit
            // position is occupied, paying for it in the exponent.
            let shift = f.sig + 1 - (64 - frac.leading_zeros());
            Cls::Num {
                m: (frac as u128) << shift,
                e: f.min_e() - shift as i32,
            }
        }
    } else {
        Cls::Num {
            m: ((1u64 << f.sig) | frac) as u128,
            e: field as i32 - f.bias() - f.sig as i32,
        }
    };
    (sign, cls)
}

fn is_nan(c: Cls) -> bool {
    matches!(c, Cls::Nan { .. })
}

fn is_snan(c: Cls) -> bool {
    matches!(c, Cls::Nan { signalling: true })
}

/// Rounds `sign * m * 2^e` into `f` and encodes it, reporting the flags the
/// rounding raised.
///
/// This is the only place a result is rounded. `m` may carry any number of
/// significant bits, and its low bit may be a sticky one standing in for
/// bits discarded earlier; both cases round identically, which is the point
/// of carrying the sticky bit inside the significand rather than beside it.
pub fn round_pack(f: Fmt, sign: bool, m: u128, e: i32, rm: Rm) -> (u64, u32) {
    if m == 0 {
        return (f.zero(sign), 0);
    }
    let msb = 127 - m.leading_zeros() as i32;
    // Where the result's least significant bit has to sit: one ulp below the
    // leading bit, unless that is below the subnormal floor.
    let lsb = (msb + e - f.sig as i32).max(f.min_e());
    let discard = lsb - e;

    let (mut q, guard, sticky) = if discard <= 0 {
        // Nothing is lost: the value is exactly representable once shifted.
        (m << (-discard) as u32, false, false)
    } else if discard >= 128 {
        (0, false, true)
    } else {
        let d = discard as u32;
        let low = m & ((1u128 << d) - 1);
        (
            m >> d,
            low >> (d - 1) & 1 != 0,
            low & ((1u128 << (d - 1)) - 1) != 0,
        )
    };

    let inexact = guard || sticky;
    let round_up = match rm {
        Rm::Rne => guard && (sticky || q & 1 == 1),
        Rm::Rtz => false,
        Rm::Rdn => sign && inexact,
        Rm::Rup => !sign && inexact,
        Rm::Rmm => guard,
    };
    if round_up {
        q += 1;
    }

    // Rounding up can carry out of the significand. The bit shifted away is
    // necessarily zero, so this stays exact.
    let mut e_out = lsb;
    if q >> (f.sig + 1) != 0 {
        q >>= 1;
        e_out += 1;
    }

    if q == 0 {
        // Rounded away to nothing, which is a tiny inexact result.
        return (f.zero(sign), NX | UF);
    }

    let normal = q >> f.sig != 0;
    let field = if normal {
        e_out + f.sig as i32 + f.bias()
    } else {
        0
    };
    if field as i64 >= f.max_field() as i64 {
        // Overflow. Which infinity-or-largest-finite comes back is the one
        // rounding was heading towards.
        let to_inf = match rm {
            Rm::Rne | Rm::Rmm => true,
            Rm::Rtz => false,
            Rm::Rdn => sign,
            Rm::Rup => !sign,
        };
        let bits = if to_inf {
            f.inf(sign)
        } else {
            f.max_finite(sign)
        };
        return (bits, OF | NX);
    }

    let mut flags = 0;
    if inexact {
        flags |= NX;
        // Tininess is detected after rounding: a subnormal that rounds up to
        // the smallest normal did not underflow.
        if !normal {
            flags |= UF;
        }
    }
    let bits = f.zero(sign) | ((field as u64) << f.sig) | (q as u64 & f.sig_mask());
    (bits, flags)
}

/// Rescales `m * 2^e` to the exponent `base`, ORing a sticky one into the low
/// bit if the shift discarded anything.
fn scale(m: u128, e: i32, base: i32) -> u128 {
    if e >= base {
        m << (e - base) as u32
    } else {
        let d = (base - e) as u32;
        if d >= 128 {
            u128::from(m != 0)
        } else {
            (m >> d) | u128::from(m & ((1u128 << d) - 1) != 0)
        }
    }
}

fn bit_len(m: u128) -> u32 {
    128 - m.leading_zeros()
}

/// Adds two exactly-represented signed values, keeping enough low bits that
/// the sum can still be rounded correctly.
///
/// The common exponent is placed 126 bits below the more significant
/// operand's leading bit -- as deep as a `u128` can carry a sum. The depth is
/// measured from the leading bit rather than from the exponent field, which
/// is the distinction that matters: `e` is the weight of the *lowest* bit, so
/// a wide operand like a double-precision product sits far lower than its
/// magnitude suggests, and budgeting from `e` would truncate exactly the
/// bits a subsequent cancellation exposes.
///
/// With the budget measured this way, bits are discarded only from an operand
/// whose entire range lies more than 126 bits below the other's leading bit.
/// Two such operands cannot cancel: their magnitudes differ by a factor of
/// 2^125, so the sum keeps the larger one's leading bits.
fn add_raw(sa: bool, ma: u128, ea: i32, sb: bool, mb: u128, eb: i32) -> (bool, u128, i32) {
    let top = (ea + bit_len(ma) as i32).max(eb + bit_len(mb) as i32);
    let base = top - 126;
    let a = scale(ma, ea, base);
    let b = scale(mb, eb, base);
    if sa == sb {
        (sa, a + b, base)
    } else if a >= b {
        (sa, a - b, base)
    } else {
        (sb, b - a, base)
    }
}

/// The sign of an exact zero, which only the rounding mode decides: summing
/// two values that cancel gives +0 except when rounding towards -infinity.
fn zero_sign(rm: Rm) -> bool {
    rm == Rm::Rdn
}

/// The result of an operation whose inputs made it meaningless.
fn invalid(f: Fmt) -> (u64, u32) {
    (f.nan(), NV)
}

/// Propagates NaN operands. Returns the canonical NaN, with the invalid flag
/// set only if one of them was signalling -- a quiet NaN flowing through an
/// operation is not itself an exception.
fn nan_result(f: Fmt, operands: &[Cls]) -> (u64, u32) {
    let signalling = operands.iter().any(|c| is_snan(*c));
    (f.nan(), if signalling { NV } else { 0 })
}

pub fn add(f: Fmt, a: u64, b: u64, rm: Rm) -> (u64, u32) {
    add_or_sub(f, a, b, rm, false)
}

pub fn sub(f: Fmt, a: u64, b: u64, rm: Rm) -> (u64, u32) {
    add_or_sub(f, a, b, rm, true)
}

fn add_or_sub(f: Fmt, a: u64, b: u64, rm: Rm, negate_b: bool) -> (u64, u32) {
    let (sa, ca) = unpack(f, a);
    let (sb, cb) = unpack(f, b);
    let sb = sb != negate_b;
    if is_nan(ca) || is_nan(cb) {
        return nan_result(f, &[ca, cb]);
    }
    match (ca, cb) {
        // Infinities of opposite sign have no defined difference.
        (Cls::Inf, Cls::Inf) if sa != sb => invalid(f),
        (Cls::Inf, _) => (f.inf(sa), 0),
        (_, Cls::Inf) => (f.inf(sb), 0),
        (Cls::Zero, Cls::Zero) => {
            let sign = if sa == sb { sa } else { zero_sign(rm) };
            (f.zero(sign), 0)
        }
        (Cls::Zero, _) => (b & f.mask() ^ if negate_b { f.sign_mask() } else { 0 }, 0),
        (_, Cls::Zero) => (a & f.mask(), 0),
        (Cls::Num { m: ma, e: ea }, Cls::Num { m: mb, e: eb }) => {
            let (sign, m, e) = add_raw(sa, ma, ea, sb, mb, eb);
            if m == 0 {
                return (f.zero(zero_sign(rm)), 0);
            }
            round_pack(f, sign, m, e, rm)
        }
        _ => unreachable!("NaN handled above"),
    }
}

pub fn mul(f: Fmt, a: u64, b: u64, rm: Rm) -> (u64, u32) {
    let (sa, ca) = unpack(f, a);
    let (sb, cb) = unpack(f, b);
    if is_nan(ca) || is_nan(cb) {
        return nan_result(f, &[ca, cb]);
    }
    let sign = sa != sb;
    match (ca, cb) {
        // Zero times infinity is the classic invalid case: no finite or
        // infinite answer is defensible.
        (Cls::Inf, Cls::Zero) | (Cls::Zero, Cls::Inf) => invalid(f),
        (Cls::Inf, _) | (_, Cls::Inf) => (f.inf(sign), 0),
        (Cls::Zero, _) | (_, Cls::Zero) => (f.zero(sign), 0),
        (Cls::Num { m: ma, e: ea }, Cls::Num { m: mb, e: eb }) => {
            // Exact: two significands of at most 53 bits fit a u128 product.
            round_pack(f, sign, ma * mb, ea + eb, rm)
        }
        _ => unreachable!("NaN handled above"),
    }
}

pub fn div(f: Fmt, a: u64, b: u64, rm: Rm) -> (u64, u32) {
    let (sa, ca) = unpack(f, a);
    let (sb, cb) = unpack(f, b);
    if is_nan(ca) || is_nan(cb) {
        return nan_result(f, &[ca, cb]);
    }
    let sign = sa != sb;
    match (ca, cb) {
        (Cls::Inf, Cls::Inf) | (Cls::Zero, Cls::Zero) => invalid(f),
        (Cls::Inf, _) => (f.inf(sign), 0),
        (_, Cls::Inf) => (f.zero(sign), 0),
        // A finite non-zero over zero is the one case that raises
        // divide-by-zero rather than invalid: infinity is the right answer.
        (_, Cls::Zero) => (f.inf(sign), DZ),
        (Cls::Zero, _) => (f.zero(sign), 0),
        (Cls::Num { m: ma, e: ea }, Cls::Num { m: mb, e: eb }) => {
            // Both significands are normalised, so shifting the dividend up
            // by 64 yields at least 63 quotient bits whatever the operands.
            let num = ma << 64;
            let q = num / mb;
            round_pack(f, sign, q | u128::from(num % mb != 0), ea - eb - 64, rm)
        }
        _ => unreachable!("NaN handled above"),
    }
}

pub fn sqrt(f: Fmt, a: u64, rm: Rm) -> (u64, u32) {
    let (sign, ca) = unpack(f, a);
    if is_nan(ca) {
        return nan_result(f, &[ca]);
    }
    match ca {
        // Negative zero is the one negative input with a defined root.
        Cls::Zero => (f.zero(sign), 0),
        _ if sign => invalid(f),
        Cls::Inf => (f.inf(false), 0),
        Cls::Num { m, e } => {
            // Halving the exponent needs it even, and the radicand needs
            // twice the working precision the root does.
            let shift = if (e - 64).rem_euclid(2) == 0 { 64 } else { 65 };
            let radicand = m << shift;
            let r = isqrt(radicand);
            round_pack(
                f,
                false,
                r | u128::from(r * r != radicand),
                (e - shift) / 2,
                rm,
            )
        }
        Cls::Nan { .. } => unreachable!("NaN handled above"),
    }
}

/// Integer square root, truncated. Newton's iteration from an overestimate
/// converges downward and stops the first time it would rise.
fn isqrt(n: u128) -> u128 {
    if n == 0 {
        return 0;
    }
    let mut x = 1u128 << bit_len(n).div_ceil(2);
    loop {
        let next = (x + n / x) >> 1;
        if next >= x {
            return x;
        }
        x = next;
    }
}

/// The fused multiply-add family: `(a * b) + c` with one rounding, with the
/// product and the addend optionally negated to cover all four instructions.
pub fn fma(
    f: Fmt,
    a: u64,
    b: u64,
    c: u64,
    negate_product: bool,
    negate_addend: bool,
    rm: Rm,
) -> (u64, u32) {
    let (sa, ca) = unpack(f, a);
    let (sb, cb) = unpack(f, b);
    let (sc, cc) = unpack(f, c);
    let sc = sc != negate_addend;
    let sp = (sa != sb) != negate_product;

    // Multiplying zero by infinity is invalid even when the addend is a NaN
    // that would have decided the result anyway.
    let product_invalid = matches!((ca, cb), (Cls::Inf, Cls::Zero) | (Cls::Zero, Cls::Inf));
    if product_invalid {
        return invalid(f);
    }
    if is_nan(ca) || is_nan(cb) || is_nan(cc) {
        return nan_result(f, &[ca, cb, cc]);
    }

    let product_inf = matches!(ca, Cls::Inf) || matches!(cb, Cls::Inf);
    if product_inf {
        // An infinite product plus the opposite infinity is invalid; against
        // anything else the infinity wins.
        if matches!(cc, Cls::Inf) && sc != sp {
            return invalid(f);
        }
        return (f.inf(sp), 0);
    }
    if matches!(cc, Cls::Inf) {
        return (f.inf(sc), 0);
    }

    let product = match (ca, cb) {
        (Cls::Num { m: ma, e: ea }, Cls::Num { m: mb, e: eb }) => Some((ma * mb, ea + eb)),
        _ => None, // one of them is zero
    };
    match (product, cc) {
        (None, Cls::Zero) => {
            let sign = if sp == sc { sp } else { zero_sign(rm) };
            (f.zero(sign), 0)
        }
        // A zero product leaves the addend, which is already rounded.
        (None, _) => (
            c & f.mask() ^ if negate_addend { f.sign_mask() } else { 0 },
            0,
        ),
        (Some((m, e)), Cls::Zero) => round_pack(f, sp, m, e, rm),
        (Some((m, e)), Cls::Num { m: mc, e: ec }) => {
            let (sign, m, e) = add_raw(sp, m, e, sc, mc, ec);
            if m == 0 {
                return (f.zero(zero_sign(rm)), 0);
            }
            round_pack(f, sign, m, e, rm)
        }
        _ => unreachable!("NaN and infinity handled above"),
    }
}

/// Converts between the two precisions.
pub fn convert(from: Fmt, to: Fmt, a: u64, rm: Rm) -> (u64, u32) {
    let (sign, c) = unpack(from, a);
    match c {
        Cls::Nan { .. } => nan_result(to, &[c]),
        Cls::Inf => (to.inf(sign), 0),
        Cls::Zero => (to.zero(sign), 0),
        Cls::Num { m, e } => round_pack(to, sign, m, e, rm),
    }
}

/// Rounds `m * 2^e` to an integer magnitude, reporting whether anything was
/// lost. `sign` only matters because the directed modes are asymmetric.
fn round_to_int(m: u128, e: i32, sign: bool, rm: Rm) -> (u128, bool) {
    if e >= 0 {
        return (m << e as u32, false);
    }
    let d = (-e) as u32;
    let (q, guard, sticky) = if d >= 128 {
        (0, false, true)
    } else {
        let low = m & ((1u128 << d) - 1);
        (
            m >> d,
            low >> (d - 1) & 1 != 0,
            low & ((1u128 << (d - 1)) - 1) != 0,
        )
    };
    let inexact = guard || sticky;
    let round_up = match rm {
        Rm::Rne => guard && (sticky || q & 1 == 1),
        Rm::Rtz => false,
        Rm::Rdn => sign && inexact,
        Rm::Rup => !sign && inexact,
        Rm::Rmm => guard,
    };
    (q + u128::from(round_up), inexact)
}

/// Converts a float to an integer of `width` bits.
///
/// An out-of-range value does not wrap: RISC-V returns the nearest
/// representable integer and raises invalid, and nothing else -- an invalid
/// conversion never also reports inexact.
pub fn to_int(f: Fmt, a: u64, width: u32, signed: bool, rm: Rm) -> (u64, u32) {
    let (sign, c) = unpack(f, a);
    let max_pos: u128 = if signed {
        (1u128 << (width - 1)) - 1
    } else {
        (1u128 << width) - 1
    };
    let min_mag: u128 = if signed { 1u128 << (width - 1) } else { 0 };
    // The saturated results, which double as the answers for NaN and
    // infinity. A NaN converts to the maximum positive value in both the
    // signed and the unsigned case.
    let pos = max_pos as u64;
    let neg = if signed {
        (min_mag as i128).wrapping_neg() as u64
    } else {
        0
    };

    match c {
        Cls::Nan { .. } => (pos, NV),
        Cls::Inf => (if sign { neg } else { pos }, NV),
        Cls::Zero => (0, 0),
        Cls::Num { m, e } => {
            // Shifting left past the width can only be out of range, and
            // would overflow the u128 on the way.
            if e >= 0 && bit_len(m) + e as u32 > 127 {
                return (if sign { neg } else { pos }, NV);
            }
            let (q, inexact) = round_to_int(m, e, sign, rm);
            if sign {
                if q > min_mag {
                    return (neg, NV);
                }
                let v = (q as i128).wrapping_neg() as u64;
                (v, if inexact { NX } else { 0 })
            } else {
                if q > max_pos {
                    return (pos, NV);
                }
                (q as u64, if inexact { NX } else { 0 })
            }
        }
    }
}

/// Converts an integer to a float. The caller has already split the value
/// into a sign and a magnitude, so one routine covers signed and unsigned.
pub fn from_int(f: Fmt, sign: bool, magnitude: u64, rm: Rm) -> (u64, u32) {
    round_pack(f, sign, magnitude as u128, 0, rm)
}

/// Orders two non-NaN values. Sign-and-magnitude is monotone once the
/// magnitude of a negative is negated, and both zeroes map to the same key,
/// which is what makes `-0 == +0`.
fn key(f: Fmt, x: u64) -> i64 {
    let x = x & f.mask();
    let magnitude = (x & !f.sign_mask()) as i64;
    if x & f.sign_mask() != 0 {
        -magnitude
    } else {
        magnitude
    }
}

/// `feq`: quiet, so only a signalling NaN raises invalid.
pub fn eq(f: Fmt, a: u64, b: u64) -> (bool, u32) {
    let (_, ca) = unpack(f, a);
    let (_, cb) = unpack(f, b);
    if is_nan(ca) || is_nan(cb) {
        let flags = if is_snan(ca) || is_snan(cb) { NV } else { 0 };
        return (false, flags);
    }
    (key(f, a) == key(f, b), 0)
}

/// `flt` and `fle`: signalling, so any NaN operand raises invalid.
pub fn lt(f: Fmt, a: u64, b: u64, or_equal: bool) -> (bool, u32) {
    let (_, ca) = unpack(f, a);
    let (_, cb) = unpack(f, b);
    if is_nan(ca) || is_nan(cb) {
        return (false, NV);
    }
    let (ka, kb) = (key(f, a), key(f, b));
    (if or_equal { ka <= kb } else { ka < kb }, 0)
}

/// `fmin` and `fmax`, which return the non-NaN operand when exactly one is a
/// NaN and the canonical NaN only when both are. A quiet NaN operand does not
/// raise invalid here, but a signalling one does.
pub fn min_max(f: Fmt, a: u64, b: u64, want_max: bool) -> (u64, u32) {
    let (_, ca) = unpack(f, a);
    let (_, cb) = unpack(f, b);
    let flags = if is_snan(ca) || is_snan(cb) { NV } else { 0 };
    match (is_nan(ca), is_nan(cb)) {
        (true, true) => return (f.nan(), flags),
        (true, false) => return (b & f.mask(), flags),
        (false, true) => return (a & f.mask(), flags),
        (false, false) => {}
    }
    // Two zeroes compare equal however they are ordered, so which one comes
    // back has to be decided by their signs: -0 is the smaller.
    if matches!(ca, Cls::Zero) && matches!(cb, Cls::Zero) {
        let (sa, _) = unpack(f, a);
        let take_a = sa != want_max;
        return (if take_a { a & f.mask() } else { b & f.mask() }, flags);
    }
    let take_a = if want_max {
        key(f, a) > key(f, b)
    } else {
        key(f, a) < key(f, b)
    };
    (if take_a { a & f.mask() } else { b & f.mask() }, flags)
}

/// `fclass`: a one-hot description of the operand, from negative infinity in
/// bit 0 round to a quiet NaN in bit 9.
pub fn classify(f: Fmt, a: u64) -> u64 {
    let (sign, c) = unpack(f, a);
    match c {
        Cls::Inf => {
            if sign {
                1 << 0
            } else {
                1 << 7
            }
        }
        Cls::Zero => {
            if sign {
                1 << 3
            } else {
                1 << 4
            }
        }
        Cls::Nan { signalling } => {
            if signalling {
                1 << 8
            } else {
                1 << 9
            }
        }
        Cls::Num { e, .. } => {
            // Subnormal iff the unpacked exponent sits below the normal
            // floor, which unpacking a subnormal is exactly what produces.
            let subnormal = e < f.min_e();
            match (sign, subnormal) {
                (true, false) => 1 << 1,
                (true, true) => 1 << 2,
                (false, true) => 1 << 5,
                (false, false) => 1 << 6,
            }
        }
    }
}
