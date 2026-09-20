//! The F and D extensions.
//!
//! Two kinds of test live here. The first drive the soft-float core directly,
//! because the things that are easy to get wrong in floating point -- the
//! four rounding modes the host cannot do, the exception flags, the tiny and
//! huge ends of the range -- are awkward to reach through instructions and
//! trivial to state as values. The second drive instructions, for the parts
//! that are about the *hart* rather than the arithmetic: NaN boxing, the
//! `mstatus.FS` gate, and which encodings are illegal.
//!
//! The arithmetic is also checked against the host's own `f64` and `f32`,
//! which IEEE 754 requires to be correctly rounded in round-to-nearest. That
//! makes the host a genuine independent reference for one of the five modes,
//! which is worth more than any hand-picked table of cases.

mod common;
use common::*;

use nanoemu::cpu::{Cpu, Xlen};
use nanoemu::csr;
use nanoemu::fpu::{self, Rm, D, S};
use nanoemu::trap::Exception;

const MEM: usize = 1 << 20;

// ---------------------------------------------------------------- soft float

/// A single-precision operand, written as a host float.
fn f32b(v: f32) -> u64 {
    v.to_bits() as u64
}
/// A double-precision operand, written as a host float.
fn f64b(v: f64) -> u64 {
    v.to_bits()
}

#[test]
fn every_rounding_mode_breaks_a_tie_its_own_way() {
    // Exactly halfway between 1.0 and the next single-precision value, so
    // each mode has to decide rather than compute.
    let tie = 1.0f64 + 2f64.powi(-24);
    let up = f32::from_bits(1.0f32.to_bits() + 1);
    let cases = [
        // Nearest-even keeps 1.0, whose last bit is already even.
        (Rm::Rne, 1.0f32),
        (Rm::Rtz, 1.0),
        (Rm::Rdn, 1.0),
        (Rm::Rup, up),
        // Nearest-away is the one nearest mode that climbs off a tie.
        (Rm::Rmm, up),
    ];
    for (rm, want) in cases {
        let (v, flags) = fpu::convert(D, S, f64b(tie), rm);
        assert_eq!(v, f32b(want), "{rm:?} rounded the tie the wrong way");
        assert_eq!(flags, fpu::NX, "{rm:?} should report only inexact");
    }
}

#[test]
fn directed_rounding_overflows_to_the_largest_finite_rather_than_infinity() {
    let huge = f32b(f32::MAX);
    // Round-to-nearest overflows to infinity, but a mode rounding towards
    // zero has nowhere above the largest finite value to go.
    let (v, flags) = fpu::mul(S, huge, f32b(2.0), Rm::Rne);
    assert_eq!(v, f32b(f32::INFINITY));
    assert_eq!(flags, fpu::OF | fpu::NX);

    let (v, flags) = fpu::mul(S, huge, f32b(2.0), Rm::Rtz);
    assert_eq!(v, f32b(f32::MAX));
    assert_eq!(flags, fpu::OF | fpu::NX);

    // Rounding down still reaches -infinity: the direction is away from zero
    // on this side.
    let (v, _) = fpu::mul(S, f32b(-f32::MAX), f32b(2.0), Rm::Rdn);
    assert_eq!(v, f32b(f32::NEG_INFINITY));
    let (v, _) = fpu::mul(S, f32b(-f32::MAX), f32b(2.0), Rm::Rup);
    assert_eq!(v, f32b(-f32::MAX));
}

#[test]
fn a_subnormal_result_is_exact_unless_bits_were_actually_lost() {
    // Halving the smallest normal is exactly representable as a subnormal,
    // so it is not an underflow however tiny it is.
    let (v, flags) = fpu::div(S, f32b(f32::MIN_POSITIVE), f32b(2.0), Rm::Rne);
    assert_eq!(v, f32b(f32::MIN_POSITIVE / 2.0));
    assert_eq!(flags, 0, "an exact subnormal does not underflow");

    // Dividing the smallest subnormal loses the bit entirely.
    let tiny = f32::from_bits(1);
    let (v, flags) = fpu::div(S, f32b(tiny), f32b(4.0), Rm::Rne);
    assert_eq!(v, f32b(0.0));
    assert_eq!(flags, fpu::UF | fpu::NX);
}

#[test]
fn tininess_is_judged_after_rounding_not_before() {
    // The largest subnormal, scaled by a hair. The exact product is still
    // below the smallest normal, but rounds up onto it -- so the result is
    // normal and nothing underflowed. Detecting tininess before rounding
    // would wrongly flag this.
    let below = f64::from_bits(f64::MIN_POSITIVE.to_bits() - 1);
    let (v, flags) = fpu::mul(D, f64b(below), f64b(1.0 + 2f64.powi(-52)), Rm::Rne);
    assert_eq!(v, f64b(f64::MIN_POSITIVE));
    assert_eq!(flags, fpu::NX, "rounded up to normal, so no underflow");
}

#[test]
fn dividing_by_zero_is_not_the_same_exception_as_dividing_zero_by_zero() {
    // A finite numerator over zero has a defensible answer, so it raises
    // divide-by-zero and returns it.
    let (v, flags) = fpu::div(S, f32b(1.0), f32b(0.0), Rm::Rne);
    assert_eq!(v, f32b(f32::INFINITY));
    assert_eq!(flags, fpu::DZ);

    let (v, flags) = fpu::div(S, f32b(-1.0), f32b(0.0), Rm::Rne);
    assert_eq!(v, f32b(f32::NEG_INFINITY));
    assert_eq!(flags, fpu::DZ);

    // Zero over zero has none, so it is invalid instead.
    let (v, flags) = fpu::div(S, f32b(0.0), f32b(0.0), Rm::Rne);
    assert_eq!(v, S.nan());
    assert_eq!(flags, fpu::NV);
}

#[test]
fn zero_times_infinity_is_invalid() {
    for (a, b) in [(0.0f32, f32::INFINITY), (f32::NEG_INFINITY, -0.0)] {
        let (v, flags) = fpu::mul(S, f32b(a), f32b(b), Rm::Rne);
        assert_eq!(v, S.nan());
        assert_eq!(flags, fpu::NV);
    }
    // And it stays invalid inside a fused multiply-add, even though the
    // addend would otherwise have decided the result.
    let (v, flags) = fpu::fma(
        S,
        f32b(0.0),
        f32b(f32::INFINITY),
        f32b(1.0),
        false,
        false,
        Rm::Rne,
    );
    assert_eq!(v, S.nan());
    assert_eq!(flags, fpu::NV);
}

#[test]
fn a_nan_result_is_always_the_canonical_one() {
    // The payload of an operand NaN is not propagated: every NaN this hart
    // produces is the same bit pattern, so results do not depend on which
    // operand happened to be a NaN.
    let payload = 0x7fc1_2345u64;
    let (v, flags) = fpu::add(S, payload, f32b(1.0), Rm::Rne);
    assert_eq!(v, S.nan());
    assert_eq!(flags, 0, "a quiet NaN flowing through is not an exception");

    // A signalling NaN is, and it is quieted into the same canonical value.
    let snan = 0x7f80_0001u64;
    let (v, flags) = fpu::add(S, snan, f32b(1.0), Rm::Rne);
    assert_eq!(v, S.nan());
    assert_eq!(flags, fpu::NV);
}

#[test]
fn infinities_of_opposite_sign_have_no_difference() {
    let (v, flags) = fpu::add(D, f64b(f64::INFINITY), f64b(f64::NEG_INFINITY), Rm::Rne);
    assert_eq!(v, D.nan());
    assert_eq!(flags, fpu::NV);
    // The same two infinities with the same sign are just an infinity.
    let (v, flags) = fpu::add(D, f64b(f64::INFINITY), f64b(f64::INFINITY), Rm::Rne);
    assert_eq!(v, f64b(f64::INFINITY));
    assert_eq!(flags, 0);
}

#[test]
fn an_exact_cancellation_yields_a_zero_whose_sign_the_rounding_mode_picks() {
    // x - x is +0 in every mode but one: rounding towards minus infinity has
    // to produce the zero on that side.
    let (v, _) = fpu::sub(D, f64b(1.5), f64b(1.5), Rm::Rne);
    assert_eq!(v, f64b(0.0));
    let (v, _) = fpu::sub(D, f64b(1.5), f64b(1.5), Rm::Rdn);
    assert_eq!(v, f64b(-0.0));
    // Adding two zeroes of opposite sign is the same question.
    let (v, _) = fpu::add(D, f64b(0.0), f64b(-0.0), Rm::Rdn);
    assert_eq!(v, f64b(-0.0));
    let (v, _) = fpu::add(D, f64b(0.0), f64b(-0.0), Rm::Rne);
    assert_eq!(v, f64b(0.0));
}

#[test]
fn the_square_root_of_a_negative_is_invalid_but_of_negative_zero_is_not() {
    let (v, flags) = fpu::sqrt(D, f64b(-1.0), Rm::Rne);
    assert_eq!(v, D.nan());
    assert_eq!(flags, fpu::NV);

    // Negative zero is the one negative input with a defined root, and it
    // keeps its sign.
    let (v, flags) = fpu::sqrt(D, f64b(-0.0), Rm::Rne);
    assert_eq!(v, f64b(-0.0));
    assert_eq!(flags, 0);

    let (v, flags) = fpu::sqrt(D, f64b(4.0), Rm::Rne);
    assert_eq!(v, f64b(2.0));
    assert_eq!(flags, 0, "an exact root is not inexact");
}

#[test]
fn a_fused_multiply_add_rounds_once_where_two_operations_round_twice() {
    // (1 + 2^-52) * (1 - 2^-53) is 1 + 2^-53 - 2^-105 exactly, which needs 53
    // bits above the 1 and so does not survive being rounded to a double.
    // Subtracting the 1 first, inside the fusion, leaves a difference that
    // fits perfectly.
    let a = f64::from_bits(1.0f64.to_bits() + 1);
    let b = 1.0 - 2f64.powi(-53);
    let (v, _) = fpu::fma(D, f64b(a), f64b(b), f64b(-1.0), false, false, Rm::Rne);
    assert_eq!(v, f64b(2f64.powi(-53) - 2f64.powi(-105)));
    assert_eq!(v, f64b(a.mul_add(b, -1.0)), "host fma agrees");

    let (separate, _) = fpu::mul(D, f64b(a), f64b(b), Rm::Rne);
    let (separate, _) = fpu::add(D, separate, f64b(-1.0), Rm::Rne);
    assert_eq!(separate, f64b(0.0), "rounding twice loses it, as expected");
}

#[test]
fn the_negated_multiply_adds_negate_the_product_not_the_result() {
    // FNMSUB is -(a*b) + c, which is not -(a*b + c): the addend keeps its
    // sign. Picking a case where the two differ is the only way to tell.
    let (a, b, c) = (2.0f64, 3.0, 1.0);
    let (v, _) = fpu::fma(D, f64b(a), f64b(b), f64b(c), true, false, Rm::Rne);
    assert_eq!(v, f64b(-5.0), "-(2*3) + 1");
    let (v, _) = fpu::fma(D, f64b(a), f64b(b), f64b(c), true, true, Rm::Rne);
    assert_eq!(v, f64b(-7.0), "-(2*3) - 1");
    let (v, _) = fpu::fma(D, f64b(a), f64b(b), f64b(c), false, true, Rm::Rne);
    assert_eq!(v, f64b(5.0), "(2*3) - 1");
}

#[test]
fn converting_out_of_range_saturates_and_reports_invalid_alone() {
    // Out of range is invalid, not inexact: an implementation that reported
    // both would look right until software tested for exactness.
    let (v, flags) = fpu::to_int(S, f32b(3e9), 32, true, Rm::Rtz);
    assert_eq!(v, 0x7fff_ffff);
    assert_eq!(flags, fpu::NV);

    let (v, flags) = fpu::to_int(S, f32b(-3e9), 32, true, Rm::Rtz);
    assert_eq!(v as i64 as i32, i32::MIN);
    assert_eq!(flags, fpu::NV);

    // A NaN converts to the maximum positive value whether or not the target
    // is signed -- it does not go to zero, and it does not go negative.
    let (v, flags) = fpu::to_int(S, S.nan(), 32, true, Rm::Rtz);
    assert_eq!(v, 0x7fff_ffff);
    assert_eq!(flags, fpu::NV);
    let (v, flags) = fpu::to_int(S, S.nan(), 32, false, Rm::Rtz);
    assert_eq!(v, 0xffff_ffff);
    assert_eq!(flags, fpu::NV);

    // A negative that rounds to zero is in range, so it is merely inexact.
    let (v, flags) = fpu::to_int(S, f32b(-0.5), 32, false, Rm::Rtz);
    assert_eq!(v, 0);
    assert_eq!(flags, fpu::NX);
    // One that does not round to zero is not.
    let (v, flags) = fpu::to_int(S, f32b(-1.5), 32, false, Rm::Rtz);
    assert_eq!(v, 0);
    assert_eq!(flags, fpu::NV);
}

#[test]
fn integer_conversion_rounds_rather_than_truncating_unless_asked_to() {
    for (rm, want) in [
        (Rm::Rtz, 1i64),
        (Rm::Rne, 2),
        (Rm::Rdn, 1),
        (Rm::Rup, 2),
        (Rm::Rmm, 2),
    ] {
        let (v, flags) = fpu::to_int(D, f64b(1.5), 64, true, rm);
        assert_eq!(v as i64, want, "{rm:?}");
        assert_eq!(flags, fpu::NX);
    }
    // A tie to even goes down, which is the case truncation gets right by
    // accident and away-from-zero gets wrong.
    let (v, _) = fpu::to_int(D, f64b(2.5), 64, true, Rm::Rne);
    assert_eq!(v as i64, 2);
    let (v, _) = fpu::to_int(D, f64b(2.5), 64, true, Rm::Rmm);
    assert_eq!(v as i64, 3);
}

#[test]
fn the_most_negative_integer_converts_without_overflowing_its_own_magnitude() {
    // Negating i64::MIN is the classic trap: its magnitude does not fit back
    // into an i64, so the conversion has to work in unsigned magnitudes.
    let (v, flags) = fpu::from_int(D, true, (i64::MIN as u64).wrapping_neg(), Rm::Rne);
    assert_eq!(v, f64b(i64::MIN as f64));
    assert_eq!(flags, 0, "a power of two converts exactly");

    // And back again, which is exactly in range.
    let (v, flags) = fpu::to_int(D, f64b(i64::MIN as f64), 64, true, Rm::Rtz);
    assert_eq!(v as i64, i64::MIN);
    assert_eq!(flags, 0);

    // 2^63 itself is one past the signed range, though.
    let (_, flags) = fpu::to_int(D, f64b(-(i64::MIN as f64)), 64, true, Rm::Rtz);
    assert_eq!(flags, fpu::NV);
}

#[test]
fn an_integer_too_wide_for_the_significand_is_rounded() {
    // 2^53 + 1 has no double, so converting it must round and say so.
    let (v, flags) = fpu::from_int(D, false, (1u64 << 53) + 1, Rm::Rne);
    assert_eq!(v, f64b((1u64 << 53) as f64));
    assert_eq!(flags, fpu::NX);
    let (v, flags) = fpu::from_int(D, false, (1u64 << 53) + 1, Rm::Rup);
    assert_eq!(v, f64b((1u64 << 53) as f64 + 2.0));
    assert_eq!(flags, fpu::NX);
}

#[test]
fn min_and_max_return_the_other_operand_when_one_is_a_nan() {
    let one = f32b(1.0);
    let qnan = S.nan();
    let snan = 0x7f80_0001;

    // A quiet NaN loses to a number, and raises nothing.
    assert_eq!(fpu::min_max(S, qnan, one, false), (one, 0));
    assert_eq!(fpu::min_max(S, one, qnan, true), (one, 0));
    // A signalling one also loses, but is an exception.
    assert_eq!(fpu::min_max(S, snan, one, true), (one, fpu::NV));
    // Only when both are NaN is the result a NaN, and it is the canonical one.
    assert_eq!(fpu::min_max(S, qnan, 0x7fff_ffff, true), (qnan, 0));
}

#[test]
fn min_and_max_order_the_two_zeroes() {
    // -0 and +0 compare equal, so nothing in the ordering decides this; the
    // sign has to.
    let (neg, pos) = (f32b(-0.0), f32b(0.0));
    assert_eq!(fpu::min_max(S, neg, pos, false).0, neg);
    assert_eq!(fpu::min_max(S, pos, neg, false).0, neg);
    assert_eq!(fpu::min_max(S, neg, pos, true).0, pos);
    assert_eq!(fpu::min_max(S, pos, neg, true).0, pos);
}

#[test]
fn comparisons_differ_in_whether_a_quiet_nan_is_an_exception() {
    let nan = D.nan();
    // Equality is the quiet comparison: a quiet NaN is simply unequal.
    assert_eq!(fpu::eq(D, nan, f64b(1.0)), (false, 0));
    // The ordered comparisons signal, because an unordered pair has no
    // less-than answer at all.
    assert_eq!(fpu::lt(D, nan, f64b(1.0), false), (false, fpu::NV));
    assert_eq!(fpu::lt(D, nan, f64b(1.0), true), (false, fpu::NV));
    // A signalling NaN is an exception even to equality.
    assert_eq!(
        fpu::eq(D, 0x7ff0_0000_0000_0001, f64b(1.0)),
        (false, fpu::NV)
    );

    // The two zeroes are equal and neither is less than the other.
    assert_eq!(fpu::eq(D, f64b(-0.0), f64b(0.0)), (true, 0));
    assert_eq!(fpu::lt(D, f64b(-0.0), f64b(0.0), false), (false, 0));
    assert_eq!(fpu::lt(D, f64b(-0.0), f64b(0.0), true), (true, 0));
}

#[test]
fn classify_names_all_ten_kinds() {
    let cases = [
        (f64b(f64::NEG_INFINITY), 0),
        (f64b(-1.0), 1),
        (f64b(-f64::MIN_POSITIVE / 2.0), 2),
        (f64b(-0.0), 3),
        (f64b(0.0), 4),
        (f64b(f64::MIN_POSITIVE / 2.0), 5),
        (f64b(1.0), 6),
        (f64b(f64::INFINITY), 7),
        (0x7ff0_0000_0000_0001, 8), // signalling
        (D.nan(), 9),
    ];
    for (bits, expect) in cases {
        assert_eq!(
            fpu::classify(D, bits),
            1 << expect,
            "{bits:#x} should be class {expect}"
        );
    }
}

/// A cheap deterministic generator. Its quality does not matter: what matters
/// is that it covers exponents and significands broadly and identically on
/// every run.
struct Rand(u64);
impl Rand {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn round_to_nearest_matches_the_host_across_random_operands() {
    // IEEE 754 requires the host's own add, subtract, multiply, divide and
    // square root to be correctly rounded, so in this one mode it is an
    // independent implementation to diff against.
    let mut rng = Rand(0x243f_6a88_85a3_08d3);
    for _ in 0..200_000 {
        let (a, b) = (f64::from_bits(rng.next()), f64::from_bits(rng.next()));
        if a.is_nan() || b.is_nan() {
            continue;
        }
        let checks = [
            (fpu::add(D, f64b(a), f64b(b), Rm::Rne).0, a + b, "add"),
            (fpu::sub(D, f64b(a), f64b(b), Rm::Rne).0, a - b, "sub"),
            (fpu::mul(D, f64b(a), f64b(b), Rm::Rne).0, a * b, "mul"),
            (fpu::div(D, f64b(a), f64b(b), Rm::Rne).0, a / b, "div"),
            (
                fpu::sqrt(D, f64b(a.abs()), Rm::Rne).0,
                a.abs().sqrt(),
                "sqrt",
            ),
            (
                fpu::fma(D, f64b(a), f64b(b), f64b(a), false, false, Rm::Rne).0,
                a.mul_add(b, a),
                "fma",
            ),
        ];
        for (got, want, op) in checks {
            // Every NaN here is the canonical one; the host is free to pick
            // its own, so those cases are compared as "is a NaN".
            if want.is_nan() {
                assert_eq!(got, D.nan(), "{op}({a:e}, {b:e}) should be NaN");
            } else {
                assert_eq!(got, f64b(want), "{op}({a:e}, {b:e})");
            }
        }
    }
}

#[test]
fn single_precision_round_to_nearest_matches_the_host_too() {
    let mut rng = Rand(0x13198a2e_03707344);
    for _ in 0..200_000 {
        let a = f32::from_bits(rng.next() as u32);
        let b = f32::from_bits((rng.next() >> 32) as u32);
        if a.is_nan() || b.is_nan() {
            continue;
        }
        let checks = [
            (fpu::add(S, f32b(a), f32b(b), Rm::Rne).0, a + b, "add"),
            (fpu::mul(S, f32b(a), f32b(b), Rm::Rne).0, a * b, "mul"),
            (fpu::div(S, f32b(a), f32b(b), Rm::Rne).0, a / b, "div"),
            (
                fpu::sqrt(S, f32b(a.abs()), Rm::Rne).0,
                a.abs().sqrt(),
                "sqrt",
            ),
            // Narrowing a double is where double rounding would show up: the
            // host converts in one step, as this must.
            (
                fpu::convert(D, S, f64b(a as f64 * b as f64), Rm::Rne).0,
                (a as f64 * b as f64) as f32,
                "narrow",
            ),
        ];
        for (got, want, op) in checks {
            if want.is_nan() {
                assert_eq!(got, S.nan(), "{op}({a:e}, {b:e}) should be NaN");
            } else {
                assert_eq!(got, f32b(want), "{op}({a:e}, {b:e})");
            }
        }
    }
}

// --------------------------------------------------------------- instructions

const MSTATUS_FS: u32 = 0x6000;

fn op_fp(f5: u32, fmt: u32, rm: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    r(0x53, rm, (f5 << 2) | fmt, rd, rs1, rs2)
}
fn fadd_s(rd: u32, rs1: u32, rs2: u32, rm: u32) -> u32 {
    op_fp(0x00, 0, rm, rd, rs1, rs2)
}
fn fmv_w_x(rd: u32, rs1: u32) -> u32 {
    op_fp(0x1e, 0, 0, rd, rs1, 0)
}
fn fmv_x_w(rd: u32, rs1: u32) -> u32 {
    op_fp(0x1c, 0, 0, rd, rs1, 0)
}
fn fmv_d_x(rd: u32, rs1: u32) -> u32 {
    op_fp(0x1e, 1, 0, rd, rs1, 0)
}
fn fsgnj_s(rd: u32, rs1: u32, rs2: u32) -> u32 {
    op_fp(0x04, 0, 0, rd, rs1, rs2)
}
fn feq_s(rd: u32, rs1: u32, rs2: u32) -> u32 {
    op_fp(0x14, 0, 2, rd, rs1, rs2)
}
fn fadd_d(rd: u32, rs1: u32, rs2: u32, rm: u32) -> u32 {
    op_fp(0x00, 1, rm, rd, rs1, rs2)
}
fn fld(rd: u32, rs1: u32, imm: i32) -> u32 {
    i(0x07, 0x3, rd, rs1, imm)
}
fn fsd(rs1: u32, rs2: u32, imm: i32) -> u32 {
    s(0x27, 0x3, rs1, rs2, imm)
}
fn csrrs(rd: u32, addr: u16, rs1: u32) -> u32 {
    i(0x73, 0x2, rd, rs1, addr as i32)
}
fn csrrw(rd: u32, addr: u16, rs1: u32) -> u32 {
    i(0x73, 0x1, rd, rs1, addr as i32)
}

/// Turns the FP unit on, which every program below has to do first.
fn enable_fp() -> [u32; 2] {
    [lui(1, MSTATUS_FS), csrrs(0, csr::MSTATUS, 1)]
}

fn run_from(xlen: Xlen, prog: &[u32]) -> Cpu {
    let mut cpu = Cpu::new(MEM, xlen);
    cpu.mem.load(&image(prog));
    for _ in 0..10_000 {
        match cpu.step() {
            Ok(()) => {}
            Err(Exception::EnvironmentCall) => return cpu,
            Err(e) => panic!("unexpected trap: {e:?}"),
        }
    }
    panic!("program did not terminate");
}

fn run(prog: &[u32]) -> Cpu {
    run_from(Xlen::Rv64, prog)
}

/// Runs until the first trap and reports it, for the encodings that should
/// not execute at all.
fn trap_of(prog: &[u32]) -> Exception {
    let mut cpu = Cpu::new(MEM, Xlen::Rv64);
    cpu.mem.load(&image(prog));
    for _ in 0..10_000 {
        match cpu.step() {
            Ok(()) => {}
            Err(e) => return e,
        }
    }
    panic!("program did not trap");
}

#[test]
fn floating_point_is_unusable_until_mstatus_enables_it() {
    // FS starts at zero, meaning this context owns no FP state. Every FP
    // instruction has to trap so that an operating system finds out it needs
    // to allocate some.
    let trap = trap_of(&[fadd_s(0, 0, 0, 0), ECALL]);
    assert!(matches!(trap, Exception::IllegalInstruction(_)), "{trap:?}");

    // Including a read of the FP CSRs, which are part of that state.
    let trap = trap_of(&[csrrs(5, csr::FCSR, 0), ECALL]);
    assert!(matches!(trap, Exception::IllegalInstruction(_)), "{trap:?}");
    let trap = trap_of(&[csrrs(5, csr::FFLAGS, 0), ECALL]);
    assert!(matches!(trap, Exception::IllegalInstruction(_)), "{trap:?}");

    // And a load, which would otherwise write an f register.
    let trap = trap_of(&[i(0x07, 0x2, 0, 0, 0), ECALL]);
    assert!(matches!(trap, Exception::IllegalInstruction(_)), "{trap:?}");
}

#[test]
fn using_the_fp_unit_marks_its_state_dirty() {
    let [a, b] = enable_fp();
    // Enabling it writes 0b11 -- dirty -- but software may then write 0b01,
    // clean, and expect the next FP write to make it dirty again.
    let cpu = run(&[
        a,
        b,
        addi(2, 0, 1),
        i(0x13, 0x1, 2, 2, 13), // 1 << 13: FS = clean
        csrrw(0, csr::MSTATUS, 2),
        fmv_w_x(0, 0),
        ECALL,
    ]);
    let status = cpu.csrs.read(csr::MSTATUS);
    assert_eq!(status >> 13 & 0x3, 3, "FS should be dirty");
    // SD summarises it, and it is computed rather than stored.
    assert_eq!(status >> 63, 1, "SD should follow FS");
}

#[test]
fn a_single_precision_value_is_nan_boxed_in_its_register() {
    let [a, b] = enable_fp();
    // FMV.W.X takes the low 32 bits and boxes them; the upper half must come
    // back as all ones, or a later FMV.X.D would see a different value than
    // the one that went in.
    let cpu = run(&[a, b, lui(2, 0x3f80_0000), fmv_w_x(0, 2), ECALL]);
    assert_eq!(cpu.fregs[0], 0xffff_ffff_3f80_0000);
}

#[test]
fn an_unboxed_register_reads_as_a_nan_rather_than_as_its_low_half() {
    let [a, b] = enable_fp();
    // f0 holds a double, whose upper half is not all ones. Read as a single
    // it is not a number at all, and the spec says it must be the canonical
    // NaN -- not the low 32 bits, which would be a plausible wrong answer.
    let cpu = run(&[
        a,
        b,
        addi(2, 0, 1),
        i(0x13, 0x1, 2, 2, 62), // a double: 2.0
        fmv_d_x(0, 2),
        fadd_s(1, 0, 0, 0),
        fmv_x_w(3, 1),
        ECALL,
    ]);
    assert_eq!(cpu.regs[3] as u32, S.nan() as u32);
    // And it is an arithmetic NaN, not a signalling-NaN exception.
    assert_eq!(cpu.csrs.read(csr::FFLAGS), 0);
}

#[test]
fn moving_a_register_to_an_integer_does_not_unbox_it() {
    let [a, b] = enable_fp();
    // FMV.X.W is a raw copy of the low half, sign-extended. It is how
    // software inspects a badly-boxed register in the first place, so it must
    // not apply the NaN-boxing rule that arithmetic does.
    let cpu = run(&[
        a,
        b,
        addi(2, 0, -1),
        fmv_d_x(0, 2), // f0 = all ones, a double NaN
        fmv_x_w(3, 0),
        ECALL,
    ]);
    assert_eq!(cpu.regs[3], u64::MAX, "the low half, sign-extended");
}

#[test]
fn sign_injection_leaves_a_nan_payload_alone() {
    let [a, b] = enable_fp();
    // FSGNJ is bit manipulation, not arithmetic: fabs and fneg are built out
    // of it, and they must not canonicalise a NaN or raise a flag.
    let payload = 0x7fc1_2345u32;
    let cpu = run(&[
        a,
        b,
        lui(2, payload & 0xffff_f000),
        addi(2, 2, (payload & 0xfff) as i32),
        fmv_w_x(0, 2),
        fsgnj_s(1, 0, 0),
        fmv_x_w(3, 1),
        ECALL,
    ]);
    assert_eq!(cpu.regs[3] as u32, payload);
    assert_eq!(cpu.csrs.read(csr::FFLAGS), 0, "no flags from bit shuffling");
}

#[test]
fn a_reserved_rounding_mode_is_illegal_wherever_it_is_named() {
    let [a, b] = enable_fp();
    // Named in the instruction.
    let trap = trap_of(&[a, b, fadd_s(0, 0, 0, 5), ECALL]);
    assert!(matches!(trap, Exception::IllegalInstruction(_)), "{trap:?}");

    // Or reached through the dynamic mode, which is the case an
    // implementation that only checked the instruction would miss.
    let trap = trap_of(&[
        a,
        b,
        addi(2, 0, 7),
        csrrw(0, csr::FRM, 2),
        fadd_s(0, 0, 0, 7),
        ECALL,
    ]);
    assert!(matches!(trap, Exception::IllegalInstruction(_)), "{trap:?}");

    // A comparison has no rounding mode: funct3 selects the comparison, so
    // the same bit pattern is perfectly legal there. f0 has to be boxed
    // first -- a register at reset holds a double, not a single.
    let cpu = run(&[a, b, fmv_w_x(0, 0), feq_s(3, 0, 0), ECALL]);
    assert_eq!(cpu.regs[3], 1);
}

#[test]
fn the_flags_accumulate_rather_than_being_replaced() {
    let [a, b] = enable_fp();
    // fflags is sticky: an inexact operation followed by an exact one still
    // reports inexact, which is what lets software check once at the end.
    let cpu = run(&[
        a,
        b,
        addi(2, 0, 1),
        fmv_w_x(0, 2),              // the smallest subnormal
        fadd_s(1, 0, 0, 1),         // exact: doubling it
        op_fp(0x03, 0, 1, 2, 0, 0), // 1.0, exact
        ECALL,
    ]);
    assert_eq!(cpu.csrs.read(csr::FFLAGS), 0, "both were exact");

    let cpu = run(&[
        a,
        b,
        addi(2, 0, 1),
        fmv_w_x(0, 2),
        op_fp(0x03, 0, 1, 1, 0, 0), // 1.0 exactly
        fadd_s(2, 1, 0, 1),         // 1.0 + tiny: inexact
        fadd_s(3, 1, 1, 1),         // exact
        ECALL,
    ]);
    assert_eq!(cpu.csrs.read(csr::FFLAGS), fpu::NX as u64);
}

#[test]
fn the_double_extension_works_at_both_widths() {
    // D on RV32 is not a special case: the f registers are 64 bits wide
    // whatever the integer width. What RV32 lacks is only the moves through
    // an integer register, so the value has to be assembled in memory -- and
    // that is exactly how a 32-bit compiler does it too.
    let [a, b] = enable_fp();
    let scratch = [
        addi(2, 0, 1),
        i(0x13, 0x1, 2, 2, 31), // x2 = DRAM_BASE
        addi(2, 2, 0x100),
    ];
    for xlen in [Xlen::Rv32, Xlen::Rv64] {
        let cpu = run_from(
            xlen,
            &[
                a,
                b,
                scratch[0],
                scratch[1],
                scratch[2],
                lui(3, 0x4000_0000), // the upper half of 2.0
                sw(2, 0, 0),         // its lower half is zero
                sw(2, 3, 4),
                fld(0, 2, 0),
                fadd_d(1, 0, 0, 0), // 2.0 + 2.0
                fsd(2, 1, 8),
                lw(4, 2, 12),
                ECALL,
            ],
        );
        assert_eq!(cpu.regs[4], 0x4010_0000, "4.0 at {xlen:?}");
    }

    // FMV.X.D moves 64 bits into an integer register, which RV32 has nowhere
    // to put, so that one really does not exist there.
    let mut cpu = Cpu::new(MEM, Xlen::Rv32);
    cpu.mem
        .load(&image(&[a, b, op_fp(0x1c, 1, 0, 3, 0, 0), ECALL]));
    let mut trap = None;
    for _ in 0..100 {
        if let Err(e) = cpu.step() {
            trap = Some(e);
            break;
        }
    }
    assert!(
        matches!(trap, Some(Exception::IllegalInstruction(_))),
        "{trap:?}"
    );
}
