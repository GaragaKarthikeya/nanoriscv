//! The SBI layer: the calls a kernel makes into the firmware beneath it.
//!
//! These matter disproportionately. A kernel makes its first SBI call before
//! it has a console driver, so anything wrong here shows up as a boot that
//! produces no output at all.

mod common;
use common::*;

use nanoemu::cpu::{Cpu, Exit};
use nanoemu::csr::{self, int, mstatus};
use nanoemu::trap::Priv;
use nanoemu::DRAM_BASE;

const MEM: usize = 1 << 20;

/// A supervisor-mode hart poised on an `ecall`, with the SBI arguments in
/// place. The arguments are placed directly rather than assembled: extension
/// ids such as "TIME" are full 32-bit values that no single instruction can
/// load, and a load sequence would shift every address these tests assert on.
fn sbi_cpu_prog(prog: &[u32], eid: u64, fid: u64, a0: u64, a1: u64) -> Cpu {
    let mut cpu = Cpu::rv64(MEM);
    cpu.mem.load(&image(prog));
    cpu.boot_supervisor(DRAM_BASE, 0, 0);
    cpu.regs[17] = eid; // a7 = extension id
    cpu.regs[16] = fid; // a6 = function id
    cpu.regs[10] = a0;
    cpu.regs[11] = a1;
    cpu
}

/// The common case: one `ecall`, then a second one to end the run.
fn sbi_cpu(eid: u64, fid: u64, a0: u64, a1: u64) -> Cpu {
    sbi_cpu_prog(&[ECALL, ECALL], eid, fid, a0, a1)
}

/// Makes one SBI call and returns the hart that made it.
fn run_sbi(eid: u64, fid: u64, a0: u64, a1: u64) -> Cpu {
    let mut cpu = sbi_cpu(eid, fid, a0, a1);
    cpu.step().unwrap();
    cpu
}

const EXT_BASE: u64 = 0x10;
const EXT_TIME: u64 = 0x5449_4D45;
const EXT_SRST: u64 = 0x5352_5354;
const EXT_DBCN: u64 = 0x4442_434E;
const EXT_IPI: u64 = 0x0073_5049;

#[test]
fn boot_enters_supervisor_mode_with_the_boot_protocol_registers() {
    let mut cpu = Cpu::rv64(MEM);
    cpu.mem.load(&image(&[addi(0, 0, 0)]));
    cpu.boot_supervisor(DRAM_BASE, 0, 0x8FE0_0000);
    assert_eq!(cpu.priv_mode, Priv::Supervisor);
    assert_eq!(cpu.pc, DRAM_BASE);
    assert_eq!(cpu.regs[10], 0, "a0 is the hart id");
    assert_eq!(cpu.regs[11], 0x8FE0_0000, "a1 points at the device tree");
}

#[test]
fn everything_but_the_supervisor_ecall_is_delegated() {
    // Cause 9 has to stay with machine mode: it is how the kernel calls the
    // firmware. Delegating it would send every SBI call back to the kernel.
    let mut cpu = Cpu::rv64(MEM);
    cpu.boot_supervisor(DRAM_BASE, 0, 0);
    let medeleg = cpu.csrs.read(csr::MEDELEG);
    assert_eq!(medeleg >> 9 & 1, 0, "ecall from S must not be delegated");
    for cause in [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 13, 15] {
        assert_eq!(medeleg >> cause & 1, 1, "cause {cause} should be delegated");
    }
}

#[test]
fn the_kernel_may_read_time_and_cycle() {
    // Without mcounteren every rdtime in the kernel is an illegal
    // instruction, and the boot dies before printing anything.
    let mut cpu = Cpu::rv64(MEM);
    cpu.mem
        .load(&image(&[i(0x73, 0x2, 10, 0, csr::TIME as i32), ECALL]));
    cpu.boot_supervisor(DRAM_BASE, 0, 0);
    cpu.step()
        .expect("rdtime must be permitted in supervisor mode");
    assert_ne!(cpu.regs[10], 0);
}

#[test]
fn a_supervisor_ecall_returns_to_the_instruction_after_it() {
    // An SBI call is a call, not a trap: it must not enter a handler.
    let cpu = run_sbi(EXT_BASE, 0, 0, 0);
    assert_eq!(cpu.priv_mode, Priv::Supervisor);
    assert_eq!(cpu.pc, DRAM_BASE + 4);
}

#[test]
fn the_base_extension_reports_a_version_and_probes() {
    let cpu = run_sbi(EXT_BASE, 0, 0, 0);
    assert_eq!(cpu.regs[10], 0, "no error");
    assert_eq!(cpu.regs[11] >> 24, 2, "spec version 2.x");

    // Probing a supported extension returns 1, an unsupported one 0.
    let cpu = run_sbi(EXT_BASE, 3, EXT_TIME, 0);
    assert_eq!(cpu.regs[11], 1);
}

#[test]
fn probing_an_unknown_extension_reports_absence_not_an_error() {
    // 0x99 is not an extension anyone implements.
    let cpu = run_sbi(EXT_BASE, 3, 0x99, 0);
    assert_eq!(cpu.regs[10], 0, "the call itself succeeded");
    assert_eq!(cpu.regs[11], 0, "the extension is absent");
}

#[test]
fn an_unknown_extension_call_is_refused() {
    let cpu = run_sbi(0x99, 0, 0, 0);
    assert_eq!(cpu.regs[10] as i64, -2, "SBI_ERR_NOT_SUPPORTED");
}

#[test]
fn the_legacy_console_writes_a_byte() {
    // earlycon=sbi uses this before any driver exists.
    let cpu = run_sbi(0x01, 0, b'A' as u64, 0);
    assert_eq!(cpu.mem.uart.output(), "A");
}

#[test]
fn the_legacy_console_reports_no_input_as_minus_one() {
    let cpu = run_sbi(0x02, 0, 0, 0);
    assert_eq!(cpu.regs[10] as i64, -1);

    let mut cpu = sbi_cpu(0x02, 0, 0, 0);
    cpu.mem.uart.push_input(b"z");
    cpu.step().unwrap();
    assert_eq!(cpu.regs[10], b'z' as u64);
}

#[test]
fn the_debug_console_writes_a_buffer_from_physical_memory() {
    // The spec gives the buffer address as the halves of a *physical*
    // address, and the kernel passes __pa(). Translating it would work only
    // until the kernel drops its early identity mapping, after which the
    // whole console would go silent.
    let text = b"sbi!";
    let buf = DRAM_BASE + 0x1000;
    let mut cpu = sbi_cpu(EXT_DBCN, 0, text.len() as u64, buf);
    cpu.mem.load_at(buf, text).unwrap();
    cpu.step().unwrap();
    assert_eq!(cpu.mem.uart.output(), "sbi!");
    assert_eq!(cpu.regs[11], text.len() as u64, "bytes written");
}

#[test]
fn setting_a_timer_schedules_a_supervisor_interrupt() {
    // The kernel cannot see the CLINT, so the firmware turns mtimecmp into
    // mip.STIP on its behalf.
    let mut cpu = sbi_cpu(EXT_TIME, 0, 5, 0);

    cpu.step().unwrap(); // makes the call
    assert!(cpu.timer_armed);
    assert_eq!(cpu.mem.clint.mtimecmp, 5);
    // mtime has already passed 5 while running those instructions, so the
    // interrupt is raised on the next tick.
    cpu.run(2);
    assert_ne!(cpu.csrs.read(csr::MIP) & int::STIP, 0);
}

#[test]
fn a_scheduled_timer_actually_interrupts_the_kernel() {
    let mut prog = vec![ECALL];
    prog.extend([addi(0, 0, 0); 12]);
    let mut cpu = sbi_cpu_prog(&prog, EXT_TIME, 0, 8, 0);
    cpu.csrs.write(csr::STVEC, DRAM_BASE + 0x400);
    cpu.mem
        .load_at(DRAM_BASE + 0x400, &image(&[addi(0, 0, 0); 32]))
        .unwrap();
    cpu.csrs.write(csr::MSTATUS, mstatus::SIE);
    cpu.csrs.write(csr::SIE, int::STIP);

    cpu.run(13);
    let sepc = cpu.csrs.read(csr::SEPC);
    assert!(
        (DRAM_BASE + 4..DRAM_BASE + 13 * 4).contains(&sepc),
        "interrupted mid-stream, at {sepc:#x}"
    );
    assert!(
        (DRAM_BASE + 0x400..DRAM_BASE + 0x480).contains(&cpu.pc),
        "running in the timer handler, at {:#x}",
        cpu.pc
    );
    // Supervisor timer is interrupt cause 5.
    assert_eq!(cpu.csrs.read(csr::SCAUSE), (1 << 63) | 5);
}

#[test]
fn system_reset_stops_the_machine() {
    let mut cpu = sbi_cpu(EXT_SRST, 0, 0, 0);
    assert_eq!(cpu.run(10), Exit::Shutdown);
    assert!(cpu.shutdown);
}

#[test]
fn the_legacy_shutdown_call_also_stops_the_machine() {
    let mut cpu = sbi_cpu(0x08, 0, 0, 0);
    assert_eq!(cpu.run(10), Exit::Shutdown);
}

#[test]
fn a_user_mode_ecall_is_still_a_trap() {
    // Only supervisor ecalls are SBI calls. A user ecall is a syscall and
    // belongs to the kernel, delegated to supervisor mode.
    let mut cpu = Cpu::rv64(MEM);
    cpu.mem.load(&image(&[ECALL]));
    cpu.boot_supervisor(DRAM_BASE, 0, 0);
    cpu.priv_mode = Priv::User;
    cpu.csrs.write(csr::STVEC, DRAM_BASE + 0x400);
    let _ = cpu.step();
    assert_eq!(cpu.priv_mode, Priv::Supervisor);
    assert_eq!(cpu.pc, DRAM_BASE + 0x400);
    assert_eq!(cpu.csrs.read(csr::SCAUSE), 8, "ecall from user mode");
}

/// A user-mode ecall is a syscall for the kernel, not a reason to stop.
///
/// The emulator ends a bare payload's run on an ecall, because a test binary
/// with no `tohost` symbol has no other way to say it is finished. A booted
/// kernel has no `tohost` either, so without the privilege check that rule
/// swallowed the first syscall userspace ever made -- the run ended at the
/// instant it became interesting.
#[test]
fn a_user_syscall_does_not_end_the_run() {
    let mut cpu = sbi_cpu_prog(&[ECALL, addi(0, 0, 0), addi(0, 0, 0)], 0, 0, 0, 0);
    cpu.priv_mode = Priv::User;
    cpu.csrs.write(csr::STVEC, DRAM_BASE + 0x400);
    cpu.mem
        .load_at(DRAM_BASE + 0x400, &image(&[addi(0, 0, 0); 4]))
        .unwrap();
    assert_eq!(cpu.mem.tohost, None, "a kernel Image declares no tohost");

    assert_eq!(cpu.run(2), Exit::StepLimit, "the run kept going");
    assert_eq!(
        cpu.pc & !0x3,
        DRAM_BASE + 0x400 + 4,
        "the kernel handled it"
    );
    assert_eq!(cpu.csrs.read(csr::SCAUSE), 8, "ecall from user mode");
}

/// Sending an IPI has to actually raise one, even with a single hart.
///
/// The hart a kernel interrupts here is itself: RISC-V delivers irq_work by
/// self-IPI, and irq_work runs the deferred callbacks that end an RCU grace
/// period. A firmware that answers "success" without raising SSIP leaves
/// irq_work_needs_cpu() true for good, and anything waiting on a grace
/// period -- unregistering a console, for one -- waits forever.
#[test]
fn a_self_ipi_raises_a_supervisor_software_interrupt() {
    let mut cpu = run_sbi(EXT_IPI, 0, 1, 0); // hart 0 in the mask, base 0
    assert_eq!(cpu.regs[10], 0, "no error");
    assert_ne!(cpu.csrs.read(csr::MIP) & int::SSIP, 0, "SSIP raised");

    // A mask that names no hart raises nothing.
    let cpu2 = run_sbi(EXT_IPI, 0, 0, 0);
    assert_eq!(cpu2.csrs.read(csr::MIP) & int::SSIP, 0);

    // And the kernel is actually interrupted by it.
    cpu.csrs.write(csr::STVEC, DRAM_BASE + 0x400);
    cpu.csrs.write(csr::MSTATUS, mstatus::SIE);
    cpu.csrs.write(csr::SIE, int::SSIP);
    cpu.run(1);
    assert_eq!(cpu.pc, DRAM_BASE + 0x400, "entered the handler");
    assert_eq!(
        cpu.csrs.read(csr::SCAUSE),
        (1 << 63) | 1,
        "supervisor software"
    );
}

/// A base of all-ones means every hart, whatever the mask says.
#[test]
fn an_ipi_to_every_hart_ignores_the_mask() {
    let cpu = run_sbi(EXT_IPI, 0, 0, u64::MAX);
    assert_ne!(cpu.csrs.read(csr::MIP) & int::SSIP, 0);
}
