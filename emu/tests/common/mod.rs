//! A minimal assembler used by the test suite, so the tests read as RISC-V
//! rather than as hex constants.

#![allow(dead_code)]

pub fn r(op: u32, f3: u32, f7: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    (f7 << 25) | (rs2 << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | op
}

pub fn i(op: u32, f3: u32, rd: u32, rs1: u32, imm: i32) -> u32 {
    ((imm as u32 & 0xfff) << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | op
}

pub fn s(op: u32, f3: u32, rs1: u32, rs2: u32, imm: i32) -> u32 {
    let imm = imm as u32;
    ((imm >> 5 & 0x7f) << 25) | (rs2 << 20) | (rs1 << 15) | (f3 << 12) | ((imm & 0x1f) << 7) | op
}

pub fn b(f3: u32, rs1: u32, rs2: u32, imm: i32) -> u32 {
    let imm = imm as u32;
    ((imm >> 12 & 1) << 31)
        | ((imm >> 5 & 0x3f) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (f3 << 12)
        | ((imm >> 1 & 0xf) << 8)
        | ((imm >> 11 & 1) << 7)
        | 0x63
}

pub fn u(op: u32, rd: u32, imm: u32) -> u32 {
    (imm & 0xffff_f000) | (rd << 7) | op
}

pub fn jal(rd: u32, imm: i32) -> u32 {
    let imm = imm as u32;
    ((imm >> 20 & 1) << 31)
        | ((imm >> 1 & 0x3ff) << 21)
        | ((imm >> 11 & 1) << 20)
        | ((imm >> 12 & 0xff) << 12)
        | (rd << 7)
        | 0x6f
}

pub fn addi(rd: u32, rs1: u32, imm: i32) -> u32 {
    i(0x13, 0x0, rd, rs1, imm)
}
pub fn add(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r(0x33, 0x0, 0x00, rd, rs1, rs2)
}
pub fn sub(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r(0x33, 0x0, 0x20, rd, rs1, rs2)
}
pub fn lui(rd: u32, imm: u32) -> u32 {
    u(0x37, rd, imm)
}
pub fn auipc(rd: u32, imm: u32) -> u32 {
    u(0x17, rd, imm)
}
pub fn jalr(rd: u32, rs1: u32, imm: i32) -> u32 {
    i(0x67, 0x0, rd, rs1, imm)
}
pub fn sw(rs1: u32, rs2: u32, imm: i32) -> u32 {
    s(0x23, 0x2, rs1, rs2, imm)
}
pub fn lw(rd: u32, rs1: u32, imm: i32) -> u32 {
    i(0x03, 0x2, rd, rs1, imm)
}
pub fn mul(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r(0x33, 0x0, 0x01, rd, rs1, rs2)
}
pub fn div(rd: u32, rs1: u32, rs2: u32) -> u32 {
    r(0x33, 0x4, 0x01, rd, rs1, rs2)
}
pub const ECALL: u32 = 0x0000_0073;

/// Packs a program into the little-endian byte image the loader expects.
pub fn image(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}
