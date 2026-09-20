//! RV32IM hart, machine mode only.
//!
//! `step()` executes exactly one instruction and is the unit the RTL core will
//! be diffed against: after each step the architectural state here (pc, x1..x31,
//! and the machine CSRs) must match the core's retire-stage state exactly.

use crate::csr::{self, CsrFile};
use crate::decode::*;
use crate::memory::{Memory, DRAM_BASE};
use crate::trap::Exception;

pub struct Cpu {
    /// x0 is stored but always reads as zero; writes to it are dropped.
    pub regs: [u32; 32],
    pub pc: u32,
    pub csrs: CsrFile,
    pub mem: Memory,
    pub cycle: u64,
}

impl Cpu {
    pub fn new(mem_size: usize) -> Self {
        let mut cpu = Cpu {
            regs: [0; 32],
            pc: DRAM_BASE as u32,
            csrs: CsrFile::new(),
            mem: Memory::new(mem_size),
            cycle: 0,
        };
        // Stack pointer starts at the top of DRAM, as a bare-metal ABI expects.
        cpu.regs[2] = DRAM_BASE as u32 + mem_size as u32;
        cpu
    }

    #[inline]
    fn rr(&self, r: usize) -> u32 {
        if r == 0 {
            0
        } else {
            self.regs[r]
        }
    }

    #[inline]
    fn wr(&mut self, r: usize, v: u32) {
        if r != 0 {
            self.regs[r] = v;
        }
    }

    fn fetch(&self) -> Result<u32, Exception> {
        if self.pc & 0x3 != 0 {
            return Err(Exception::InstructionAccessFault(self.pc as u64));
        }
        self.mem
            .read(self.pc as u64, 4)
            .map(|v| v as u32)
            .map_err(|_| Exception::InstructionAccessFault(self.pc as u64))
    }

    /// Fetches, executes and retires one instruction. On an exception the trap
    /// is taken here, so the hart is left ready to run the handler.
    pub fn step(&mut self) -> Result<(), Exception> {
        self.cycle += 1;
        self.csrs.force(csr::CYCLE, self.cycle);
        self.csrs.force(csr::TIME, self.cycle);

        // Held across execute(), which advances self.pc before it can fault.
        let inst_pc = self.pc;
        let result = self.fetch().and_then(|inst| {
            let next = self.pc.wrapping_add(4);
            self.execute(inst, next)
        });

        match result {
            Ok(()) => {
                self.csrs
                    .force(csr::INSTRET, self.csrs.read(csr::INSTRET).wrapping_add(1));
                Ok(())
            }
            Err(e) => {
                self.trap(e, inst_pc);
                Err(e)
            }
        }
    }

    /// Enters the machine-mode handler: save the faulting pc and cause, then
    /// jump to mtvec. Only direct mode (mtvec[1:0] == 0) is implemented.
    ///
    /// `inst_pc` is the address of the instruction that raised the exception,
    /// which is what mepc must hold -- not wherever execute() left self.pc.
    fn trap(&mut self, e: Exception, inst_pc: u32) {
        self.csrs.write(csr::MEPC, inst_pc as u64);
        self.csrs.write(csr::MCAUSE, e.cause());
        self.csrs.write(csr::MTVAL, e.tval());
        let mtvec = self.csrs.read(csr::MTVEC) as u32;
        self.pc = mtvec & !0x3;
    }

    fn execute(&mut self, inst: u32, next_pc: u32) -> Result<(), Exception> {
        let (rd, rs1, rs2) = (rd(inst), rs1(inst), rs2(inst));
        let (f3, f7) = (funct3(inst), funct7(inst));
        let illegal = Err(Exception::IllegalInstruction(inst));
        self.pc = next_pc;

        match opcode(inst) {
            // LUI
            0x37 => self.wr(rd, imm_u(inst) as u32),
            // AUIPC — relative to the instruction's own address, not next_pc.
            0x17 => self.wr(rd, next_pc.wrapping_sub(4).wrapping_add(imm_u(inst) as u32)),
            // JAL
            0x6f => {
                self.wr(rd, next_pc);
                self.pc = next_pc.wrapping_sub(4).wrapping_add(imm_j(inst) as u32);
            }
            // JALR — the low bit of the target is cleared by the spec.
            0x67 if f3 == 0 => {
                let target = self.rr(rs1).wrapping_add(imm_i(inst) as u32) & !1;
                self.wr(rd, next_pc);
                self.pc = target;
            }
            // BRANCH
            0x63 => {
                let (a, b) = (self.rr(rs1), self.rr(rs2));
                let taken = match f3 {
                    0x0 => a == b,                   // BEQ
                    0x1 => a != b,                   // BNE
                    0x4 => (a as i32) < (b as i32),  // BLT
                    0x5 => (a as i32) >= (b as i32), // BGE
                    0x6 => a < b,                    // BLTU
                    0x7 => a >= b,                   // BGEU
                    _ => return illegal,
                };
                if taken {
                    self.pc = next_pc.wrapping_sub(4).wrapping_add(imm_b(inst) as u32);
                }
            }
            // LOAD
            0x03 => {
                let addr = self.rr(rs1).wrapping_add(imm_i(inst) as u32) as u64;
                let v = match f3 {
                    0x0 => self.mem.read(addr, 1)? as u8 as i8 as i32 as u32, // LB
                    0x1 => self.mem.read(addr, 2)? as u16 as i16 as i32 as u32, // LH
                    0x2 => self.mem.read(addr, 4)? as u32,                    // LW
                    0x4 => self.mem.read(addr, 1)? as u32,                    // LBU
                    0x5 => self.mem.read(addr, 2)? as u32,                    // LHU
                    _ => return illegal,
                };
                self.wr(rd, v);
            }
            // STORE
            0x23 => {
                let addr = self.rr(rs1).wrapping_add(imm_s(inst) as u32) as u64;
                let v = self.rr(rs2) as u64;
                match f3 {
                    0x0 => self.mem.write(addr, 1, v)?,
                    0x1 => self.mem.write(addr, 2, v)?,
                    0x2 => self.mem.write(addr, 4, v)?,
                    _ => return illegal,
                }
            }
            // OP-IMM
            0x13 => {
                let a = self.rr(rs1);
                let imm = imm_i(inst);
                // Shift amount is the low 5 bits of the immediate field on RV32.
                let shamt = (inst >> 20) & 0x1f;
                let v = match (f3, f7) {
                    (0x0, _) => a.wrapping_add(imm as u32),      // ADDI
                    (0x2, _) => ((a as i32) < imm) as u32,       // SLTI
                    (0x3, _) => (a < imm as u32) as u32,         // SLTIU
                    (0x4, _) => a ^ imm as u32,                  // XORI
                    (0x6, _) => a | imm as u32,                  // ORI
                    (0x7, _) => a & imm as u32,                  // ANDI
                    (0x1, 0x00) => a << shamt,                   // SLLI
                    (0x5, 0x00) => a >> shamt,                   // SRLI
                    (0x5, 0x20) => ((a as i32) >> shamt) as u32, // SRAI
                    _ => return illegal,
                };
                self.wr(rd, v);
            }
            // OP
            0x33 => {
                let (a, b) = (self.rr(rs1), self.rr(rs2));
                let shamt = b & 0x1f;
                let v = match (f3, f7) {
                    (0x0, 0x00) => a.wrapping_add(b),                // ADD
                    (0x0, 0x20) => a.wrapping_sub(b),                // SUB
                    (0x1, 0x00) => a << shamt,                       // SLL
                    (0x2, 0x00) => ((a as i32) < (b as i32)) as u32, // SLT
                    (0x3, 0x00) => (a < b) as u32,                   // SLTU
                    (0x4, 0x00) => a ^ b,                            // XOR
                    (0x5, 0x00) => a >> shamt,                       // SRL
                    (0x5, 0x20) => ((a as i32) >> shamt) as u32,     // SRA
                    (0x6, 0x00) => a | b,                            // OR
                    (0x7, 0x00) => a & b,                            // AND
                    (_, 0x01) => self.muldiv(f3, a, b),
                    _ => return illegal,
                };
                self.wr(rd, v);
            }
            // MISC-MEM: FENCE is a no-op on a single in-order hart.
            0x0f => {}
            // SYSTEM
            0x73 => match f3 {
                0x0 => match inst >> 20 {
                    0x000 => return Err(Exception::EnvironmentCall),
                    0x001 => return Err(Exception::Breakpoint),
                    // MRET
                    0x302 => self.pc = self.csrs.read(csr::MEPC) as u32,
                    _ => return illegal,
                },
                // Zicsr. The read must happen before the write so that
                // `csrrw rd, csr, rd` still returns the old value.
                _ => {
                    let addr = csr(inst);
                    let old = self.csrs.read(addr) as u32;
                    let src = if f3 & 0x4 != 0 {
                        rs1 as u32
                    } else {
                        self.rr(rs1)
                    };
                    let new = match f3 & 0x3 {
                        0x1 => src,        // CSRRW / CSRRWI
                        0x2 => old | src,  // CSRRS / CSRRSI
                        0x3 => old & !src, // CSRRC / CSRRCI
                        _ => return illegal,
                    };
                    // A set/clear with rs1 == x0 must not write the CSR at all.
                    if f3 & 0x3 == 0x1 || rs1 != 0 {
                        self.csrs.write(addr, new as u64);
                    }
                    self.wr(rd, old);
                }
            },
            _ => return illegal,
        }
        Ok(())
    }

    /// The M extension. Division by zero and signed overflow have defined
    /// results in RISC-V rather than trapping, which is why they are spelled out.
    /// The zero checks stay explicit rather than folding into `checked_div`, so
    /// each arm reads the way the spec table does.
    #[allow(clippy::manual_checked_ops)]
    fn muldiv(&self, f3: u32, a: u32, b: u32) -> u32 {
        let (sa, sb) = (a as i32, b as i32);
        match f3 {
            0x0 => a.wrapping_mul(b),                      // MUL
            0x1 => ((sa as i64 * sb as i64) >> 32) as u32, // MULH
            0x2 => ((sa as i64 * b as i64) >> 32) as u32,  // MULHSU
            0x3 => ((a as u64 * b as u64) >> 32) as u32,   // MULHU
            0x4 => {
                if b == 0 {
                    u32::MAX
                } else {
                    sa.wrapping_div(sb) as u32
                }
            } // DIV
            0x5 => {
                if b == 0 {
                    u32::MAX
                } else {
                    a / b
                }
            } // DIVU
            0x6 => {
                if b == 0 {
                    a
                } else {
                    sa.wrapping_rem(sb) as u32
                }
            } // REM
            _ => {
                if b == 0 {
                    a
                } else {
                    a % b
                }
            } // REMU
        }
    }
}

/// Why a program stopped. `Pass`/`Fail` come from the `tohost` protocol that
/// riscv-tests uses; the rest are this simulator's own stopping conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The payload wrote 1 to `tohost`.
    Pass,
    /// The payload wrote `(n << 1) | 1`; `n` is the number of the failing test.
    Fail(u32),
    /// An ECALL with no `tohost` symbol to interpret it.
    Ecall,
    /// A trap was raised with `mtvec` still zero, so there is no handler to
    /// enter. Left as a distinct outcome because it is the usual symptom of a
    /// test that never got as far as installing one.
    UnhandledTrap(Exception),
    /// Ran past the step budget -- almost always an infinite loop.
    StepLimit,
}

impl Cpu {
    /// Loads an ELF32 image: its PT_LOAD segments, entry point, and the
    /// `tohost` symbol if the payload exports one.
    pub fn load_elf(&mut self, elf: &crate::elf::Elf) -> Result<(), Exception> {
        for seg in &elf.segments {
            self.mem.load_at(seg.addr as u64, &seg.data)?;
            if seg.zero_len > 0 {
                self.mem
                    .zero(seg.addr as u64 + seg.data.len() as u64, seg.zero_len as u64)?;
            }
        }
        self.pc = elf.entry;
        self.mem.tohost = elf.symbols.get("tohost").map(|&a| a as u64);
        Ok(())
    }

    /// Steps until the program stops, for at most `max_steps` instructions.
    ///
    /// Traps are not stopping conditions on their own: a riscv-tests binary
    /// installs a handler and deliberately traps as part of the test. The run
    /// ends when the payload reports through `tohost`, or when a trap is taken
    /// with no handler installed.
    pub fn run(&mut self, max_steps: u64) -> Exit {
        for _ in 0..max_steps {
            let r = self.step();
            if let Some(v) = self.mem.tohost_value {
                // Bit 0 set means "terminate"; the rest is the payload's status,
                // where 0 is success and n identifies the failing test case.
                if v & 1 == 1 {
                    return match (v >> 1) as u32 {
                        0 => Exit::Pass,
                        n => Exit::Fail(n),
                    };
                }
                // An even value is a syscall request, which bare tests do not
                // use; clear it and keep going.
                self.mem.tohost_value = None;
            }
            match r {
                Ok(()) => {}
                Err(Exception::EnvironmentCall) if self.mem.tohost.is_none() => return Exit::Ecall,
                Err(e) if self.csrs.read(csr::MTVEC) == 0 => return Exit::UnhandledTrap(e),
                Err(_) => {}
            }
        }
        Exit::StepLimit
    }
}
