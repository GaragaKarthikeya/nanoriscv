//! Virtual memory: Sv32 on RV32, Sv39 on RV64.
//!
//! The walk itself is short. What takes the space is the permission checking,
//! because every rule has a reason software depends on: SUM decides whether
//! the kernel may touch user pages, MXR whether execute-only pages are
//! readable, and the A/D bits are how the kernel learns which pages were used
//! and which need writing back.
//!
//! Vol II, "Sv32" and "Sv39".

use crate::cpu::Xlen;
use crate::csr::mstatus;
use crate::memory::Memory;
use crate::trap::{Access, Exception, Priv};

const PAGE_SIZE: u64 = 4096;

// PTE flag bits.
const V: u64 = 1 << 0;
const R: u64 = 1 << 1;
const W: u64 = 1 << 2;
const X: u64 = 1 << 3;
const U: u64 = 1 << 4;
const A: u64 = 1 << 6;
const D: u64 = 1 << 7;
/// The physical page number starts here in every PTE format.
const PPN_SHIFT: u32 = 10;

/// The translation scheme selected by `satp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Bare,
    Sv32,
    Sv39,
}

impl Mode {
    fn of(satp: u64, xlen: Xlen) -> Mode {
        match xlen {
            Xlen::Rv32 if satp >> 31 == 1 => Mode::Sv32,
            Xlen::Rv64 if (satp >> 60) & 0xf == 8 => Mode::Sv39,
            // Any other value, including the Sv48 and Sv57 encodings this
            // hart does not implement, reads as Bare.
            _ => Mode::Bare,
        }
    }

    /// Levels, bits of VPN per level, and PTE size in bytes.
    fn shape(self) -> (u32, u32, u64) {
        match self {
            Mode::Sv32 => (2, 10, 4),
            Mode::Sv39 => (3, 9, 8),
            Mode::Bare => (0, 0, 0),
        }
    }
}

/// Translates a virtual address, updating the A and D bits as it goes.
///
/// `mode` is the privilege the access is performed at, which is not always
/// the privilege the hart is running at: MPRV makes machine-mode loads and
/// stores behave as though they came from MPP, so a trap handler can reach
/// into the address space it interrupted.
pub fn translate(
    mem: &mut Memory,
    xlen: Xlen,
    satp: u64,
    status: u64,
    mode: Priv,
    va: u64,
    access: Access,
) -> Result<u64, Exception> {
    // MPRV never applies to instruction fetch.
    let effective = if access != Access::Fetch && status & mstatus::MPRV != 0 {
        Priv::from_bits((status & mstatus::MPP) >> mstatus::MPP_SHIFT)
    } else {
        mode
    };

    let scheme = Mode::of(satp, xlen);
    if scheme == Mode::Bare || effective == Priv::Machine {
        return Ok(va);
    }

    let fault = || Exception::page_fault(access, va);
    let (levels, vpn_bits, pte_size) = scheme.shape();

    // Sv39 defines only 39 address bits; the rest must be a sign extension of
    // bit 38. Addresses that are not are faulted rather than truncated, so a
    // stray pointer is caught instead of silently aliasing a valid page.
    if scheme == Mode::Sv39 {
        let top = ((va as i64) >> 38) as u64;
        if top != 0 && top != u64::MAX {
            return Err(fault());
        }
    }

    let ppn_mask = if scheme == Mode::Sv32 {
        0x3f_ffff
    } else {
        0xfff_ffff_ffff
    };
    let mut table = (satp & ppn_mask) * PAGE_SIZE;

    for level in (0..levels).rev() {
        let vpn = (va >> (12 + vpn_bits * level)) & ((1 << vpn_bits) - 1);
        let pte_addr = table + vpn * pte_size;
        let pte = mem.read(pte_addr, pte_size).map_err(|_| fault())?;

        // Not valid, or the reserved write-without-read encoding.
        if pte & V == 0 || (pte & R == 0 && pte & W != 0) {
            return Err(fault());
        }

        if pte & (R | X) == 0 {
            // A pointer to the next level down.
            if level == 0 {
                return Err(fault());
            }
            table = ((pte >> PPN_SHIFT) & ppn_mask) * PAGE_SIZE;
            continue;
        }

        // A leaf. Check that this privilege may touch the page at all.
        match effective {
            Priv::User if pte & U == 0 => return Err(fault()),
            // SUM lets the supervisor read and write user pages, but never
            // execute them: that would turn any user page into kernel code.
            Priv::Supervisor
                if pte & U != 0 && (access == Access::Fetch || status & mstatus::SUM == 0) =>
            {
                return Err(fault())
            }
            _ => {}
        }

        let permitted = match access {
            Access::Fetch => pte & X != 0,
            // MXR makes execute-only pages readable, which is how a kernel
            // reads its own instructions without mapping them writable.
            Access::Load => pte & R != 0 || (status & mstatus::MXR != 0 && pte & X != 0),
            Access::Store => pte & W != 0,
        };
        if !permitted {
            return Err(fault());
        }

        // A superpage must be aligned: the PPN bits below its level have to
        // be zero, because they come from the virtual address instead.
        let ppn = (pte >> PPN_SHIFT) & ppn_mask;
        if level > 0 {
            let low_bits = ppn & ((1 << (vpn_bits * level)) - 1);
            if low_bits != 0 {
                return Err(fault());
            }
        }

        // Update the accessed and dirty bits in place. The spec permits
        // faulting instead and letting software do it; updating here is what
        // the `dirty` conformance test expects.
        let need = A | if access == Access::Store { D } else { 0 };
        if pte & need != need {
            mem.write(pte_addr, pte_size, pte | need)
                .map_err(|_| fault())?;
        }

        // Splice: the high PPN bits come from the PTE, the low ones from the
        // virtual address, which is what makes a superpage bigger than a page.
        let split = vpn_bits * level;
        let pa_ppn = (ppn & !((1 << split) - 1)) | ((va >> 12) & ((1 << split) - 1));
        return Ok(pa_ppn * PAGE_SIZE + (va & (PAGE_SIZE - 1)));
    }

    Err(fault())
}
