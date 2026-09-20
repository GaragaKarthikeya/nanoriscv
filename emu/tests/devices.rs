//! The PLIC and the UART.
//!
//! Neither has a conformance suite, so these tests are the only thing
//! standing between a plausible-looking device and one Linux will actually
//! talk to. They work the registers the way a driver does rather than
//! checking fields in isolation.

mod common;
use common::*;

use nanoemu::cpu::Cpu;
use nanoemu::csr::{self, int, mstatus};
use nanoemu::plic::{CONTEXT_MACHINE, CONTEXT_SUPERVISOR, PLIC_BASE};
use nanoemu::trap::{Exception, Priv};
use nanoemu::uart::{UART_BASE, UART_IRQ};
use nanoemu::DRAM_BASE;

const MEM: usize = 1 << 20;

// PLIC register addresses, as a driver computes them.
fn priority(source: u32) -> u64 {
    PLIC_BASE + 4 * source as u64
}
const PENDING: u64 = PLIC_BASE + 0x1000;
fn enable(context: usize) -> u64 {
    PLIC_BASE + 0x2000 + 0x80 * context as u64
}
fn threshold(context: usize) -> u64 {
    PLIC_BASE + 0x20_0000 + 0x1000 * context as u64
}
fn claim(context: usize) -> u64 {
    threshold(context) + 4
}

fn new_cpu() -> Cpu {
    Cpu::rv64(MEM)
}

/// Reads a device register the way the hart would.
fn rd(cpu: &mut Cpu, addr: u64) -> u64 {
    cpu.mem.read(addr, 4).expect("device register is mapped")
}
fn wr(cpu: &mut Cpu, addr: u64, value: u64) {
    cpu.mem.write(addr, 4, value).expect("device is writable");
}

// ------------------------------------------------------------------------ PLIC

#[test]
fn a_source_needs_priority_enable_and_threshold_to_be_pending() {
    let mut cpu = new_cpu();
    cpu.mem.plic.set_level(UART_IRQ, true);

    // Asserted, but priority 0 means "never interrupt".
    assert!(!cpu.mem.plic.is_pending(CONTEXT_MACHINE));

    wr(&mut cpu, priority(UART_IRQ), 1);
    assert!(!cpu.mem.plic.is_pending(CONTEXT_MACHINE), "not yet enabled");

    wr(&mut cpu, enable(CONTEXT_MACHINE), 1 << UART_IRQ);
    assert!(cpu.mem.plic.is_pending(CONTEXT_MACHINE));

    // A threshold equal to the priority masks it: the comparison is strict.
    wr(&mut cpu, threshold(CONTEXT_MACHINE), 1);
    assert!(!cpu.mem.plic.is_pending(CONTEXT_MACHINE));
}

#[test]
fn contexts_are_routed_independently() {
    let mut cpu = new_cpu();
    cpu.mem.plic.set_level(UART_IRQ, true);
    wr(&mut cpu, priority(UART_IRQ), 1);
    wr(&mut cpu, enable(CONTEXT_SUPERVISOR), 1 << UART_IRQ);

    assert!(!cpu.mem.plic.is_pending(CONTEXT_MACHINE));
    assert!(cpu.mem.plic.is_pending(CONTEXT_SUPERVISOR));
}

#[test]
fn claiming_returns_the_source_and_stops_it_interrupting() {
    let mut cpu = new_cpu();
    cpu.mem.plic.set_level(UART_IRQ, true);
    wr(&mut cpu, priority(UART_IRQ), 1);
    wr(&mut cpu, enable(CONTEXT_MACHINE), 1 << UART_IRQ);

    assert_eq!(rd(&mut cpu, claim(CONTEXT_MACHINE)), UART_IRQ as u64);
    // The line is still asserted, but a claimed source must not re-interrupt
    // before it is completed -- otherwise the handler never gets to return.
    assert!(!cpu.mem.plic.is_pending(CONTEXT_MACHINE));
    assert_eq!(rd(&mut cpu, PENDING) & (1 << UART_IRQ), 0);
}

#[test]
fn completing_a_still_asserted_source_makes_it_pending_again() {
    let mut cpu = new_cpu();
    cpu.mem.plic.set_level(UART_IRQ, true);
    wr(&mut cpu, priority(UART_IRQ), 1);
    wr(&mut cpu, enable(CONTEXT_MACHINE), 1 << UART_IRQ);
    rd(&mut cpu, claim(CONTEXT_MACHINE));

    wr(&mut cpu, claim(CONTEXT_MACHINE), UART_IRQ as u64);
    assert!(
        cpu.mem.plic.is_pending(CONTEXT_MACHINE),
        "a level-triggered device that was not quietened fires again"
    );

    // Whereas a source whose line has dropped stays quiet.
    rd(&mut cpu, claim(CONTEXT_MACHINE));
    cpu.mem.plic.set_level(UART_IRQ, false);
    wr(&mut cpu, claim(CONTEXT_MACHINE), UART_IRQ as u64);
    assert!(!cpu.mem.plic.is_pending(CONTEXT_MACHINE));
}

#[test]
fn claiming_with_nothing_pending_returns_zero() {
    // Source 0 does not exist, which is what makes it usable as "none".
    let mut cpu = new_cpu();
    assert_eq!(rd(&mut cpu, claim(CONTEXT_MACHINE)), 0);
}

#[test]
fn the_highest_priority_source_wins_and_ties_go_to_the_lower_number() {
    let mut cpu = new_cpu();
    for source in [3, 5, 7] {
        cpu.mem.plic.set_level(source, true);
    }
    wr(
        &mut cpu,
        enable(CONTEXT_MACHINE),
        (1 << 3) | (1 << 5) | (1 << 7),
    );
    wr(&mut cpu, priority(3), 1);
    wr(&mut cpu, priority(5), 9);
    wr(&mut cpu, priority(7), 4);
    assert_eq!(rd(&mut cpu, claim(CONTEXT_MACHINE)), 5);

    // With 5 claimed, 7 outranks 3.
    assert_eq!(rd(&mut cpu, claim(CONTEXT_MACHINE)), 7);

    // Equal priorities: the lower source number is taken first.
    let mut cpu = new_cpu();
    cpu.mem.plic.set_level(3, true);
    cpu.mem.plic.set_level(7, true);
    wr(&mut cpu, enable(CONTEXT_MACHINE), (1 << 3) | (1 << 7));
    wr(&mut cpu, priority(3), 2);
    wr(&mut cpu, priority(7), 2);
    assert_eq!(rd(&mut cpu, claim(CONTEXT_MACHINE)), 3);
}

#[test]
fn the_pending_register_is_read_only_and_source_zero_cannot_be_enabled() {
    let mut cpu = new_cpu();
    wr(&mut cpu, PENDING, !0);
    assert_eq!(
        rd(&mut cpu, PENDING),
        0,
        "software cannot fake an interrupt"
    );

    wr(&mut cpu, enable(CONTEXT_MACHINE), !0);
    assert_eq!(rd(&mut cpu, enable(CONTEXT_MACHINE)) & 1, 0);
}

// ------------------------------------------------------------------------ UART

#[test]
fn bytes_written_to_the_transmit_register_come_out_as_text() {
    let mut cpu = new_cpu();
    for b in b"hi!" {
        cpu.mem.write(UART_BASE, 1, *b as u64).unwrap();
    }
    assert_eq!(cpu.mem.uart.output(), "hi!");
}

#[test]
fn input_is_readable_once_and_reported_by_line_status() {
    let mut cpu = new_cpu();
    const LSR: u64 = UART_BASE + 5;
    const DATA_READY: u64 = 1;

    assert_eq!(rd(&mut cpu, LSR) & DATA_READY, 0);
    cpu.mem.uart.push_input(b"ab");
    assert_ne!(rd(&mut cpu, LSR) & DATA_READY, 0);

    assert_eq!(cpu.mem.read(UART_BASE, 1).unwrap(), b'a' as u64);
    assert_eq!(cpu.mem.read(UART_BASE, 1).unwrap(), b'b' as u64);
    assert_eq!(rd(&mut cpu, LSR) & DATA_READY, 0, "the queue is drained");
}

#[test]
fn the_transmitter_always_reports_itself_empty() {
    // A driver spins on this bit before writing; if it never sets, the guest
    // hangs before printing anything at all.
    let mut cpu = new_cpu();
    const LSR: u64 = UART_BASE + 5;
    assert_ne!(rd(&mut cpu, LSR) & (1 << 5), 0);
    cpu.mem.write(UART_BASE, 1, b'x' as u64).unwrap();
    assert_ne!(rd(&mut cpu, LSR) & (1 << 5), 0);
}

#[test]
fn dlab_swaps_the_first_two_registers_for_the_baud_divisor() {
    // Nothing here cares about baud rate, but a driver probes by writing the
    // divisor and reading it back, and gives up if it does not stick.
    let mut cpu = new_cpu();
    const LCR: u64 = UART_BASE + 3;

    cpu.mem.write(UART_BASE + 1, 1, 0x05).unwrap(); // IER while DLAB is clear
    cpu.mem.write(LCR, 1, 0x80).unwrap(); // set DLAB
    cpu.mem.write(UART_BASE, 1, 0x01).unwrap(); // divisor low
    cpu.mem.write(UART_BASE + 1, 1, 0x00).unwrap(); // divisor high
    assert_eq!(cpu.mem.read(UART_BASE, 1).unwrap(), 0x01);

    cpu.mem.write(LCR, 1, 0x03).unwrap(); // clear DLAB, 8 bits no parity
    assert_eq!(
        cpu.mem.read(UART_BASE + 1, 1).unwrap(),
        0x05,
        "the interrupt enable was not clobbered by the divisor"
    );
}

#[test]
fn the_uart_interrupts_only_when_the_matching_enable_is_set() {
    let mut cpu = new_cpu();
    const IER: u64 = UART_BASE + 1;

    cpu.mem.uart.push_input(b"x");
    assert!(!cpu.mem.uart.is_interrupting(), "no enable bits set");

    cpu.mem.write(IER, 1, 0x01).unwrap(); // receive interrupt
    assert!(cpu.mem.uart.is_interrupting());

    cpu.mem.read(UART_BASE, 1).unwrap(); // consume the byte
    assert!(!cpu.mem.uart.is_interrupting());
}

#[test]
fn the_identification_register_reports_the_highest_priority_cause() {
    let mut cpu = new_cpu();
    const IER: u64 = UART_BASE + 1;
    const IIR: u64 = UART_BASE + 2;

    // Bit 0 set means "nothing pending" -- the sense is inverted.
    assert_eq!(rd(&mut cpu, IIR) & 0x0f, 0x01);

    cpu.mem.write(IER, 1, 0x02).unwrap(); // transmit-empty interrupt
    assert_eq!(rd(&mut cpu, IIR) & 0x0f, 0x02);

    cpu.mem.write(IER, 1, 0x03).unwrap();
    cpu.mem.uart.push_input(b"x");
    // Received data outranks transmitter-empty.
    assert_eq!(rd(&mut cpu, IIR) & 0x0f, 0x04);
}

// ------------------------------------------------------------------- end to end

#[test]
fn a_uart_byte_drives_an_external_interrupt_through_the_plic() {
    // The whole path: a byte arrives, the UART raises its line, the PLIC
    // routes it to a context, mip.MEIP sets, and the hart takes the trap.
    let mut cpu = new_cpu();
    cpu.mem.load(&image(&[addi(0, 0, 0); 8]));
    // The handler needs real instructions too, or stepping into it hits a
    // zeroed page and traps again.
    cpu.mem
        .load_at(DRAM_BASE + 0x400, &image(&[addi(0, 0, 0); 4]))
        .unwrap();
    cpu.csrs.write(csr::MTVEC, DRAM_BASE + 0x400);
    cpu.csrs.write(csr::MSTATUS, mstatus::MIE);
    cpu.csrs.write(csr::MIE, int::MEIP);

    wr(&mut cpu, priority(UART_IRQ), 1);
    wr(&mut cpu, enable(CONTEXT_MACHINE), 1 << UART_IRQ);
    cpu.mem.write(UART_BASE + 1, 1, 0x01).unwrap(); // enable rx interrupt

    // Nothing has arrived, so the hart just runs.
    cpu.step().unwrap();
    assert_eq!(cpu.csrs.read(csr::MIP) & int::MEIP, 0);
    assert_eq!(cpu.pc, DRAM_BASE + 4);

    cpu.mem.uart.push_input(b"k");
    cpu.step().unwrap();
    assert_ne!(cpu.csrs.read(csr::MIP) & int::MEIP, 0, "line reached mip");
    assert_eq!(cpu.pc, DRAM_BASE + 0x400, "the hart entered the handler");
    // Interrupt causes have the top bit set; machine external is 11.
    assert_eq!(cpu.csrs.read(csr::MCAUSE), (1 << 63) | 11);

    // The handler claims, reads the byte, and completes.
    assert_eq!(rd(&mut cpu, claim(CONTEXT_MACHINE)), UART_IRQ as u64);
    assert_eq!(cpu.mem.read(UART_BASE, 1).unwrap(), b'k' as u64);
    wr(&mut cpu, claim(CONTEXT_MACHINE), UART_IRQ as u64);

    // With the byte consumed the line has dropped, so the hart runs on.
    cpu.step().unwrap();
    assert_eq!(cpu.csrs.read(csr::MIP) & int::MEIP, 0);
}

#[test]
fn a_delegated_external_interrupt_lands_in_supervisor_mode() {
    let mut cpu = new_cpu();
    cpu.mem.load(&image(&[addi(0, 0, 0); 8]));
    cpu.priv_mode = Priv::Supervisor;
    cpu.csrs.write(csr::STVEC, DRAM_BASE + 0x800);
    cpu.csrs.write(csr::MSTATUS, mstatus::SIE);
    cpu.csrs.write(csr::MIE, int::SEIP);
    cpu.csrs.write(csr::MIDELEG, int::SEIP);

    wr(&mut cpu, priority(UART_IRQ), 1);
    wr(&mut cpu, enable(CONTEXT_SUPERVISOR), 1 << UART_IRQ);
    cpu.mem.write(UART_BASE + 1, 1, 0x01).unwrap();
    cpu.mem.uart.push_input(b"s");

    cpu.step().unwrap();
    assert_eq!(cpu.priv_mode, Priv::Supervisor);
    assert_eq!(cpu.pc, DRAM_BASE + 0x800);
    assert_eq!(cpu.csrs.read(csr::SCAUSE), (1 << 63) | 9);
}

#[test]
fn an_unmapped_device_address_still_faults() {
    // The device holes must not turn the whole low address space into
    // readable memory.
    let mut cpu = new_cpu();
    cpu.mem.load(&image(&[i(0x03, 0x2, 10, 0, 0x100)]));
    assert!(matches!(cpu.step(), Err(Exception::LoadAccessFault(0x100))));
}
