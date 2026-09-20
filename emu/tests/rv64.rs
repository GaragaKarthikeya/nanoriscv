//! RV64-specific behaviour, and the seams of the dual-width implementation.
//!
//! The official suite already covers the ISA. What it does not isolate is this
//! model's own design decision -- one ALU parameterised by width, with
//! registers held in a canonical XLEN-wide form -- so these tests aim at the
//! places that choice could go wrong.

mod common;
use common::*;

use nanoemu::cpu::{Cpu, Xlen};
use nanoemu::trap::Exception;

const MEM: usize = 1 << 20;

fn run_xlen(xlen: Xlen, prog: &[u32]) -> Cpu {
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
    run_xlen(Xlen::Rv64, prog)
}

/// 1 << 31, built without relying on LUI's own sign extension.
fn bit31(rd: u32) -> [u32; 2] {
    [addi(rd, 0, 1), i(0x13, 0x1, rd, rd, 31)]
}

/// A usable DRAM address in `rd`. LUI cannot be used for this on RV64: every
/// address at or above DRAM_BASE has bit 31 set, so LUI would sign-extend it
/// into the top of the address space instead.
fn dram_addr(rd: u32, offset: i32) -> [u32; 3] {
    let [a, b] = bit31(rd);
    [a, b, addi(rd, rd, offset)]
}

#[test]
fn w_instructions_sign_extend_into_the_upper_half() {
    // ADDIW of a value whose bit 31 is set must fill the top 32 bits with ones.
    let [a, b] = bit31(5);
    let cpu = run(&[a, b, i(0x1b, 0x0, 10, 5, 0), ECALL]);
    assert_eq!(cpu.regs[10], 0xffff_ffff_8000_0000);
}

#[test]
fn addw_discards_carry_out_of_bit_31() {
    // 0x8000_0000 + 0x8000_0000 is 2^32: zero in 32 bits, and the carry must
    // not survive into the upper half.
    let [a, b] = bit31(5);
    let cpu = run(&[a, b, r(0x3b, 0x0, 0x00, 10, 5, 5), ECALL]);
    assert_eq!(cpu.regs[10], 0);
}

#[test]
fn shift_amounts_are_six_bits_on_rv64_and_illegal_past_five_on_rv32() {
    // An immediate shift by 33 is a 6-bit shift amount on RV64. On RV32 that
    // bit belongs to funct7, so the same encoding is not a shift by 1 -- it
    // is not a legal instruction at all.
    let prog = [addi(5, 0, 1), i(0x13, 0x1, 10, 5, 33), ECALL];
    assert_eq!(run_xlen(Xlen::Rv64, &prog).regs[10], 1u64 << 33);

    let mut cpu = Cpu::rv32(MEM);
    cpu.mem.load(&image(&prog));
    cpu.step().expect("the addi is fine");
    assert!(matches!(cpu.step(), Err(Exception::IllegalInstruction(_))));
}

#[test]
fn slliw_uses_five_bits_even_on_rv64() {
    // SLLIW is a 32-bit operation, so its shift amount stays 5 bits wide.
    let cpu = run(&[addi(5, 0, 1), i(0x1b, 0x1, 10, 5, 31), ECALL]);
    assert_eq!(cpu.regs[10], 0xffff_ffff_8000_0000);
}

#[test]
fn sraw_shifts_the_low_word_arithmetically() {
    // The upper half of the source must be ignored, and the result
    // sign-extended from bit 31 of the shifted word.
    let [a, b] = bit31(5);
    let cpu = run(&[a, b, r(0x3b, 0x5, 0x20, 10, 5, 0), ECALL]);
    assert_eq!(cpu.regs[10], 0xffff_ffff_8000_0000);
}

#[test]
fn srlw_does_not_see_the_upper_half() {
    // x5 = 0xffff_ffff_8000_0000; SRLW must shift only the low word, giving
    // 0x4000_0000 rather than anything derived from the top bits.
    let [a, b] = bit31(5);
    let prog = [
        a,
        b,
        i(0x1b, 0x0, 5, 5, 0),
        addi(6, 0, 1),
        r(0x3b, 0x5, 0x00, 10, 5, 6),
        ECALL,
    ];
    assert_eq!(run(&prog).regs[10], 0x4000_0000);
}

#[test]
fn ld_and_sd_roundtrip_a_full_doubleword() {
    let [a, b, c] = dram_addr(5, 256);
    let prog = [
        a,
        b,
        c,
        addi(6, 0, -1),
        s(0x23, 0x3, 5, 6, 0),  // sd
        i(0x03, 0x3, 10, 5, 0), // ld
        ECALL,
    ];
    assert_eq!(run(&prog).regs[10], u64::MAX);
}

#[test]
fn lw_sign_extends_where_lwu_zero_extends() {
    let [a, b] = bit31(6);
    let [c, d, e] = dram_addr(5, 256);
    let prog = [
        c,
        d,
        e,
        a,
        b,
        s(0x23, 0x2, 5, 6, 0),  // sw: stores 0x8000_0000
        i(0x03, 0x2, 10, 5, 0), // lw  -> sign-extended
        i(0x03, 0x6, 11, 5, 0), // lwu -> zero-extended
        ECALL,
    ];
    let cpu = run(&prog);
    assert_eq!(cpu.regs[10], 0xffff_ffff_8000_0000);
    assert_eq!(cpu.regs[11], 0x0000_0000_8000_0000);
}

#[test]
fn mulw_truncates_before_sign_extending() {
    // 0x10000 * 0x10000 is 2^32: the low word is zero, so MULW yields 0 even
    // though the full 64-bit product does not.
    let prog = [
        addi(5, 0, 1),
        i(0x13, 0x1, 5, 5, 16), // x5 = 0x10000
        r(0x3b, 0x0, 0x01, 10, 5, 5),
        mul(11, 5, 5), // the full-width product, for contrast
        ECALL,
    ];
    let cpu = run(&prog);
    assert_eq!(cpu.regs[10], 0);
    assert_eq!(cpu.regs[11], 1u64 << 32);
}

#[test]
fn divw_operates_on_the_low_word() {
    // The dividend's upper half must not participate: -8 / 2 in 32-bit terms.
    let prog = [
        addi(5, 0, -8),
        addi(6, 0, 2),
        r(0x3b, 0x4, 0x01, 10, 5, 6),
        ECALL,
    ];
    assert_eq!(run(&prog).regs[10], (-4i64) as u64);
}

#[test]
fn mulhsu_multiplies_signed_by_unsigned() {
    // -1 as signed times 2 as unsigned is -2, whose high half is all ones.
    // Reading the unsigned operand from rs1 instead of rs2 would give 0 here.
    let prog = [
        addi(5, 0, -1),
        addi(6, 0, 2),
        r(0x33, 0x2, 0x01, 10, 5, 6),
        ECALL,
    ];
    assert_eq!(run(&prog).regs[10], u64::MAX);
}

#[test]
fn rv32_rejects_the_rv64_only_opcodes() {
    // OP-32 does not exist on RV32 and must raise an illegal instruction
    // rather than quietly behaving like its 64-bit counterpart.
    let mut cpu = Cpu::rv32(MEM);
    cpu.mem.load(&image(&[r(0x3b, 0x0, 0x00, 10, 0, 0)]));
    assert!(matches!(cpu.step(), Err(Exception::IllegalInstruction(_))));
}

#[test]
fn rv32_registers_hold_only_thirty_two_bits() {
    // The canonical form on RV32 is zero-extended, so the register file reads
    // back exactly as the 32-bit RTL core's will.
    let cpu = run_xlen(Xlen::Rv32, &[addi(10, 0, -1), ECALL]);
    assert_eq!(cpu.regs[10], 0x0000_0000_ffff_ffff);
}
