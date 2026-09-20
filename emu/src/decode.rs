//! Instruction field extraction. The bit positions are fixed by the base ISA
//! encoding, which is why every immediate variant can be pulled out of a raw
//! word without first knowing the opcode.

#[inline]
pub fn opcode(i: u32) -> u32 {
    i & 0x7f
}
#[inline]
pub fn rd(i: u32) -> usize {
    ((i >> 7) & 0x1f) as usize
}
#[inline]
pub fn rs1(i: u32) -> usize {
    ((i >> 15) & 0x1f) as usize
}
#[inline]
pub fn rs2(i: u32) -> usize {
    ((i >> 20) & 0x1f) as usize
}
#[inline]
pub fn funct3(i: u32) -> u32 {
    (i >> 12) & 0x7
}
#[inline]
pub fn funct7(i: u32) -> u32 {
    i >> 25
}
#[inline]
pub fn csr(i: u32) -> u16 {
    (i >> 20) as u16 & 0xfff
}

/// I-type: sign-extended [31:20].
#[inline]
pub fn imm_i(i: u32) -> i32 {
    (i as i32) >> 20
}

/// S-type: sign-extended {[31:25], [11:7]}.
#[inline]
pub fn imm_s(i: u32) -> i32 {
    (((i as i32) >> 25) << 5) | ((i >> 7) & 0x1f) as i32
}

/// B-type: sign-extended {[31], [7], [30:25], [11:8], 0}.
#[inline]
pub fn imm_b(i: u32) -> i32 {
    (((i as i32) >> 31) << 12)
        | (((i >> 7) & 1) << 11) as i32
        | (((i >> 25) & 0x3f) << 5) as i32
        | (((i >> 8) & 0xf) << 1) as i32
}

/// U-type: [31:12] placed in the high bits.
#[inline]
pub fn imm_u(i: u32) -> i32 {
    (i & 0xffff_f000) as i32
}

/// J-type: sign-extended {[31], [19:12], [20], [30:21], 0}.
#[inline]
pub fn imm_j(i: u32) -> i32 {
    (((i as i32) >> 31) << 20)
        | ((i & 0x000f_f000) as i32)
        | (((i >> 20) & 1) << 11) as i32
        | (((i >> 21) & 0x3ff) << 1) as i32
}

pub const REG_NAMES: [&str; 32] = [
    "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3", "a4",
    "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4",
    "t5", "t6",
];
