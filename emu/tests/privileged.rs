//! Privilege modes, trap delegation and virtual memory.
//!
//! The conformance suite covers these, but it exercises them through long
//! assembly sequences where a failure says only "test 2 failed". These set the
//! state up directly so that a break points at one rule.

mod common;
use common::*;

use nanoemu::cpu::{Cpu, Xlen};
use nanoemu::csr::{self, mstatus};
use nanoemu::trap::{Access, Exception, Priv};
use nanoemu::{mmu, DRAM_BASE};

const MEM: usize = 1 << 20;

fn cpu_with(prog: &[u32]) -> Cpu {
    let mut cpu = Cpu::rv64(MEM);
    cpu.mem.load(&image(prog));
    cpu
}

/// `ecall`, `mret` and `sret` as raw encodings.
const MRET: u32 = 0x3020_0073;
const SRET: u32 = 0x1020_0073;
const WFI: u32 = 0x1050_0073;
const SFENCE_VMA: u32 = 0x1200_0073;

// ------------------------------------------------------------- trap delegation

#[test]
fn a_trap_goes_to_machine_mode_by_default() {
    let mut cpu = cpu_with(&[ECALL]);
    cpu.csrs.write(csr::MTVEC, 0x8000_1000);
    cpu.priv_mode = Priv::User;
    let _ = cpu.step();
    assert_eq!(cpu.priv_mode, Priv::Machine);
    assert_eq!(cpu.pc, 0x8000_1000);
    // ECALL has a distinct cause per mode; from user that is 8.
    assert_eq!(cpu.csrs.read(csr::MCAUSE), 8);
}

#[test]
fn a_delegated_trap_goes_to_supervisor_mode() {
    let mut cpu = cpu_with(&[ECALL]);
    cpu.csrs.write(csr::MTVEC, 0x8000_1000);
    cpu.csrs.write(csr::STVEC, 0x8000_2000);
    cpu.csrs.write(csr::MEDELEG, 1 << 8); // ECALL from user
    cpu.priv_mode = Priv::User;
    let _ = cpu.step();
    assert_eq!(cpu.priv_mode, Priv::Supervisor);
    assert_eq!(cpu.pc, 0x8000_2000);
    assert_eq!(cpu.csrs.read(csr::SCAUSE), 8);
    assert_eq!(cpu.csrs.read(csr::SEPC), DRAM_BASE);
}

#[test]
fn machine_mode_never_delegates_to_supervisor() {
    // Delegation only applies to traps taken from S or U. A machine-mode trap
    // stays in machine mode however medeleg is set, or the kernel could
    // capture the machine handler's own faults.
    let mut cpu = cpu_with(&[ECALL]);
    cpu.csrs.write(csr::MTVEC, 0x8000_1000);
    cpu.csrs.write(csr::STVEC, 0x8000_2000);
    cpu.csrs.write(csr::MEDELEG, !0);
    let _ = cpu.step();
    assert_eq!(cpu.priv_mode, Priv::Machine);
    assert_eq!(cpu.pc, 0x8000_1000);
    assert_eq!(cpu.csrs.read(csr::MCAUSE), 11, "ecall from machine mode");
}

#[test]
fn a_trap_saves_the_interrupt_enable_and_privilege() {
    let mut cpu = cpu_with(&[ECALL]);
    cpu.csrs.write(csr::MTVEC, 0x8000_1000);
    cpu.csrs.write(csr::MSTATUS, mstatus::MIE);
    cpu.priv_mode = Priv::Supervisor;
    let _ = cpu.step();
    let status = cpu.csrs.read(csr::MSTATUS);
    assert_eq!(status & mstatus::MIE, 0, "interrupts disabled on entry");
    assert_ne!(status & mstatus::MPIE, 0, "previous enable saved");
    assert_eq!(
        (status & mstatus::MPP) >> mstatus::MPP_SHIFT,
        Priv::Supervisor as u64
    );
}

#[test]
fn mret_restores_what_the_trap_saved() {
    let mut cpu = cpu_with(&[MRET]);
    cpu.csrs.write(csr::MEPC, 0x8000_3000);
    cpu.csrs.write(
        csr::MSTATUS,
        mstatus::MPIE | ((Priv::User as u64) << mstatus::MPP_SHIFT),
    );
    cpu.step().unwrap();
    assert_eq!(cpu.priv_mode, Priv::User);
    assert_eq!(cpu.pc, 0x8000_3000);
    let status = cpu.csrs.read(csr::MSTATUS);
    assert_ne!(status & mstatus::MIE, 0, "the saved enable is restored");
    // MPP drops to the least privileged mode, so returning twice cannot
    // accidentally land back in machine mode.
    assert_eq!(status & mstatus::MPP, 0);
}

#[test]
fn vectored_mode_fans_interrupts_out_by_cause() {
    let mut cpu = cpu_with(&[addi(0, 0, 0)]);
    cpu.csrs.write(csr::MTVEC, 0x8000_1000 | 1); // vectored
    cpu.csrs.write(csr::MSTATUS, mstatus::MIE);
    cpu.csrs.write(csr::MIE, csr::int::MSIP);
    cpu.mem.clint.msip = 1;
    cpu.step().unwrap();
    // Machine software interrupt is cause 3, so the handler is base + 4 * 3.
    assert_eq!(cpu.pc, 0x8000_1000 + 12);
}

#[test]
fn an_exception_ignores_vectored_mode() {
    // Only interrupts fan out; exceptions always land on the base address.
    let mut cpu = cpu_with(&[ECALL]);
    cpu.csrs.write(csr::MTVEC, 0x8000_1000 | 1);
    let _ = cpu.step();
    assert_eq!(cpu.pc, 0x8000_1000);
}

// --------------------------------------------------------- privileged opcodes

#[test]
fn mret_is_illegal_below_machine_mode() {
    let mut cpu = cpu_with(&[MRET]);
    cpu.priv_mode = Priv::Supervisor;
    assert!(matches!(cpu.step(), Err(Exception::IllegalInstruction(_))));
}

#[test]
fn sret_traps_when_tsr_is_set() {
    let mut cpu = cpu_with(&[SRET]);
    cpu.priv_mode = Priv::Supervisor;
    cpu.csrs.write(csr::MSTATUS, mstatus::TSR);
    assert!(matches!(cpu.step(), Err(Exception::IllegalInstruction(_))));
}

#[test]
fn wfi_traps_below_machine_mode_only_when_tw_is_set() {
    let mut cpu = cpu_with(&[WFI]);
    cpu.priv_mode = Priv::Supervisor;
    cpu.step().expect("TW clear: wfi just retires");

    let mut cpu = cpu_with(&[WFI]);
    cpu.priv_mode = Priv::Supervisor;
    cpu.csrs.write(csr::MSTATUS, mstatus::TW);
    assert!(matches!(cpu.step(), Err(Exception::IllegalInstruction(_))));
}

#[test]
fn tvm_traps_sfence_and_satp_alike() {
    // Reading the page table root is as good as walking it, so TVM has to
    // cover satp as well as the fence.
    for inst in [SFENCE_VMA, i(0x73, 0x2, 10, 0, csr::SATP as i32)] {
        let mut cpu = cpu_with(&[inst]);
        cpu.priv_mode = Priv::Supervisor;
        cpu.csrs.write(csr::MSTATUS, mstatus::TVM);
        assert!(
            matches!(cpu.step(), Err(Exception::IllegalInstruction(_))),
            "instruction {inst:#x} should trap with TVM set"
        );
    }
}

// ------------------------------------------------------------------------ CSRs

#[test]
fn an_unimplemented_csr_is_illegal_rather_than_zero() {
    // 0x7A0 is a debug trigger register; this hart has no Sdtrig support, and
    // reading zero would tell software the triggers exist.
    let mut cpu = cpu_with(&[i(0x73, 0x2, 10, 0, 0x7A0)]);
    assert!(matches!(cpu.step(), Err(Exception::IllegalInstruction(_))));
}

#[test]
fn a_supervisor_cannot_reach_a_machine_csr() {
    let mut cpu = cpu_with(&[i(0x73, 0x2, 10, 0, csr::MSTATUS as i32)]);
    cpu.priv_mode = Priv::Supervisor;
    assert!(matches!(cpu.step(), Err(Exception::IllegalInstruction(_))));
}

#[test]
fn writing_a_read_only_csr_is_illegal_but_reading_is_fine() {
    // mhartid is read-only; the distinction is the rs1 field, not the opcode.
    let mut cpu = cpu_with(&[i(0x73, 0x2, 10, 0, csr::MHARTID as i32)]);
    cpu.step().expect("csrrs with rs1 = x0 is a read");

    let mut cpu = cpu_with(&[i(0x73, 0x1, 0, 1, csr::MHARTID as i32)]);
    assert!(matches!(cpu.step(), Err(Exception::IllegalInstruction(_))));
}

#[test]
fn sstatus_is_a_window_onto_mstatus() {
    // They are not separate registers: a write through one must be visible
    // through the other, masked to what supervisor mode may see.
    let mut cpu = cpu_with(&[addi(0, 0, 0)]);
    cpu.csrs.write(csr::MSTATUS, mstatus::SIE | mstatus::MIE);
    assert_ne!(cpu.csrs.read(csr::SSTATUS) & mstatus::SIE, 0);
    assert_eq!(
        cpu.csrs.read(csr::SSTATUS) & mstatus::MIE,
        0,
        "MIE is not visible to supervisor mode"
    );

    cpu.csrs.write(csr::SSTATUS, 0);
    assert_eq!(cpu.csrs.read(csr::MSTATUS) & mstatus::SIE, 0);
    assert_ne!(
        cpu.csrs.read(csr::MSTATUS) & mstatus::MIE,
        0,
        "writing sstatus must not disturb machine-only bits"
    );
}

#[test]
fn counters_are_gated_by_mcounteren_below_machine_mode() {
    let mut cpu = cpu_with(&[i(0x73, 0x2, 10, 0, csr::CYCLE as i32)]);
    cpu.priv_mode = Priv::Supervisor;
    assert!(
        matches!(cpu.step(), Err(Exception::IllegalInstruction(_))),
        "cycle is off-limits until mcounteren allows it"
    );

    let mut cpu = cpu_with(&[i(0x73, 0x2, 10, 0, csr::CYCLE as i32)]);
    cpu.priv_mode = Priv::Supervisor;
    cpu.csrs.write(csr::MCOUNTEREN, 1);
    cpu.step().expect("permitted once the bit is set");
}

#[test]
fn writing_minstret_suppresses_that_instructions_own_increment() {
    // The value written is what the *next* instruction reads.
    let mut cpu = cpu_with(&[i(0x73, 0x5, 0, 0, csr::MINSTRET as i32), addi(0, 0, 0)]);
    cpu.step().unwrap();
    assert_eq!(cpu.csrs.read(csr::MINSTRET), 0);
    cpu.step().unwrap();
    assert_eq!(cpu.csrs.read(csr::MINSTRET), 1);
}

// -------------------------------------------------------------- virtual memory

/// Where the test page tables live: inside the 1 MiB of DRAM these tests
/// allocate, not at the end of it.
const ROOT: u64 = DRAM_BASE + 0x8_0000;

/// Builds a one-entry Sv39 page table mapping one gigapage, and returns satp.
///
/// A level-2 leaf covers 1 GiB, which is enough to map DRAM with a single PTE
/// and keeps the test about permissions rather than about table building.
fn map_gigapage(cpu: &mut Cpu, flags: u64) -> u64 {
    let root = ROOT;
    // VPN[2] of 0x8000_0000 is 2, so the third entry covers it.
    let vpn2 = (DRAM_BASE >> 30) & 0x1ff;
    let ppn = DRAM_BASE >> 12;
    let pte = (ppn << 10) | flags;
    cpu.mem.write(root + vpn2 * 8, 8, pte).unwrap();
    (8 << 60) | (root >> 12)
}

const PTE_V: u64 = 1 << 0;
const PTE_R: u64 = 1 << 1;
const PTE_W: u64 = 1 << 2;
const PTE_X: u64 = 1 << 3;
const PTE_U: u64 = 1 << 4;

fn translate(
    cpu: &mut Cpu,
    satp: u64,
    mode: Priv,
    va: u64,
    access: Access,
) -> Result<u64, Exception> {
    let status = cpu.csrs.read(csr::MSTATUS);
    mmu::translate(&mut cpu.mem, Xlen::Rv64, satp, status, mode, va, access)
}

#[test]
fn machine_mode_is_not_translated() {
    let mut cpu = cpu_with(&[]);
    let satp = map_gigapage(&mut cpu, PTE_V | PTE_R);
    // Even with paging configured, machine mode addresses are physical.
    let pa = translate(&mut cpu, satp, Priv::Machine, 0x1234, Access::Load).unwrap();
    assert_eq!(pa, 0x1234);
}

#[test]
fn a_gigapage_maps_its_whole_range() {
    let mut cpu = cpu_with(&[]);
    let satp = map_gigapage(&mut cpu, PTE_V | PTE_R | PTE_W);
    let va = DRAM_BASE + 0x12_3456;
    let pa = translate(&mut cpu, satp, Priv::Supervisor, va, Access::Load).unwrap();
    // The low bits come from the virtual address, which is what makes a
    // superpage larger than a page.
    assert_eq!(pa, va);
}

#[test]
fn permission_bits_are_enforced_per_access_kind() {
    let mut cpu = cpu_with(&[]);
    let satp = map_gigapage(&mut cpu, PTE_V | PTE_R); // readable only
    let va = DRAM_BASE;
    assert!(translate(&mut cpu, satp, Priv::Supervisor, va, Access::Load).is_ok());
    assert!(matches!(
        translate(&mut cpu, satp, Priv::Supervisor, va, Access::Store),
        Err(Exception::StorePageFault(_))
    ));
    assert!(matches!(
        translate(&mut cpu, satp, Priv::Supervisor, va, Access::Fetch),
        Err(Exception::InstructionPageFault(_))
    ));
}

#[test]
fn a_user_page_is_unreachable_from_supervisor_until_sum_is_set() {
    let mut cpu = cpu_with(&[]);
    let satp = map_gigapage(&mut cpu, PTE_V | PTE_R | PTE_X | PTE_U);
    let va = DRAM_BASE;

    assert!(translate(&mut cpu, satp, Priv::Supervisor, va, Access::Load).is_err());
    cpu.csrs.write(csr::MSTATUS, mstatus::SUM);
    assert!(translate(&mut cpu, satp, Priv::Supervisor, va, Access::Load).is_ok());
    // SUM never permits execution: that would make any user page kernel code.
    assert!(translate(&mut cpu, satp, Priv::Supervisor, va, Access::Fetch).is_err());
}

#[test]
fn a_supervisor_page_is_unreachable_from_user_mode() {
    let mut cpu = cpu_with(&[]);
    let satp = map_gigapage(&mut cpu, PTE_V | PTE_R | PTE_W);
    assert!(matches!(
        translate(&mut cpu, satp, Priv::User, DRAM_BASE, Access::Load),
        Err(Exception::LoadPageFault(_))
    ));
}

#[test]
fn mxr_makes_execute_only_pages_readable() {
    let mut cpu = cpu_with(&[]);
    let satp = map_gigapage(&mut cpu, PTE_V | PTE_X);
    let va = DRAM_BASE;
    assert!(translate(&mut cpu, satp, Priv::Supervisor, va, Access::Load).is_err());
    cpu.csrs.write(csr::MSTATUS, mstatus::MXR);
    assert!(translate(&mut cpu, satp, Priv::Supervisor, va, Access::Load).is_ok());
}

#[test]
fn the_accessed_and_dirty_bits_are_set_by_hardware() {
    let mut cpu = cpu_with(&[]);
    let satp = map_gigapage(&mut cpu, PTE_V | PTE_R | PTE_W);
    let pte_addr = ROOT + ((DRAM_BASE >> 30) & 0x1ff) * 8;

    translate(&mut cpu, satp, Priv::Supervisor, DRAM_BASE, Access::Load).unwrap();
    let pte = cpu.mem.read(pte_addr, 8).unwrap();
    assert_ne!(pte & (1 << 6), 0, "A set by a load");
    assert_eq!(pte & (1 << 7), 0, "D not set by a load");

    translate(&mut cpu, satp, Priv::Supervisor, DRAM_BASE, Access::Store).unwrap();
    let pte = cpu.mem.read(pte_addr, 8).unwrap();
    assert_ne!(pte & (1 << 7), 0, "D set by a store");
}

#[test]
fn an_invalid_pte_faults() {
    let mut cpu = cpu_with(&[]);
    let satp = map_gigapage(&mut cpu, PTE_R | PTE_W); // V clear
    assert!(translate(&mut cpu, satp, Priv::Supervisor, DRAM_BASE, Access::Load).is_err());
}

#[test]
fn a_write_only_pte_is_reserved() {
    let mut cpu = cpu_with(&[]);
    let satp = map_gigapage(&mut cpu, PTE_V | PTE_W); // W without R
    assert!(translate(&mut cpu, satp, Priv::Supervisor, DRAM_BASE, Access::Load).is_err());
}

#[test]
fn a_misaligned_superpage_faults() {
    // A level-2 leaf must have its low PPN bits clear, because those bits
    // come from the virtual address instead.
    let mut cpu = cpu_with(&[]);
    let root = ROOT;
    let vpn2 = (DRAM_BASE >> 30) & 0x1ff;
    let ppn = (DRAM_BASE >> 12) | 1; // deliberately not gigapage-aligned
    cpu.mem
        .write(root + vpn2 * 8, 8, (ppn << 10) | PTE_V | PTE_R)
        .unwrap();
    let satp = (8 << 60) | (root >> 12);
    assert!(translate(&mut cpu, satp, Priv::Supervisor, DRAM_BASE, Access::Load).is_err());
}

#[test]
fn a_non_canonical_sv39_address_faults() {
    // Sv39 defines 39 bits; the rest must be a sign extension of bit 38, so a
    // stray pointer is caught instead of aliasing a valid page.
    let mut cpu = cpu_with(&[]);
    let satp = map_gigapage(&mut cpu, PTE_V | PTE_R);
    assert!(translate(
        &mut cpu,
        satp,
        Priv::Supervisor,
        0x0000_4000_8000_0000,
        Access::Load
    )
    .is_err());
}

#[test]
fn mprv_makes_machine_loads_use_the_previous_mode() {
    // A handler uses MPRV to reach into the address space it interrupted.
    let mut cpu = cpu_with(&[]);
    let satp = map_gigapage(&mut cpu, PTE_V | PTE_R | PTE_W);
    cpu.csrs.write(
        csr::MSTATUS,
        mstatus::MPRV | ((Priv::Supervisor as u64) << mstatus::MPP_SHIFT),
    );
    // The load is translated despite the hart being in machine mode...
    assert!(translate(&mut cpu, satp, Priv::Machine, DRAM_BASE, Access::Load).is_ok());
    assert!(translate(&mut cpu, satp, Priv::Machine, 0x1234, Access::Load).is_err());
    // ...but a fetch never is.
    assert_eq!(
        translate(&mut cpu, satp, Priv::Machine, 0x1234, Access::Fetch).unwrap(),
        0x1234
    );
}

#[test]
fn an_unsupported_satp_mode_does_not_stick() {
    // Linux probes for Sv57 and Sv48 by writing the mode and reading it
    // back. Accepting a mode this MMU cannot walk makes the kernel build
    // five-level page tables and fault on its first translated access.
    let mut cpu = cpu_with(&[]);
    let sv39 = 8u64 << 60;
    cpu.csrs.write(csr::SATP, sv39 | 0x1234);
    assert_eq!(cpu.csrs.read(csr::SATP), sv39 | 0x1234);

    for unsupported in [9u64, 10] {
        cpu.csrs.write(csr::SATP, unsupported << 60);
        assert_eq!(
            cpu.csrs.read(csr::SATP) >> 60,
            8,
            "mode {unsupported} must be rejected, leaving satp alone"
        );
    }

    // Bare is always supported.
    cpu.csrs.write(csr::SATP, 0);
    assert_eq!(cpu.csrs.read(csr::SATP), 0);
}
