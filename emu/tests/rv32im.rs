//! Behavioural tests for the RV32IM hart.
//!
//! Each test assembles a short program, runs it to the terminating ECALL, and
//! checks architectural state. Cases are chosen for the corners the spec calls
//! out explicitly, since those are where a hand-written core goes wrong.

mod common;
use common::*;

use nanoemu::cpu::Cpu;
use nanoemu::trap::Exception;
use nanoemu::DRAM_BASE;

const MEM: usize = 1 << 20;

/// Runs a program to its ECALL and returns the final hart state.
fn run(prog: &[u32]) -> Cpu {
    let mut cpu = Cpu::rv32(MEM);
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

#[test]
fn addi_and_add() {
    let cpu = run(&[addi(5, 0, 7), addi(6, 0, 35), add(10, 5, 6), ECALL]);
    assert_eq!(cpu.regs[10], 42);
}

#[test]
fn x0_is_hardwired_to_zero() {
    let cpu = run(&[addi(0, 0, 999), add(10, 0, 0), ECALL]);
    assert_eq!(cpu.regs[0], 0);
    assert_eq!(cpu.regs[10], 0);
}

#[test]
fn addi_sign_extends_the_immediate() {
    // -1 is 0xfff in the 12-bit field; it must become 0xffff_ffff.
    let cpu = run(&[addi(10, 0, -1), ECALL]);
    assert_eq!(cpu.regs[10], 0xffff_ffff);
}

#[test]
fn sub_wraps_without_trapping() {
    let cpu = run(&[addi(5, 0, 1), sub(10, 0, 5), ECALL]);
    assert_eq!(cpu.regs[10], 0xffff_ffff);
}

#[test]
fn lui_and_auipc() {
    // AUIPC adds to the address of the AUIPC itself, not the next instruction.
    let cpu = run(&[lui(10, 0xdead_0000), auipc(11, 0x0000_1000), ECALL]);
    assert_eq!(cpu.regs[10], 0xdead_0000);
    assert_eq!(cpu.regs[11], DRAM_BASE + 4 + 0x1000);
}

#[test]
fn branch_taken_and_not_taken() {
    let prog = [
        addi(5, 0, 3),
        addi(6, 0, 3),
        b(0x0, 5, 6, 8), // beq -> skip the next instruction
        addi(10, 0, 1),  // must not execute
        addi(10, 10, 5),
        ECALL,
    ];
    assert_eq!(run(&prog).regs[10], 5);
}

#[test]
fn signed_and_unsigned_compares_differ() {
    // -1 is less than 1 signed, but greater unsigned. BLT and BLTU must
    // disagree here; if both use the same comparison one of them is wrong.
    let prog = [
        addi(5, 0, -1),
        addi(6, 0, 1),
        addi(10, 0, 0),
        b(0x4, 5, 6, 8), // blt  x5, x6 -> taken
        addi(10, 10, 1), // skipped
        b(0x6, 5, 6, 8), // bltu x5, x6 -> not taken
        addi(10, 10, 2), // executed
        ECALL,
    ];
    assert_eq!(run(&prog).regs[10], 2);
}

#[test]
fn jal_links_and_jumps_backwards() {
    let prog = [
        addi(10, 0, 0),
        jal(1, 8), // jump over the trap-marker below
        addi(10, 0, 99),
        addi(10, 0, 1),
        ECALL,
    ];
    let cpu = run(&prog);
    assert_eq!(cpu.regs[10], 1);
    assert_eq!(cpu.regs[1], DRAM_BASE + 8); // return address
}

#[test]
fn jalr_clears_the_low_bit_of_the_target() {
    let target = DRAM_BASE as u32 + 12;
    let prog = [
        lui(5, target),
        addi(5, 5, (target & 0xfff) as i32 + 1), // deliberately odd
        jalr(0, 5, 0),
        addi(10, 0, 7), // the aligned target
        ECALL,
    ];
    assert_eq!(run(&prog).regs[10], 7);
}

#[test]
fn store_then_load_roundtrips() {
    let prog = [
        lui(5, 0x8001_0000u32),
        addi(6, 0, 0x2a),
        sw(5, 6, 16),
        lw(10, 5, 16),
        ECALL,
    ];
    assert_eq!(run(&prog).regs[10], 0x2a);
}

#[test]
fn narrow_loads_sign_extend_only_when_signed() {
    let prog = [
        lui(5, 0x8001_0000u32),
        addi(6, 0, -1),
        common::s(0x23, 0x0, 5, 6, 0),  // sb: writes 0xff
        common::i(0x03, 0x0, 10, 5, 0), // lb  -> 0xffff_ffff
        common::i(0x03, 0x4, 11, 5, 0), // lbu -> 0x0000_00ff
        ECALL,
    ];
    let cpu = run(&prog);
    assert_eq!(cpu.regs[10], 0xffff_ffff);
    assert_eq!(cpu.regs[11], 0x0000_00ff);
}

#[test]
fn shift_right_arithmetic_versus_logical() {
    let prog = [
        addi(5, 0, -8),
        common::i(0x13, 0x5, 10, 5, 1),         // srli x10, x5, 1
        common::i(0x13, 0x5, 11, 5, 1 | 0x400), // srai x11, x5, 1
        ECALL,
    ];
    let cpu = run(&prog);
    assert_eq!(cpu.regs[10], 0x7fff_fffc);
    assert_eq!(cpu.regs[11], (-4i32) as u32 as u64);
}

#[test]
fn shifts_use_only_the_low_five_bits() {
    // A shift by 33 must behave as a shift by 1, not produce zero or panic.
    let prog = [
        addi(5, 0, 1),
        addi(6, 0, 33),
        common::r(0x33, 0x1, 0x00, 10, 5, 6),
        ECALL,
    ];
    assert_eq!(run(&prog).regs[10], 2);
}

#[test]
fn mul_and_mulh_split_the_full_product() {
    let prog = [
        lui(5, 0x0001_0000), // 0x10000
        lui(6, 0x0001_0000),
        mul(10, 5, 6),                        // low  -> 0
        common::r(0x33, 0x1, 0x01, 11, 5, 6), // mulh -> 1
        ECALL,
    ];
    let cpu = run(&prog);
    assert_eq!(cpu.regs[10], 0);
    assert_eq!(cpu.regs[11], 1);
}

#[test]
fn division_by_zero_is_defined_not_a_trap() {
    // Vol I §4.5: DIV by zero yields all ones, REM by zero yields the dividend.
    let prog = [
        addi(5, 0, 17),
        div(10, 5, 0),
        common::r(0x33, 0x6, 0x01, 11, 5, 0), // rem
        ECALL,
    ];
    let cpu = run(&prog);
    assert_eq!(cpu.regs[10], 0xffff_ffff);
    assert_eq!(cpu.regs[11], 17);
}

#[test]
fn signed_division_overflow_is_defined() {
    // i32::MIN / -1 overflows; the spec says the result is i32::MIN and the
    // remainder is 0, rather than a trap.
    let prog = [
        lui(5, 0x8000_0000u32), // i32::MIN
        addi(6, 0, -1),
        div(10, 5, 6),
        common::r(0x33, 0x6, 0x01, 11, 5, 6), // rem
        ECALL,
    ];
    let cpu = run(&prog);
    assert_eq!(cpu.regs[10], 0x8000_0000);
    assert_eq!(cpu.regs[11], 0);
}

#[test]
fn csrrw_returns_the_old_value_then_writes() {
    let prog = [
        addi(5, 0, 0x123),
        common::i(0x73, 0x1, 0, 5, 0x340), // csrrw x0, mscratch, x5
        addi(6, 0, 0x456),
        common::i(0x73, 0x1, 10, 6, 0x340), // csrrw x10, mscratch, x6
        common::i(0x73, 0x2, 11, 0, 0x340), // csrrs x11, mscratch, x0 (read)
        ECALL,
    ];
    let cpu = run(&prog);
    assert_eq!(cpu.regs[10], 0x123);
    assert_eq!(cpu.regs[11], 0x456);
}

#[test]
fn csrrs_with_rs1_zero_does_not_write() {
    // Reading a read-only-in-practice CSR through csrrs x0 must leave it alone.
    let prog = [
        addi(5, 0, 0x7f),
        common::i(0x73, 0x1, 0, 5, 0x340),  // mscratch = 0x7f
        common::i(0x73, 0x2, 10, 0, 0x340), // csrrs x10, mscratch, x0
        common::i(0x73, 0x2, 11, 0, 0x340),
        ECALL,
    ];
    let cpu = run(&prog);
    assert_eq!(cpu.regs[10], 0x7f);
    assert_eq!(cpu.regs[11], 0x7f);
}

#[test]
fn an_illegal_instruction_traps_to_mtvec() {
    let handler = DRAM_BASE as u32 + 0x100;
    let mut cpu = Cpu::rv32(MEM);
    cpu.mem.load(&image(&[
        lui(5, handler),
        addi(5, 5, (handler & 0xfff) as i32),
        common::i(0x73, 0x1, 0, 5, 0x305), // csrrw x0, mtvec, x5
        0xffff_ffff,                       // not an instruction
    ]));
    for _ in 0..3 {
        cpu.step().expect("setup should not trap");
    }
    assert!(matches!(cpu.step(), Err(Exception::IllegalInstruction(_))));
    assert_eq!(cpu.pc, handler as u64);
    assert_eq!(cpu.csrs.read(nanoemu::csr::MCAUSE), 2);
    assert_eq!(cpu.csrs.read(nanoemu::csr::MEPC), DRAM_BASE + 12);
}

#[test]
fn a_load_outside_dram_faults() {
    let mut cpu = Cpu::rv32(MEM);
    // lw x10, 0(x0) -> address 0, well below DRAM_BASE
    cpu.mem.load(&image(&[lw(10, 0, 0)]));
    assert!(matches!(cpu.step(), Err(Exception::LoadAccessFault(0))));
}

#[test]
fn instret_counts_only_retired_instructions() {
    let cpu = run(&[addi(5, 0, 1), addi(6, 0, 2), ECALL]);
    // The ECALL traps rather than retiring, so it is not counted.
    assert_eq!(cpu.csrs.read(nanoemu::csr::INSTRET), 2);
}
