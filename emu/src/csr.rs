//! Control and status registers.
//!
//! Three things make this more than an array. CSRs have a minimum privilege
//! encoded in their address; some are read-only; and the supervisor views
//! `sstatus`, `sie` and `sip` are not registers of their own but masked
//! windows onto `mstatus`, `mie` and `mip`. Modelling those as aliases rather
//! than as copies is what stops the two views drifting apart.

use crate::cpu::Xlen;
use crate::trap::Priv;

// Unprivileged floating-point state. All three are windows onto one
// register: `fflags` is its low five bits and `frm` its next three.
pub const FFLAGS: u16 = 0x001;
pub const FRM: u16 = 0x002;
pub const FCSR: u16 = 0x003;

// Unprivileged counters.
pub const CYCLE: u16 = 0xC00;
pub const TIME: u16 = 0xC01;
pub const INSTRET: u16 = 0xC02;
// The high halves exist only on RV32, where the counters are wider than a
// register.
pub const CYCLEH: u16 = 0xC80;
pub const TIMEH: u16 = 0xC81;
pub const INSTRETH: u16 = 0xC82;

// Supervisor trap setup and handling.
pub const SSTATUS: u16 = 0x100;
pub const SIE: u16 = 0x104;
pub const STVEC: u16 = 0x105;
pub const SCOUNTEREN: u16 = 0x106;
pub const SSCRATCH: u16 = 0x140;
pub const SEPC: u16 = 0x141;
pub const SCAUSE: u16 = 0x142;
pub const STVAL: u16 = 0x143;
pub const SIP: u16 = 0x144;
pub const SATP: u16 = 0x180;

// Machine information.
pub const MVENDORID: u16 = 0xF11;
pub const MARCHID: u16 = 0xF12;
pub const MIMPID: u16 = 0xF13;
pub const MHARTID: u16 = 0xF14;

// Machine trap setup and handling.
pub const MSTATUS: u16 = 0x300;
pub const MISA: u16 = 0x301;
pub const MEDELEG: u16 = 0x302;
pub const MIDELEG: u16 = 0x303;
pub const MIE: u16 = 0x304;
pub const MTVEC: u16 = 0x305;
pub const MCOUNTEREN: u16 = 0x306;
pub const MSTATUSH: u16 = 0x310;
pub const MCOUNTINHIBIT: u16 = 0x320;
pub const MSCRATCH: u16 = 0x340;
pub const MEPC: u16 = 0x341;
pub const MCAUSE: u16 = 0x342;
pub const MTVAL: u16 = 0x343;
pub const MIP: u16 = 0x344;

// Machine counters.
pub const MCYCLE: u16 = 0xB00;
pub const MINSTRET: u16 = 0xB02;
pub const MCYCLEH: u16 = 0xB80;
pub const MINSTRETH: u16 = 0xB82;

/// `mstatus` field positions.
pub mod mstatus {
    pub const SIE: u64 = 1 << 1;
    pub const MIE: u64 = 1 << 3;
    pub const SPIE: u64 = 1 << 5;
    pub const MPIE: u64 = 1 << 7;
    pub const SPP: u64 = 1 << 8;
    pub const MPP_SHIFT: u32 = 11;
    pub const MPP: u64 = 3 << MPP_SHIFT;
    pub const FS_SHIFT: u32 = 13;
    pub const FS: u64 = 3 << FS_SHIFT;
    pub const XS: u64 = 3 << 15;
    pub const MPRV: u64 = 1 << 17;
    pub const SUM: u64 = 1 << 18;
    pub const MXR: u64 = 1 << 19;
    pub const TVM: u64 = 1 << 20;
    pub const TW: u64 = 1 << 21;
    pub const TSR: u64 = 1 << 22;
    pub const UXL: u64 = 3 << 32;
    pub const SXL: u64 = 3 << 34;
    pub const SD: u64 = 1 << 63;

    /// Bits software may write. Everything else is WPRI or hardwired, and
    /// silently dropping writes to them is what the spec asks for.
    pub const WRITABLE: u64 =
        SIE | MIE | SPIE | MPIE | SPP | MPP | FS | MPRV | SUM | MXR | TVM | TW | TSR;

    /// The subset visible as `sstatus`.
    pub const S_VISIBLE: u64 = SIE | SPIE | SPP | FS | XS | SUM | MXR | UXL | SD;
}

/// `fcsr` field positions.
pub mod fcsr {
    /// The accrued exception flags, in the low five bits.
    pub const FLAGS: u64 = 0x1f;
    pub const RM_SHIFT: u32 = 5;
    pub const RM: u64 = 0x7 << RM_SHIFT;
    /// Everything defined; the rest is reserved and reads as zero.
    pub const WRITABLE: u64 = FLAGS | RM;
}

/// Interrupt bits, shared by `mie`, `mip`, `sie` and `sip`.
pub mod int {
    pub const SSIP: u64 = 1 << 1;
    pub const MSIP: u64 = 1 << 3;
    pub const STIP: u64 = 1 << 5;
    pub const MTIP: u64 = 1 << 7;
    pub const SEIP: u64 = 1 << 9;
    pub const MEIP: u64 = 1 << 11;

    /// The bits a supervisor may see and touch through `sie`/`sip`.
    pub const S_VISIBLE: u64 = SSIP | STIP | SEIP;
    /// Everything this hart implements.
    pub const ALL: u64 = SSIP | MSIP | STIP | MTIP | SEIP | MEIP;
}

pub struct CsrFile {
    regs: [u64; 4096],
    xlen: Xlen,
}

impl CsrFile {
    pub fn new(xlen: Xlen) -> Self {
        let mut f = CsrFile {
            regs: [0; 4096],
            xlen,
        };
        f.regs[MISA as usize] = Self::misa(xlen);
        // UXL and SXL report the width of the lower privilege modes. They are
        // read-only here because this hart does not support running S or U at
        // a narrower width than M.
        if xlen == Xlen::Rv64 {
            f.regs[MSTATUS as usize] = (2 << 32) | (2 << 34);
        }
        f
    }

    /// Re-points the file at a different width.
    ///
    /// The width is not known until an ELF is loaded, and it changes what
    /// `misa` reports, whether `mstatush` exists, and how wide a read is. A
    /// file left at the constructor's width would quietly answer as the wrong
    /// machine.
    pub fn set_xlen(&mut self, xlen: Xlen) {
        self.xlen = xlen;
        self.regs[MISA as usize] = Self::misa(xlen);
        let status = self.regs[MSTATUS as usize] & !(mstatus::UXL | mstatus::SXL);
        self.regs[MSTATUS as usize] = match xlen {
            // UXL and SXL do not exist on RV32; the lower modes are 32-bit
            // because the machine is.
            Xlen::Rv32 => status,
            Xlen::Rv64 => status | (2 << 32) | (2 << 34),
        };
    }

    /// `misa`: the width in the top two bits, then one bit per extension
    /// letter. Reporting S and U matters because software probes it to decide
    /// whether supervisor mode exists at all.
    fn misa(xlen: Xlen) -> u64 {
        let letters = (1 << 0)   // A
            | (1 << 2)           // C
            | (1 << 3)           // D
            | (1 << 5)           // F
            | (1 << 8)           // I
            | (1 << 12)          // M
            | (1 << 18)          // S
            | (1 << 20); // U
        match xlen {
            Xlen::Rv32 => (1u64 << 30) | letters,
            Xlen::Rv64 => (2u64 << 62) | letters,
        }
    }

    /// Whether a CSR is implemented. Accessing one that is not must raise an
    /// illegal instruction rather than reading zero, which is what the
    /// `csr` conformance tests check.
    pub fn exists(&self, addr: u16) -> bool {
        matches!(
            addr,
            FFLAGS
                | FRM
                | FCSR
                | CYCLE
                | TIME
                | INSTRET
                | SSTATUS
                | SIE
                | STVEC
                | SCOUNTEREN
                | SSCRATCH
                | SEPC
                | SCAUSE
                | STVAL
                | SIP
                | SATP
                | MVENDORID
                | MARCHID
                | MIMPID
                | MHARTID
                | MSTATUS
                | MISA
                | MEDELEG
                | MIDELEG
                | MIE
                | MTVEC
                | MCOUNTEREN
                | MCOUNTINHIBIT
                | MSCRATCH
                | MEPC
                | MCAUSE
                | MTVAL
                | MIP
                | MCYCLE
                | MINSTRET
        ) || (0x3A0..=0x3AF).contains(&addr)   // pmpcfg
            || (0x3B0..=0x3EF).contains(&addr) // pmpaddr
            || (self.xlen == Xlen::Rv32
                && matches!(
                    addr,
                    MSTATUSH | MCYCLEH | MINSTRETH | CYCLEH | TIMEH | INSTRETH
                ))
    }

    /// Whether `mode` may access `addr`, and if writing, whether it is
    /// writable at all. The minimum privilege lives in bits 9:8 of the
    /// address and read-only registers are marked by bits 11:10 being set.
    pub fn accessible(addr: u16, mode: Priv, write: bool) -> bool {
        let required = Priv::from_bits(((addr >> 8) & 0x3) as u64);
        if mode < required {
            return false;
        }
        !(write && (addr >> 10) & 0x3 == 0x3)
    }

    /// Reads the architectural value, resolving the supervisor aliases.
    pub fn read(&self, addr: u16) -> u64 {
        let v = match addr {
            FFLAGS => self.regs[FCSR as usize] & fcsr::FLAGS,
            FRM => (self.regs[FCSR as usize] & fcsr::RM) >> fcsr::RM_SHIFT,
            SSTATUS => self.status() & mstatus::S_VISIBLE,
            MSTATUS => self.status(),
            SIE => self.regs[MIE as usize] & int::S_VISIBLE,
            SIP => self.regs[MIP as usize] & int::S_VISIBLE,
            CYCLE => self.regs[MCYCLE as usize],
            INSTRET => self.regs[MINSTRET as usize],
            CYCLEH => self.regs[MCYCLE as usize] >> 32,
            TIMEH => self.regs[TIME as usize] >> 32,
            INSTRETH => self.regs[MINSTRET as usize] >> 32,
            MCYCLEH => self.regs[MCYCLE as usize] >> 32,
            MINSTRETH => self.regs[MINSTRET as usize] >> 32,
            MSTATUSH => self.status() >> 32,
            _ => self.regs[addr as usize],
        };
        if self.xlen == Xlen::Rv32 {
            v & 0xffff_ffff
        } else {
            v
        }
    }

    /// `mstatus` with its derived bits filled in. SD summarises whether any
    /// extension state is dirty, so it is computed rather than stored.
    fn status(&self) -> u64 {
        let v = self.regs[MSTATUS as usize];
        if v & mstatus::FS == mstatus::FS || v & mstatus::XS == mstatus::XS {
            v | mstatus::SD
        } else {
            v & !mstatus::SD
        }
    }

    /// Writes the architectural value, applying WARL masking and resolving
    /// the supervisor aliases onto their machine registers.
    pub fn write(&mut self, addr: u16, value: u64) {
        match addr {
            FCSR => self.regs[FCSR as usize] = value & fcsr::WRITABLE,
            FFLAGS => {
                let old = self.regs[FCSR as usize];
                self.regs[FCSR as usize] = (old & !fcsr::FLAGS) | (value & fcsr::FLAGS);
            }
            // frm is WLRL, but every three-bit value is a legal encoding to
            // store; two of them are simply reserved and are rejected when an
            // instruction tries to round with them, not when they are written.
            FRM => {
                let old = self.regs[FCSR as usize];
                self.regs[FCSR as usize] =
                    (old & !fcsr::RM) | ((value << fcsr::RM_SHIFT) & fcsr::RM);
            }
            MSTATUS => {
                let old = self.regs[MSTATUS as usize];
                self.regs[MSTATUS as usize] =
                    (old & !mstatus::WRITABLE) | (value & mstatus::WRITABLE);
            }
            SSTATUS => {
                // Only the supervisor-visible writable bits may change.
                let mask = mstatus::WRITABLE & mstatus::S_VISIBLE;
                let old = self.regs[MSTATUS as usize];
                self.regs[MSTATUS as usize] = (old & !mask) | (value & mask);
            }
            SIE => {
                let old = self.regs[MIE as usize];
                self.regs[MIE as usize] = (old & !int::S_VISIBLE) | (value & int::S_VISIBLE);
            }
            MIE => self.regs[MIE as usize] = value & int::ALL,
            // Only the software-settable interrupt bits are writable in mip;
            // timer and external bits are driven by the devices.
            SIP => {
                let old = self.regs[MIP as usize];
                self.regs[MIP as usize] = (old & !int::SSIP) | (value & int::SSIP);
            }
            MIP => {
                let mask = int::SSIP | int::MSIP | int::STIP;
                let old = self.regs[MIP as usize];
                self.regs[MIP as usize] = (old & !mask) | (value & mask);
            }
            // With C implemented, IALIGN is 16, so only bit 0 is cleared.
            MEPC | SEPC => self.regs[addr as usize] = value & !1,
            // Bits 1:0 select direct or vectored mode; 2 and 3 are reserved,
            // so a write of them is held at the previous legal value.
            MTVEC | STVEC => {
                let v = if value & 0x3 > 1 { value & !0x3 } else { value };
                self.regs[addr as usize] = v;
            }
            // misa is writable in principle but this hart has a fixed set of
            // extensions, so writes are ignored rather than allowed to lie.
            MISA => {}
            // satp.MODE is WARL, and Linux probes for Sv57 and Sv48 by
            // writing a mode and reading it back. An unsupported mode must
            // leave satp unchanged, or the kernel concludes it has five-level
            // paging and builds page tables this MMU cannot walk.
            SATP => {
                let mode = match self.xlen {
                    Xlen::Rv32 => value >> 31,
                    Xlen::Rv64 => (value >> 60) & 0xf,
                };
                let supported = match self.xlen {
                    Xlen::Rv32 => mode <= 1,
                    Xlen::Rv64 => mode == 0 || mode == 8, // Bare or Sv39
                };
                if supported {
                    self.regs[SATP as usize] = value;
                }
            }
            MCYCLEH => {
                let lo = self.regs[MCYCLE as usize] & 0xffff_ffff;
                self.regs[MCYCLE as usize] = (value << 32) | lo;
            }
            MINSTRETH => {
                let lo = self.regs[MINSTRET as usize] & 0xffff_ffff;
                self.regs[MINSTRET as usize] = (value << 32) | lo;
            }
            CYCLE => self.regs[MCYCLE as usize] = value,
            INSTRET => self.regs[MINSTRET as usize] = value,
            MSTATUSH => {
                let lo = self.regs[MSTATUS as usize] & 0xffff_ffff;
                self.regs[MSTATUS as usize] = (value << 32) | lo;
            }
            _ => self.regs[addr as usize] = value,
        }
    }

    /// Writes without WARL masking, for state the hart itself maintains:
    /// counters, and the fields updated on a trap.
    pub fn force(&mut self, addr: u16, value: u64) {
        self.regs[addr as usize] = value;
    }

    pub fn set_bits(&mut self, addr: u16, bits: u64) {
        self.regs[addr as usize] |= bits;
    }

    pub fn clear_bits(&mut self, addr: u16, bits: u64) {
        self.regs[addr as usize] &= !bits;
    }
}
