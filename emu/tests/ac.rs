//! The A and C extensions.
//!
//! The official suite covers the instructions. These tests aim at the places
//! this implementation could be wrong in ways the suite would not pin down:
//! the expansion table for C, and the reservation and old-value semantics for A.

mod common;
use common::*;

use nanoemu::compress::decompress;
use nanoemu::cpu::{Cpu, Xlen};
use nanoemu::trap::Exception;

const MEM: usize = 1 << 20;
const DRAM: u64 = nanoemu::DRAM_BASE;

/// Builds an instruction stream that mixes 16- and 32-bit encodings.
enum Ins {
    C(u16),
    W(u32),
}
use Ins::{C, W};

fn stream(items: &[Ins]) -> Vec<u8> {
    let mut v = Vec::new();
    for it in items {
        match it {
            C(h) => v.extend_from_slice(&h.to_le_bytes()),
            W(w) => v.extend_from_slice(&w.to_le_bytes()),
        }
    }
    v
}

fn run_xlen(xlen: Xlen, items: &[Ins]) -> Cpu {
    let mut cpu = Cpu::new(MEM, xlen);
    cpu.mem.load(&stream(items));
    for _ in 0..10_000 {
        match cpu.step() {
            Ok(()) => {}
            Err(Exception::EnvironmentCall) => return cpu,
            Err(e) => panic!("unexpected trap: {e:?}"),
        }
    }
    panic!("program did not terminate");
}

fn run(items: &[Ins]) -> Cpu {
    run_xlen(Xlen::Rv64, items)
}

/// An AMO instruction. `f5` selects the operation; the aq/rl bits are left
/// clear because ordering is meaningless on one in-order hart.
fn amo(f5: u32, f3: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    (f5 << 27) | (rs2 << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | 0x2f
}
const LR: u32 = 0x02;
const SC: u32 = 0x03;
const AMOADD: u32 = 0x00;
const AMOSWAP: u32 = 0x01;
const AMOMIN: u32 = 0x10;
const AMOMINU: u32 = 0x18;
const W64: u32 = 0x3; // .D
const W32: u32 = 0x2; // .W

/// Puts a usable DRAM address in `rd`; LUI cannot be used on RV64 because it
/// would sign-extend every address at or above DRAM_BASE.
fn dram_addr(rd: u32, offset: i32) -> [u32; 3] {
    [
        addi(rd, 0, 1),
        i(0x13, 0x1, rd, rd, 31),
        addi(rd, rd, offset),
    ]
}

// ---------------------------------------------------------------- C extension

#[test]
fn expansions_match_their_base_instructions() {
    // Each RVC instruction is defined as an alias for exactly one base
    // instruction, so the expansion must be bit-identical to it.
    let cases: &[(u16, u32, &str)] = &[
        (0x0001, addi(0, 0, 0), "c.nop"),
        (0x0505, addi(10, 10, 1), "c.addi x10, 1"),
        (0x4515, addi(10, 0, 5), "c.li x10, 5"),
        (0x85aa, r(0x33, 0x0, 0x00, 11, 0, 10), "c.mv x11, x10"),
        (0x95aa, r(0x33, 0x0, 0x00, 11, 11, 10), "c.add x11, x10"),
        (0x8082, i(0x67, 0x0, 0, 1, 0), "c.jr ra"),
        (0x9002, 0x0010_0073, "c.ebreak"),
    ];
    for &(compressed, expected, name) in cases {
        assert_eq!(
            decompress(compressed as u32, Xlen::Rv64),
            Some(expected),
            "{name} expanded wrongly"
        );
    }
}

#[test]
fn c_srai_selects_funct6_not_funct7() {
    // SRAI is selected by funct6 = 0b010000 sitting in imm[11:6], which is
    // 0x400. Encoding it as funct7 shifted by 6 puts the bit one place too
    // high and yields an illegal instruction instead.
    let compressed = 0x8431; // c.srai x8, 12
    assert_eq!(
        decompress(compressed, Xlen::Rv64),
        Some(i(0x13, 0x5, 8, 8, 0x400 | 12))
    );
}

#[test]
fn an_all_zero_halfword_is_illegal() {
    // Required by the spec so that execution running into a zeroed page traps
    // rather than wandering.
    assert_eq!(decompress(0x0000, Xlen::Rv64), None);
    let mut cpu = Cpu::rv64(MEM);
    cpu.mem.load(&stream(&[C(0x0000)]));
    assert!(matches!(cpu.step(), Err(Exception::IllegalInstruction(0))));
}

#[test]
fn the_same_halfword_means_different_things_at_each_xlen() {
    // 0x2505 is C.ADDIW on RV64 and C.JAL on RV32 -- the encoding space is
    // reused, so a decoder that ignores XLEN silently does the wrong thing.
    assert_eq!(
        decompress(0x2505, Xlen::Rv64),
        Some(i(0x1b, 0x0, 10, 10, 1))
    );
    let rv32 = decompress(0x2505, Xlen::Rv32).expect("c.jal is legal on rv32");
    assert_eq!(rv32 & 0x7f, 0x6f, "should expand to JAL");
    assert_eq!((rv32 >> 7) & 0x1f, 1, "JAL must link to ra");
}

#[test]
fn pc_advances_by_two_across_compressed_instructions() {
    let mut cpu = Cpu::rv64(MEM);
    cpu.mem.load(&stream(&[C(0x4515), C(0x0505), W(ECALL)]));
    cpu.step().unwrap();
    assert_eq!(cpu.pc, DRAM + 2);
    cpu.step().unwrap();
    assert_eq!(cpu.pc, DRAM + 4);
    assert_eq!(cpu.regs[10], 6); // c.li 5 then c.addi 1
}

#[test]
fn compressed_and_full_width_instructions_interleave() {
    // A 32-bit instruction at a 2-byte-aligned address must fetch correctly:
    // the two halves are read separately and reassembled.
    let cpu = run(&[C(0x4515), W(addi(11, 10, 3)), C(0x0505), W(ECALL)]);
    assert_eq!(cpu.regs[10], 6);
    assert_eq!(cpu.regs[11], 8);
}

#[test]
fn a_two_byte_aligned_jump_target_is_legal() {
    // Without C an instruction had to be 4-byte aligned. With C, requiring 4
    // would reject legal targets, so only bit 0 may fault.
    // c.j +6 clears the 4-byte word that follows, landing on the c.li at
    // DRAM+6 -- an address that is 2-byte but not 4-byte aligned.
    assert_eq!(decompress(0xa019, Xlen::Rv64).map(|w| w & 0x7f), Some(0x6f));
    let cpu = run(&[C(0xa019), W(0xffff_ffff), C(0x4515), W(ECALL)]);
    assert_eq!(cpu.regs[10], 5);
}

#[test]
fn an_odd_pc_faults() {
    let mut cpu = Cpu::rv64(MEM);
    cpu.mem.load(&stream(&[C(0x0001)]));
    cpu.pc = DRAM + 1;
    assert!(matches!(
        cpu.step(),
        Err(Exception::InstructionAccessFault(_))
    ));
}

// ---------------------------------------------------------------- A extension

#[test]
fn lr_then_sc_succeeds_and_writes_memory() {
    let [a, b, c] = dram_addr(5, 256);
    let cpu = run(&[
        W(a),
        W(b),
        W(c),
        W(addi(6, 0, 42)),
        W(amo(LR, W64, 7, 5, 0)),
        W(amo(SC, W64, 8, 5, 6)),
        W(i(0x03, 0x3, 9, 5, 0)), // ld: read it back
        W(ECALL),
    ]);
    assert_eq!(cpu.regs[8], 0, "SC returns 0 on success");
    assert_eq!(cpu.regs[9], 42, "the store must have landed");
}

#[test]
fn sc_without_a_reservation_fails_and_leaves_memory_alone() {
    let [a, b, c] = dram_addr(5, 256);
    let cpu = run(&[
        W(a),
        W(b),
        W(c),
        W(addi(6, 0, 42)),
        W(amo(SC, W64, 8, 5, 6)), // no LR first
        W(i(0x03, 0x3, 9, 5, 0)),
        W(ECALL),
    ]);
    assert_eq!(cpu.regs[8], 1, "SC returns 1 on failure");
    assert_eq!(cpu.regs[9], 0, "memory must be untouched");
}

#[test]
fn sc_to_a_different_address_fails() {
    let [a, b, c] = dram_addr(5, 256);
    let cpu = run(&[
        W(a),
        W(b),
        W(c),
        W(addi(7, 5, 8)), // a second, different address
        W(addi(6, 0, 42)),
        W(amo(LR, W64, 0, 5, 0)),
        W(amo(SC, W64, 8, 7, 6)),
        W(ECALL),
    ]);
    assert_eq!(cpu.regs[8], 1);
}

#[test]
fn a_second_sc_fails_because_the_reservation_was_consumed() {
    let [a, b, c] = dram_addr(5, 256);
    let cpu = run(&[
        W(a),
        W(b),
        W(c),
        W(addi(6, 0, 1)),
        W(amo(LR, W64, 0, 5, 0)),
        W(amo(SC, W64, 8, 5, 6)),
        W(amo(SC, W64, 9, 5, 6)),
        W(ECALL),
    ]);
    assert_eq!(cpu.regs[8], 0);
    assert_eq!(cpu.regs[9], 1);
}

#[test]
fn an_amo_returns_the_value_from_before_the_operation() {
    let [a, b, c] = dram_addr(5, 256);
    let cpu = run(&[
        W(a),
        W(b),
        W(c),
        W(addi(6, 0, 10)),
        W(s(0x23, 0x3, 5, 6, 0)), // sd 10
        W(addi(7, 0, 5)),
        W(amo(AMOADD, W64, 8, 5, 7)),
        W(i(0x03, 0x3, 9, 5, 0)),
        W(ECALL),
    ]);
    assert_eq!(cpu.regs[8], 10, "rd is the old value, not the new one");
    assert_eq!(cpu.regs[9], 15);
}

#[test]
fn amoswap_replaces_without_combining() {
    let [a, b, c] = dram_addr(5, 256);
    let cpu = run(&[
        W(a),
        W(b),
        W(c),
        W(addi(6, 0, 10)),
        W(s(0x23, 0x3, 5, 6, 0)),
        W(addi(7, 0, 5)),
        W(amo(AMOSWAP, W64, 8, 5, 7)),
        W(i(0x03, 0x3, 9, 5, 0)),
        W(ECALL),
    ]);
    assert_eq!(cpu.regs[8], 10);
    assert_eq!(cpu.regs[9], 5);
}

#[test]
fn amomin_and_amominu_disagree_on_negative_values() {
    // -1 is the smaller value signed and the larger unsigned, so the two
    // instructions must pick opposite operands.
    let [a, b, c] = dram_addr(5, 256);
    let prog = |op: u32| {
        vec![
            W(a),
            W(b),
            W(c),
            W(addi(6, 0, -1)),
            W(s(0x23, 0x3, 5, 6, 0)), // memory holds -1
            W(addi(7, 0, 1)),
            W(amo(op, W64, 8, 5, 7)),
            W(i(0x03, 0x3, 9, 5, 0)),
            W(ECALL),
        ]
    };
    assert_eq!(run(&prog(AMOMIN)).regs[9], u64::MAX, "signed min keeps -1");
    assert_eq!(run(&prog(AMOMINU)).regs[9], 1, "unsigned min keeps 1");
}

#[test]
fn a_word_width_amo_sign_extends_the_returned_value() {
    // AMOADD.W on RV64 returns the previous 32-bit word sign-extended.
    let [a, b, c] = dram_addr(5, 256);
    let cpu = run(&[
        W(a),
        W(b),
        W(c),
        W(addi(6, 0, 1)),
        W(i(0x13, 0x1, 6, 6, 31)), // x6 = 0x8000_0000
        W(s(0x23, 0x2, 5, 6, 0)),  // sw
        W(amo(AMOADD, W32, 8, 5, 0)),
        W(ECALL),
    ]);
    assert_eq!(cpu.regs[8], 0xffff_ffff_8000_0000);
}

#[test]
fn a_misaligned_amo_faults() {
    // Ordinary loads and stores tolerate misalignment here; atomics do not.
    let [a, b, c] = dram_addr(5, 257);
    let mut cpu = Cpu::rv64(MEM);
    cpu.mem
        .load(&stream(&[W(a), W(b), W(c), W(amo(AMOADD, W64, 8, 5, 0))]));
    for _ in 0..3 {
        cpu.step().expect("address setup should not trap");
    }
    assert!(matches!(
        cpu.step(),
        Err(Exception::StoreAddressMisaligned(_))
    ));
}

#[test]
fn rv32_rejects_doubleword_atomics() {
    let mut cpu = Cpu::rv32(MEM);
    cpu.mem.load(&stream(&[W(amo(AMOADD, W64, 8, 0, 0))]));
    assert!(matches!(cpu.step(), Err(Exception::IllegalInstruction(_))));
}
