//! The C extension, implemented by expanding each 16-bit instruction into the
//! equivalent 32-bit one.
//!
//! Every RVC instruction is defined by the spec as an alias for exactly one
//! base instruction, so expanding at fetch keeps the execute path unaware that
//! C exists. The cost is this module; the benefit is that nothing downstream
//! grows a second case, and the RTL core can use the same trick.
//!
//! The immediate fields are scrambled rather than contiguous. That looks
//! gratuitous on paper but is deliberate: it keeps each immediate bit in a
//! fixed position relative to the 32-bit encodings, so hardware routes wires
//! instead of multiplexing. Vol I, "Compressed Instruction Formats".

use crate::cpu::Xlen;

/// Extracts bits `hi..=lo`, counting from 0.
#[inline]
fn bits(i: u32, hi: u32, lo: u32) -> u32 {
    (i >> lo) & ((1 << (hi - lo + 1)) - 1)
}

/// Sign-extends the low `width` bits of `v`.
#[inline]
fn sext(v: u32, width: u32) -> i32 {
    let shift = 32 - width;
    ((v << shift) as i32) >> shift
}

/// The three-bit register fields in the compact formats name x8..x15, the
/// registers a compiler reaches for most often.
#[inline]
fn rp(i: u32, lo: u32) -> u32 {
    bits(i, lo + 2, lo) + 8
}

fn r_type(op: u32, f3: u32, f7: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    (f7 << 25) | (rs2 << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | op
}
fn i_type(op: u32, f3: u32, rd: u32, rs1: u32, imm: i32) -> u32 {
    ((imm as u32 & 0xfff) << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | op
}
fn s_type(op: u32, f3: u32, rs1: u32, rs2: u32, imm: i32) -> u32 {
    let imm = imm as u32;
    ((imm >> 5 & 0x7f) << 25) | (rs2 << 20) | (rs1 << 15) | (f3 << 12) | ((imm & 0x1f) << 7) | op
}
fn b_type(f3: u32, rs1: u32, rs2: u32, imm: i32) -> u32 {
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
fn u_type(op: u32, rd: u32, imm: i32) -> u32 {
    (imm as u32 & 0xffff_f000) | (rd << 7) | op
}
fn j_type(rd: u32, imm: i32) -> u32 {
    let imm = imm as u32;
    ((imm >> 20 & 1) << 31)
        | ((imm >> 1 & 0x3ff) << 21)
        | ((imm >> 11 & 1) << 20)
        | ((imm >> 12 & 0xff) << 12)
        | (rd << 7)
        | 0x6f
}

const OP_IMM: u32 = 0x13;
const OP_IMM32: u32 = 0x1b;
const OP: u32 = 0x33;
const OP32: u32 = 0x3b;
const LOAD: u32 = 0x03;
const STORE: u32 = 0x23;
const JALR: u32 = 0x67;
const LOAD_FP: u32 = 0x07;
const STORE_FP: u32 = 0x27;

/// Expands one 16-bit instruction, or returns `None` if the encoding is
/// illegal or reserved at this XLEN.
///
/// The floating-point forms expand to LOAD-FP and STORE-FP, whose register
/// fields sit in the same places as the integer ones; whether the hart may
/// execute them is `mstatus.FS`'s business, not the decoder's.
pub fn decompress(i: u32, xlen: Xlen) -> Option<u32> {
    let rv64 = xlen == Xlen::Rv64;
    let rd = bits(i, 11, 7);
    let rs2 = bits(i, 6, 2);
    let funct3 = bits(i, 15, 13);

    match bits(i, 1, 0) {
        // Quadrant 0: loads and stores off a pointer, plus the stack helper.
        0b00 => match funct3 {
            0b000 => {
                // C.ADDI4SPN. An all-zero halfword lands here, and is the
                // canonical illegal instruction, so a zeroed page traps.
                let imm = (bits(i, 10, 7) << 6)
                    | (bits(i, 12, 11) << 4)
                    | (bits(i, 5, 5) << 3)
                    | (bits(i, 6, 6) << 2);
                if imm == 0 {
                    return None;
                }
                Some(i_type(OP_IMM, 0x0, rp(i, 2), 2, imm as i32))
            }
            0b010 => {
                // C.LW
                let imm = (bits(i, 5, 5) << 6) | (bits(i, 12, 10) << 3) | (bits(i, 6, 6) << 2);
                Some(i_type(LOAD, 0x2, rp(i, 2), rp(i, 7), imm as i32))
            }
            0b011 if rv64 => {
                // C.LD
                let imm = (bits(i, 6, 5) << 6) | (bits(i, 12, 10) << 3);
                Some(i_type(LOAD, 0x3, rp(i, 2), rp(i, 7), imm as i32))
            }
            0b001 => {
                // C.FLD. The offset is scaled by eight, exactly as C.LD's is.
                let imm = (bits(i, 6, 5) << 6) | (bits(i, 12, 10) << 3);
                Some(i_type(LOAD_FP, 0x3, rp(i, 2), rp(i, 7), imm as i32))
            }
            0b011 if !rv64 => {
                // C.FLW, which the encoding space gives up on RV64 to C.LD.
                let imm = (bits(i, 5, 5) << 6) | (bits(i, 12, 10) << 3) | (bits(i, 6, 6) << 2);
                Some(i_type(LOAD_FP, 0x2, rp(i, 2), rp(i, 7), imm as i32))
            }
            0b101 => {
                // C.FSD
                let imm = (bits(i, 6, 5) << 6) | (bits(i, 12, 10) << 3);
                Some(s_type(STORE_FP, 0x3, rp(i, 7), rp(i, 2), imm as i32))
            }
            0b111 if !rv64 => {
                // C.FSW
                let imm = (bits(i, 5, 5) << 6) | (bits(i, 12, 10) << 3) | (bits(i, 6, 6) << 2);
                Some(s_type(STORE_FP, 0x2, rp(i, 7), rp(i, 2), imm as i32))
            }
            0b110 => {
                // C.SW
                let imm = (bits(i, 5, 5) << 6) | (bits(i, 12, 10) << 3) | (bits(i, 6, 6) << 2);
                Some(s_type(STORE, 0x2, rp(i, 7), rp(i, 2), imm as i32))
            }
            0b111 if rv64 => {
                // C.SD
                let imm = (bits(i, 6, 5) << 6) | (bits(i, 12, 10) << 3);
                Some(s_type(STORE, 0x3, rp(i, 7), rp(i, 2), imm as i32))
            }
            _ => None,
        },

        // Quadrant 1: immediate arithmetic and control flow.
        0b01 => match funct3 {
            0b000 => {
                // C.NOP when rd is zero, C.ADDI otherwise.
                let imm = sext((bits(i, 12, 12) << 5) | rs2, 6);
                Some(i_type(OP_IMM, 0x0, rd, rd, imm))
            }
            0b001 => {
                let imm = (bits(i, 12, 12) << 5) | rs2;
                if rv64 {
                    // C.ADDIW. rd == x0 is reserved: it would be a 32-bit
                    // truncation with nowhere to put the result.
                    if rd == 0 {
                        return None;
                    }
                    Some(i_type(OP_IMM32, 0x0, rd, rd, sext(imm, 6)))
                } else {
                    // C.JAL, which only exists on RV32.
                    Some(j_type(1, cj_offset(i)))
                }
            }
            0b010 => {
                // C.LI
                let imm = sext((bits(i, 12, 12) << 5) | rs2, 6);
                Some(i_type(OP_IMM, 0x0, rd, 0, imm))
            }
            0b011 => {
                if rd == 2 {
                    // C.ADDI16SP
                    let imm = (bits(i, 12, 12) << 9)
                        | (bits(i, 4, 3) << 7)
                        | (bits(i, 5, 5) << 6)
                        | (bits(i, 2, 2) << 5)
                        | (bits(i, 6, 6) << 4);
                    if imm == 0 {
                        return None; // reserved
                    }
                    Some(i_type(OP_IMM, 0x0, 2, 2, sext(imm, 10)))
                } else {
                    // C.LUI. The immediate lands in bits 17:12 of the result,
                    // so it is a 6-bit signed value scaled by 2^12.
                    let imm = (bits(i, 12, 12) << 5) | rs2;
                    if imm == 0 || rd == 0 {
                        return None; // reserved
                    }
                    Some(u_type(0x37, rd, sext(imm, 6) << 12))
                }
            }
            0b100 => {
                let shamt = (bits(i, 12, 12) << 5) | rs2;
                match bits(i, 11, 10) {
                    // C.SRLI / C.SRAI. On RV32 a shift of 32 or more is reserved.
                    0b00 | 0b01 => {
                        if !rv64 && shamt >= 32 {
                            return None;
                        }
                        // SRAI is selected by funct6 = 0b010000, which sits
                        // in imm[11:6] -- that is 0x400, not 0x20 shifted.
                        let sel = if bits(i, 11, 10) == 0b01 {
                            0x400
                        } else {
                            0x000
                        };
                        Some(i_type(
                            OP_IMM,
                            0x5,
                            rp(i, 7),
                            rp(i, 7),
                            (sel | shamt) as i32,
                        ))
                    }
                    // C.ANDI
                    0b10 => {
                        let imm = sext((bits(i, 12, 12) << 5) | rs2, 6);
                        Some(i_type(OP_IMM, 0x7, rp(i, 7), rp(i, 7), imm))
                    }
                    // Register-register forms, split by bit 12 into the
                    // XLEN-wide group and the RV64-only word group.
                    _ => {
                        let (d, s) = (rp(i, 7), rp(i, 2));
                        match (bits(i, 12, 12), bits(i, 6, 5)) {
                            (0, 0b00) => Some(r_type(OP, 0x0, 0x20, d, d, s)), // C.SUB
                            (0, 0b01) => Some(r_type(OP, 0x4, 0x00, d, d, s)), // C.XOR
                            (0, 0b10) => Some(r_type(OP, 0x6, 0x00, d, d, s)), // C.OR
                            (0, 0b11) => Some(r_type(OP, 0x7, 0x00, d, d, s)), // C.AND
                            (1, 0b00) if rv64 => Some(r_type(OP32, 0x0, 0x20, d, d, s)), // C.SUBW
                            (1, 0b01) if rv64 => Some(r_type(OP32, 0x0, 0x00, d, d, s)), // C.ADDW
                            _ => None,                                         // reserved
                        }
                    }
                }
            }
            // C.J
            0b101 => Some(j_type(0, cj_offset(i))),
            // C.BEQZ / C.BNEZ
            0b110 => Some(b_type(0x0, rp(i, 7), 0, cb_offset(i))),
            _ => Some(b_type(0x1, rp(i, 7), 0, cb_offset(i))),
        },

        // Quadrant 2: stack-pointer-relative access and the register moves.
        0b10 => match funct3 {
            0b000 => {
                // C.SLLI
                let shamt = (bits(i, 12, 12) << 5) | rs2;
                if !rv64 && shamt >= 32 {
                    return None;
                }
                Some(i_type(OP_IMM, 0x1, rd, rd, shamt as i32))
            }
            0b010 => {
                // C.LWSP. rd == x0 is reserved.
                if rd == 0 {
                    return None;
                }
                let imm = (bits(i, 3, 2) << 6) | (bits(i, 12, 12) << 5) | (bits(i, 6, 4) << 2);
                Some(i_type(LOAD, 0x2, rd, 2, imm as i32))
            }
            0b011 if rv64 => {
                // C.LDSP
                if rd == 0 {
                    return None;
                }
                let imm = (bits(i, 4, 2) << 6) | (bits(i, 12, 12) << 5) | (bits(i, 6, 5) << 3);
                Some(i_type(LOAD, 0x3, rd, 2, imm as i32))
            }
            0b100 => match (bits(i, 12, 12), rd, rs2) {
                (0, 0, _) => None,                                     // reserved
                (0, _, 0) => Some(i_type(JALR, 0x0, 0, rd, 0)),        // C.JR
                (0, _, _) => Some(r_type(OP, 0x0, 0x00, rd, 0, rs2)),  // C.MV
                (_, 0, 0) => Some(0x0010_0073),                        // C.EBREAK
                (_, _, 0) => Some(i_type(JALR, 0x0, 1, rd, 0)),        // C.JALR
                (_, _, _) => Some(r_type(OP, 0x0, 0x00, rd, rd, rs2)), // C.ADD
            },
            0b001 => {
                // C.FLDSP. f0 is a perfectly good destination, so unlike the
                // integer stack loads there is no reserved rd.
                let imm = (bits(i, 4, 2) << 6) | (bits(i, 12, 12) << 5) | (bits(i, 6, 5) << 3);
                Some(i_type(LOAD_FP, 0x3, rd, 2, imm as i32))
            }
            0b011 if !rv64 => {
                // C.FLWSP
                let imm = (bits(i, 3, 2) << 6) | (bits(i, 12, 12) << 5) | (bits(i, 6, 4) << 2);
                Some(i_type(LOAD_FP, 0x2, rd, 2, imm as i32))
            }
            0b101 => {
                // C.FSDSP
                let imm = (bits(i, 9, 7) << 6) | (bits(i, 12, 10) << 3);
                Some(s_type(STORE_FP, 0x3, 2, rs2, imm as i32))
            }
            0b111 if !rv64 => {
                // C.FSWSP
                let imm = (bits(i, 8, 7) << 6) | (bits(i, 12, 9) << 2);
                Some(s_type(STORE_FP, 0x2, 2, rs2, imm as i32))
            }
            0b110 => {
                // C.SWSP
                let imm = (bits(i, 8, 7) << 6) | (bits(i, 12, 9) << 2);
                Some(s_type(STORE, 0x2, 2, rs2, imm as i32))
            }
            0b111 if rv64 => {
                // C.SDSP
                let imm = (bits(i, 9, 7) << 6) | (bits(i, 12, 10) << 3);
                Some(s_type(STORE, 0x3, 2, rs2, imm as i32))
            }
            _ => None,
        },

        // 0b11 is not compressed; the caller never gets here.
        _ => None,
    }
}

/// The CJ format's jump offset: an 11-bit signed value scaled by 2.
fn cj_offset(i: u32) -> i32 {
    let imm = (bits(i, 12, 12) << 11)
        | (bits(i, 8, 8) << 10)
        | (bits(i, 10, 9) << 8)
        | (bits(i, 6, 6) << 7)
        | (bits(i, 7, 7) << 6)
        | (bits(i, 2, 2) << 5)
        | (bits(i, 11, 11) << 4)
        | (bits(i, 5, 3) << 1);
    sext(imm, 12)
}

/// The CB format's branch offset: an 8-bit signed value scaled by 2.
fn cb_offset(i: u32) -> i32 {
    let imm = (bits(i, 12, 12) << 8)
        | (bits(i, 6, 5) << 6)
        | (bits(i, 2, 2) << 5)
        | (bits(i, 11, 10) << 3)
        | (bits(i, 4, 3) << 1);
    sext(imm, 9)
}
