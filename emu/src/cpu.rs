//! A RISC-V hart, machine mode only, RV32 or RV64.
//!
//! `step()` executes exactly one instruction and is the unit the RTL core will
//! be diffed against: after each step the architectural state here (pc, x1..x31,
//! and the machine CSRs) must match the core's retire-stage state exactly.
//!
//! Both widths live in one implementation because the RTL core is RV32 while
//! the software side is heading for RV64, and a single model keeps those from
//! drifting apart. The trick that makes it cheap: RV64's `*W` instructions have
//! exactly RV32 semantics plus a sign-extension, so every ALU operation is
//! written once and parameterised by a `width` -- 32 or 64 -- rather than
//! duplicated per extension.

use crate::compress::decompress;
use crate::csr::{self, int, mstatus, CsrFile};
use crate::decode::*;
use crate::memory::{Memory, DRAM_BASE};
use crate::mmu;
use crate::plic;
use crate::trap::{interrupt, Access, Exception, Priv};
use crate::uart;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Xlen {
    Rv32,
    Rv64,
}

impl Xlen {
    pub fn bits(self) -> u32 {
        match self {
            Xlen::Rv32 => 32,
            Xlen::Rv64 => 64,
        }
    }
}

/// Sign-extends the low `width` bits of `v` to 64 bits.
#[inline]
fn sext(v: u64, width: u32) -> u64 {
    if width >= 64 {
        v
    } else {
        let shift = 64 - width;
        (((v << shift) as i64) >> shift) as u64
    }
}

/// Keeps only the low `width` bits.
#[inline]
fn trunc(v: u64, width: u32) -> u64 {
    if width >= 64 {
        v
    } else {
        v & ((1u64 << width) - 1)
    }
}

/// Translation-cache size, in entries. Direct-mapped, so this is also its
/// associativity budget: big enough that a kernel's working set of pages does
/// not thrash, small enough to stay in the host's cache.
const TLB_ENTRIES: usize = 1024;

/// How often the device interrupt state is swept even with no device access.
/// Only external input needs this, and a few hundred instructions of latency
/// on a keystroke is below anything a guest can notice.
const DEVICE_POLL_INTERVAL: u64 = 256;

/// The `mstatus` bits a translation depends on: SUM and MXR change what a
/// supervisor access is allowed to reach, and MPRV changes which privilege
/// the access is made at.
const TLB_STATUS_BITS: u64 = mstatus::SUM | mstatus::MXR | mstatus::MPRV;

pub struct Cpu {
    /// Registers hold the zero-extended XLEN-bit value, so on RV32 they read
    /// back exactly as the 32-bit core's register file will. x0 is stored but
    /// always reads as zero.
    pub regs: [u64; 32],
    pub pc: u64,
    pub xlen: Xlen,
    pub csrs: CsrFile,
    pub mem: Memory,
    pub cycle: u64,
    /// The address reserved by the most recent LR, if any. A single hart never
    /// has a reservation broken by someone else, so SC fails only when there
    /// was no LR or it named a different address.
    pub reservation: Option<u64>,
    /// The privilege the hart is executing at. Reset leaves it in machine
    /// mode, which is the only mode guaranteed to exist.
    pub priv_mode: Priv,
    /// Whether the emulator answers supervisor `ecall`s itself, standing in
    /// for the machine-mode firmware a real board would run.
    pub sbi: bool,
    /// Whether a timer has been scheduled through SBI and not yet fired.
    pub timer_armed: bool,
    /// Set when the guest has asked to power off.
    pub shutdown: bool,
    /// Set when the instruction being executed wrote `minstret` itself.
    ///
    /// Writing the retired-instruction counter suppresses that instruction's
    /// own increment, so the value written is the value the *next* instruction
    /// reads. Without this a write would always come back one too high.
    wrote_instret: bool,
    /// A direct-mapped translation cache. A page walk is three dependent
    /// memory reads, and every fetch and every load or store needs one, so
    /// without this the walker dominates the emulator's running time.
    tlb: Vec<[TlbEntry; 3]>,
}

/// One cached translation. The tag carries everything the walk depended on
/// besides the page tables themselves -- the root pointer, the privilege, and
/// the `mstatus` bits that change what is permitted -- so a change to any of
/// them misses rather than returning a stale mapping. Changes to the page
/// table *contents* are covered by SFENCE.VMA, which flushes.
#[derive(Clone, Copy, Default)]
struct TlbEntry {
    valid: bool,
    vpn: u64,
    ppn: u64,
    satp: u64,
    ctx: u64,
}

impl Cpu {
    pub fn new(mem_size: usize, xlen: Xlen) -> Self {
        let mut cpu = Cpu {
            regs: [0; 32],
            pc: DRAM_BASE,
            xlen,
            csrs: CsrFile::new(xlen),
            mem: Memory::new(mem_size),
            cycle: 0,
            reservation: None,
            tlb: vec![[TlbEntry::default(); 3]; TLB_ENTRIES],
            priv_mode: Priv::Machine,
            sbi: false,
            timer_armed: false,
            shutdown: false,
            wrote_instret: false,
        };
        // Stack pointer starts at the top of DRAM, as a bare-metal ABI expects.
        cpu.regs[2] = DRAM_BASE + mem_size as u64;
        cpu
    }

    pub fn rv32(mem_size: usize) -> Self {
        Cpu::new(mem_size, Xlen::Rv32)
    }

    pub fn rv64(mem_size: usize) -> Self {
        Cpu::new(mem_size, Xlen::Rv64)
    }

    #[inline]
    fn xbits(&self) -> u32 {
        self.xlen.bits()
    }

    #[inline]
    fn rr(&self, r: usize) -> u64 {
        if r == 0 {
            0
        } else {
            self.regs[r]
        }
    }

    /// Writes a register, normalising to the XLEN-bit canonical form.
    #[inline]
    fn wr(&mut self, r: usize, v: u64) {
        if r != 0 {
            self.regs[r] = trunc(v, self.xbits());
        }
    }

    /// Register value interpreted as signed at the current XLEN.
    #[inline]
    fn rs(&self, r: usize) -> i64 {
        sext(self.rr(r), self.xbits()) as i64
    }

    /// Translates a virtual address for `access`, which is a no-op when the
    /// hart is in machine mode or paging is off.
    fn translate(&mut self, va: u64, access: Access) -> Result<u64, Exception> {
        let (satp, status, mode, xlen) = (
            self.csrs.read(csr::SATP),
            self.csrs.read(csr::MSTATUS),
            self.priv_mode,
            self.xlen,
        );
        let vpn = va >> 12;
        let ctx = (status & TLB_STATUS_BITS) | mode as u64;
        // Each access kind gets its own way. They share a page constantly --
        // every instruction fetches, and most also load -- so indexing on the
        // page alone would make fetch and load evict each other every step.
        let way = access as usize;
        let slot = (vpn as usize) & (TLB_ENTRIES - 1);
        let e = self.tlb[slot][way];
        if e.valid && e.vpn == vpn && e.satp == satp && e.ctx == ctx {
            return Ok(e.ppn | (va & 0xfff));
        }
        let pa = mmu::translate(&mut self.mem, xlen, satp, status, mode, va, access)?;
        // Only successful walks are cached: a fault has to be re-taken every
        // time, since the kernel may have fixed the mapping in between.
        self.tlb[slot][way] = TlbEntry {
            valid: true,
            vpn,
            ppn: pa & !0xfff,
            satp,
            ctx,
        };
        Ok(pa)
    }

    /// Drops every cached translation. Called for SFENCE.VMA, which is the
    /// guest's promise that it has finished editing the page tables.
    fn flush_tlb(&mut self) {
        for slot in &mut self.tlb {
            for e in slot {
                e.valid = false;
            }
        }
    }

    /// Reads memory through the MMU.
    ///
    /// An access that stays inside one page needs a single translation. One
    /// that straddles a page boundary is split byte by byte, because the two
    /// halves may map to unrelated physical pages -- or the second may not be
    /// mapped at all, which has to fault rather than read the first page twice.
    fn read_mem(&mut self, va: u64, size: u64) -> Result<u64, Exception> {
        if (va & 0xfff) + size <= 0x1000 {
            let pa = self.translate(va, Access::Load)?;
            return self
                .mem
                .read(pa, size)
                .map_err(|_| Exception::LoadAccessFault(va));
        }
        let mut v = 0u64;
        for k in 0..size {
            let pa = self.translate(va + k, Access::Load)?;
            let b = self
                .mem
                .read(pa, 1)
                .map_err(|_| Exception::LoadAccessFault(va))?;
            v |= b << (8 * k);
        }
        Ok(v)
    }

    /// Writes memory through the MMU, splitting a page-crossing access the
    /// same way `read_mem` does.
    fn write_mem(&mut self, va: u64, size: u64, value: u64) -> Result<(), Exception> {
        if (va & 0xfff) + size <= 0x1000 {
            let pa = self.translate(va, Access::Store)?;
            return self
                .mem
                .write(pa, size, value)
                .map_err(|_| Exception::StoreAccessFault(va));
        }
        // Both halves are translated before either is written, so a fault on
        // the second page does not leave the first partially updated.
        for k in 0..size {
            self.translate(va + k, Access::Store)?;
        }
        for k in 0..size {
            let pa = self.translate(va + k, Access::Store)?;
            self.mem
                .write(pa, 1, value >> (8 * k))
                .map_err(|_| Exception::StoreAccessFault(va))?;
        }
        Ok(())
    }

    /// Fetches one instruction and reports its encoded length in bytes.
    ///
    /// A compressed instruction is expanded here, so nothing downstream needs
    /// to know that C exists. With C implemented, instructions need only
    /// 2-byte alignment -- requiring 4 would reject perfectly legal targets.
    fn fetch(&mut self) -> Result<(u32, u64), Exception> {
        if self.pc & 0x1 != 0 {
            return Err(Exception::InstructionAddressMisaligned(self.pc));
        }
        // Both low bits set means a 32-bit encoding; anything else is
        // compressed. Vol I, "Base Instruction-Length Encoding".
        //
        // When all four bytes are on the same page the fetch is one
        // translation and one read, which is the case for all but one
        // instruction in a thousand.
        if self.pc & 0xfff <= 0xffc {
            let pa = self.translate(self.pc, Access::Fetch)?;
            let word = self
                .mem
                .read(pa, 4)
                .map_err(|_| Exception::InstructionAccessFault(self.pc))?
                as u32;
            if word & 0x3 != 0x3 {
                let lo = word & 0xffff;
                let expanded =
                    decompress(lo, self.xlen).ok_or(Exception::IllegalInstruction(lo))?;
                return Ok((expanded, 2));
            }
            return Ok((word, 4));
        }
        let lo = self.fetch_half(self.pc)? as u32;
        if lo & 0x3 != 0x3 {
            let expanded = decompress(lo, self.xlen).ok_or(Exception::IllegalInstruction(lo))?;
            return Ok((expanded, 2));
        }
        let hi = self.fetch_half(self.pc + 2)? as u32;
        Ok(((hi << 16) | lo, 4))
    }

    /// Fetches one halfword. A 32-bit instruction is fetched as two of these
    /// because it may straddle a page boundary, with the second half on a
    /// page that is not mapped.
    fn fetch_half(&mut self, va: u64) -> Result<u64, Exception> {
        let pa = self.translate(va, Access::Fetch)?;
        self.mem
            .read(pa, 2)
            .map_err(|_| Exception::InstructionAccessFault(va))
    }

    /// Fetches, executes and retires one instruction. On an exception the trap
    /// is taken here, so the hart is left ready to run the handler.
    pub fn step(&mut self) -> Result<(), Exception> {
        self.cycle += 1;
        self.csrs.force(csr::MCYCLE, self.cycle);
        self.tick_timer();

        // An interrupt is taken before the next instruction rather than in
        // the middle of one, so this is the only place it can happen.
        if let Some(cause) = self.pending_interrupt() {
            let pc = self.pc;
            self.take_trap(cause, 0, pc, true);
            return Ok(());
        }

        // Held across execute(), which advances self.pc before it can fault.
        let inst_pc = self.pc;
        self.wrote_instret = false;
        let result = self.fetch().and_then(|(inst, len)| {
            let next = self.pc.wrapping_add(len);
            self.execute(inst, inst_pc, next)
        });

        match result {
            Ok(()) => {
                if !self.wrote_instret {
                    let retired = self.csrs.read(csr::MINSTRET).wrapping_add(1);
                    self.csrs.force(csr::MINSTRET, retired);
                }
                Ok(())
            }
            // An ecall from supervisor mode is a call into the firmware, not
            // a trap. self.pc already points past it, so answering here
            // returns to the instruction after the call.
            Err(Exception::EnvironmentCall) if self.sbi && self.priv_mode == Priv::Supervisor => {
                if !self.handle_sbi() {
                    self.shutdown = true;
                }
                Ok(())
            }
            Err(e) => {
                self.trap(e, inst_pc);
                Err(e)
            }
        }
    }

    /// Advances the machine timer and reflects the CLINT into `mip`.
    ///
    /// One tick per instruction is not how real hardware works -- mtime runs
    /// off a fixed oscillator, independent of the core -- but nothing here
    /// depends on the rate, only on the count increasing.
    fn tick_timer(&mut self) {
        self.mem.clint.mtime = self.mem.clint.mtime.wrapping_add(1);
        self.csrs.force(csr::TIME, self.mem.clint.mtime);

        if self.sbi {
            // The kernel cannot see the CLINT, so the firmware owns mtimecmp
            // and turns its expiry into a supervisor timer interrupt.
            if self.timer_armed && self.mem.clint.mtime >= self.mem.clint.mtimecmp {
                self.csrs.set_bits(csr::MIP, int::STIP);
                self.timer_armed = false;
            }
        } else if self.mem.clint.mtime >= self.mem.clint.mtimecmp {
            self.csrs.set_bits(csr::MIP, int::MTIP);
        } else {
            self.csrs.clear_bits(csr::MIP, int::MTIP);
        }
        if self.mem.clint.msip != 0 {
            self.csrs.set_bits(csr::MIP, int::MSIP);
        } else {
            self.csrs.clear_bits(csr::MIP, int::MSIP);
        }

        // Devices assert their lines into the PLIC, which decides whether a
        // context has anything worth interrupting for. The external-interrupt
        // bits are driven entirely from here, never written by software.
        //
        // Nothing here can change unless a device register was touched, so
        // the scan runs then rather than on every instruction. The periodic
        // sweep covers input arriving from outside the guest.
        if !self.mem.devices_dirty
            && !self.mem.uart.has_input()
            && !self.cycle.is_multiple_of(DEVICE_POLL_INTERVAL)
        {
            return;
        }
        self.mem.devices_dirty = false;
        let uart_active = self.mem.uart.is_interrupting();
        self.mem.plic.set_level(uart::UART_IRQ, uart_active);
        for (context, bit) in [
            (plic::CONTEXT_MACHINE, int::MEIP),
            (plic::CONTEXT_SUPERVISOR, int::SEIP),
        ] {
            if self.mem.plic.is_pending(context) {
                self.csrs.set_bits(csr::MIP, bit);
            } else {
                self.csrs.clear_bits(csr::MIP, bit);
            }
        }
    }

    /// The highest-priority interrupt that is pending, enabled, and allowed
    /// to fire at the current privilege, if any.
    ///
    /// An interrupt destined for a mode more privileged than the current one
    /// is always taken; one destined for the current mode is taken only if
    /// that mode's global enable is set. A less privileged mode's interrupt
    /// never preempts.
    fn pending_interrupt(&self) -> Option<u64> {
        let pending = self.csrs.read(csr::MIE) & self.csrs.read(csr::MIP);
        if pending == 0 {
            return None;
        }
        let status = self.csrs.read(csr::MSTATUS);
        let mideleg = self.csrs.read(csr::MIDELEG);

        // Priority order is fixed by the spec: external, then software, then
        // timer, machine before supervisor at each step.
        const ORDER: [(u64, u64); 6] = [
            (int::MEIP, interrupt::MACHINE_EXTERNAL),
            (int::MSIP, interrupt::MACHINE_SOFTWARE),
            (int::MTIP, interrupt::MACHINE_TIMER),
            (int::SEIP, interrupt::SUPERVISOR_EXTERNAL),
            (int::SSIP, interrupt::SUPERVISOR_SOFTWARE),
            (int::STIP, interrupt::SUPERVISOR_TIMER),
        ];
        for (bit, cause) in ORDER {
            if pending & bit == 0 {
                continue;
            }
            let delegated = mideleg & bit != 0;
            let enabled = if delegated {
                match self.priv_mode {
                    Priv::Machine => false,
                    Priv::Supervisor => status & mstatus::SIE != 0,
                    Priv::User => true,
                }
            } else {
                self.priv_mode < Priv::Machine || status & mstatus::MIE != 0
            };
            if enabled {
                return Some(cause);
            }
        }
        None
    }

    /// `inst_pc` is the address of the instruction that raised the exception,
    /// which is what mepc must hold -- not wherever execute() left self.pc.
    fn trap(&mut self, e: Exception, inst_pc: u64) {
        let cause = e.cause(self.priv_mode);
        self.take_trap(cause, e.tval(), inst_pc, false);
    }

    /// Enters a trap handler, in supervisor mode if the cause is delegated
    /// there and the hart is not already in machine mode.
    ///
    /// The previous interrupt-enable and privilege are pushed into the
    /// xPIE and xPP fields, which is the whole of the return mechanism: xRET
    /// pops them back out.
    fn take_trap(&mut self, cause: u64, tval: u64, epc: u64, is_interrupt: bool) {
        let deleg = if is_interrupt {
            self.csrs.read(csr::MIDELEG)
        } else {
            self.csrs.read(csr::MEDELEG)
        };
        let to_supervisor = self.priv_mode <= Priv::Supervisor && (deleg >> cause) & 1 == 1;

        // The interrupt flag is the top bit of the cause register, so its
        // position follows XLEN.
        let flag = if is_interrupt {
            1u64 << (self.xbits() - 1)
        } else {
            0
        };
        let status = self.csrs.read(csr::MSTATUS);
        let from = self.priv_mode;

        if to_supervisor {
            self.csrs.force(csr::SEPC, epc);
            self.csrs.force(csr::SCAUSE, cause | flag);
            self.csrs.force(csr::STVAL, tval);
            let mut s = status & !(mstatus::SPIE | mstatus::SIE | mstatus::SPP);
            if status & mstatus::SIE != 0 {
                s |= mstatus::SPIE;
            }
            if from == Priv::Supervisor {
                s |= mstatus::SPP;
            }
            self.csrs.force(csr::MSTATUS, s);
            self.priv_mode = Priv::Supervisor;
            self.pc = self.trap_vector(csr::STVEC, cause, is_interrupt);
        } else {
            self.csrs.force(csr::MEPC, epc);
            self.csrs.force(csr::MCAUSE, cause | flag);
            self.csrs.force(csr::MTVAL, tval);
            let mut s = status & !(mstatus::MPIE | mstatus::MIE | mstatus::MPP);
            if status & mstatus::MIE != 0 {
                s |= mstatus::MPIE;
            }
            s |= (from as u64) << mstatus::MPP_SHIFT;
            self.csrs.force(csr::MSTATUS, s);
            self.priv_mode = Priv::Machine;
            self.pc = self.trap_vector(csr::MTVEC, cause, is_interrupt);
        }
    }

    /// The handler address. In vectored mode interrupts fan out by cause;
    /// exceptions always land at the base.
    fn trap_vector(&self, which: u16, cause: u64, is_interrupt: bool) -> u64 {
        let tvec = self.csrs.read(which);
        let base = tvec & !0x3;
        if tvec & 0x3 == 1 && is_interrupt {
            base + 4 * cause
        } else {
            base
        }
    }

    /// Returns from a trap: pop the saved interrupt-enable and privilege,
    /// leave the popped xPIE set, and drop xPP to the least privileged mode.
    ///
    /// Setting xPP to U is not housekeeping -- it means a handler that
    /// returns twice cannot accidentally return to machine mode the second
    /// time.
    fn trap_return(&mut self, from_machine: bool) {
        let status = self.csrs.read(csr::MSTATUS);
        let (pie, ie, pp_mask, epc) = if from_machine {
            (mstatus::MPIE, mstatus::MIE, mstatus::MPP, csr::MEPC)
        } else {
            (mstatus::SPIE, mstatus::SIE, mstatus::SPP, csr::SEPC)
        };
        let target = if from_machine {
            Priv::from_bits((status & mstatus::MPP) >> mstatus::MPP_SHIFT)
        } else if status & mstatus::SPP != 0 {
            Priv::Supervisor
        } else {
            Priv::User
        };

        let mut s = status & !(ie | pp_mask);
        if status & pie != 0 {
            s |= ie;
        }
        s |= pie;
        // MPRV only has meaning in machine mode, so returning below it clears
        // the field rather than leaving loads redirected.
        if target != Priv::Machine {
            s &= !mstatus::MPRV;
        }
        self.csrs.force(csr::MSTATUS, s);
        self.priv_mode = target;
        self.pc = self.csrs.read(epc);
    }

    fn execute(&mut self, inst: u32, inst_pc: u64, next_pc: u64) -> Result<(), Exception> {
        let (rd, rs1, rs2) = (rd(inst), rs1(inst), rs2(inst));
        let (f3, f7) = (funct3(inst), funct7(inst));
        let xb = self.xbits();
        let illegal = Err(Exception::IllegalInstruction(inst));
        self.pc = next_pc;

        match opcode(inst) {
            // LUI
            0x37 => self.wr(rd, imm_u(inst) as i64 as u64),
            // AUIPC -- relative to the instruction's own address, not next_pc.
            0x17 => self.wr(rd, inst_pc.wrapping_add(imm_u(inst) as i64 as u64)),
            // JAL
            0x6f => {
                self.wr(rd, next_pc);
                self.pc = inst_pc.wrapping_add(imm_j(inst) as i64 as u64);
            }
            // JALR -- the low bit of the target is cleared by the spec.
            0x67 if f3 == 0 => {
                let target = self.rr(rs1).wrapping_add(imm_i(inst) as i64 as u64) & !1;
                self.wr(rd, next_pc);
                self.pc = trunc(target, xb);
            }
            // BRANCH
            0x63 => {
                let (a, b) = (self.rr(rs1), self.rr(rs2));
                let (sa, sb) = (self.rs(rs1), self.rs(rs2));
                let taken = match f3 {
                    0x0 => a == b,   // BEQ
                    0x1 => a != b,   // BNE
                    0x4 => sa < sb,  // BLT
                    0x5 => sa >= sb, // BGE
                    0x6 => a < b,    // BLTU
                    0x7 => a >= b,   // BGEU
                    _ => return illegal,
                };
                if taken {
                    self.pc = trunc(inst_pc.wrapping_add(imm_b(inst) as i64 as u64), xb);
                }
            }
            // LOAD
            0x03 => {
                let addr = self.rr(rs1).wrapping_add(imm_i(inst) as i64 as u64);
                let addr = trunc(addr, xb);
                // LD and LWU do not exist on RV32.
                let v = match f3 {
                    0x0 => sext(self.read_mem(addr, 1)?, 8),    // LB
                    0x1 => sext(self.read_mem(addr, 2)?, 16),   // LH
                    0x2 => sext(self.read_mem(addr, 4)?, 32),   // LW
                    0x3 if xb == 64 => self.read_mem(addr, 8)?, // LD
                    0x4 => self.read_mem(addr, 1)?,             // LBU
                    0x5 => self.read_mem(addr, 2)?,             // LHU
                    0x6 if xb == 64 => self.read_mem(addr, 4)?, // LWU
                    _ => return illegal,
                };
                self.wr(rd, v);
            }
            // STORE
            0x23 => {
                let addr = self.rr(rs1).wrapping_add(imm_s(inst) as i64 as u64);
                let addr = trunc(addr, xb);
                let v = self.rr(rs2);
                let size = match f3 {
                    0x0 => 1,
                    0x1 => 2,
                    0x2 => 4,
                    0x3 if xb == 64 => 8, // SD
                    _ => return illegal,
                };
                self.write_mem(addr, size, v)?;
            }
            // OP-IMM / OP-IMM-32. The 32-bit forms are RV64 only.
            0x13 | 0x1b => {
                let width = if opcode(inst) == 0x1b { 32 } else { xb };
                if width == 32 && opcode(inst) == 0x1b && xb == 32 {
                    return illegal;
                }
                let a = self.rr(rs1);
                // For shifts the immediate field holds the shift amount; for
                // everything else it is a sign-extended 12-bit constant.
                let b = if matches!(f3, 0x1 | 0x5) {
                    ((inst >> 20) & 0x3f) as u64
                } else {
                    imm_i(inst) as i64 as u64
                };
                // funct7 doubles as the shift-type selector; on RV64 its low
                // bit belongs to a 6-bit shift amount, so mask it off.
                let sel = if matches!(f3, 0x1 | 0x5) { f7 & !1 } else { 0 };
                // A 32-bit shift takes a 5-bit amount, so bit 25 is part of
                // funct7 and must be clear. Accepting it would silently turn
                // an illegal RV32 encoding into a shift by 32 or more.
                if matches!(f3, 0x1 | 0x5) && width == 32 && f7 & 1 != 0 {
                    return illegal;
                }
                match self.alu(f3, sel, a, b, width) {
                    Some(v) => self.wr(rd, v),
                    None => return illegal,
                }
            }
            // OP / OP-32. The 32-bit forms are RV64 only.
            0x33 | 0x3b => {
                let width = if opcode(inst) == 0x3b { 32 } else { xb };
                if opcode(inst) == 0x3b && xb == 32 {
                    return illegal;
                }
                let (a, b) = (self.rr(rs1), self.rr(rs2));
                let v = if f7 == 0x01 {
                    match self.muldiv(f3, a, b, width) {
                        Some(v) => v,
                        None => return illegal,
                    }
                } else {
                    match self.alu(f3, f7, a, b, width) {
                        Some(v) => v,
                        None => return illegal,
                    }
                };
                self.wr(rd, v);
            }
            // AMO: the A extension.
            0x2f => {
                let width = match f3 {
                    0x2 => 32,
                    0x3 if xb == 64 => 64,
                    _ => return illegal,
                };
                let size = width as u64 / 8;
                let addr = self.rr(rs1);
                // Atomics must be naturally aligned; unlike ordinary loads and
                // stores there is no misaligned fallback for them.
                if !addr.is_multiple_of(size) {
                    return Err(Exception::StoreAddressMisaligned(addr));
                }
                // The aq and rl bits occupy funct7[1:0]; ordering is a no-op
                // on one in-order hart, so only funct5 selects the operation.
                match f7 >> 2 {
                    // LR
                    0x02 => {
                        if rs2 != 0 {
                            return illegal;
                        }
                        let v = sext(self.read_mem(addr, size)?, width);
                        self.reservation = Some(addr);
                        self.wr(rd, v);
                    }
                    // SC. Writes 0 to rd on success and 1 on failure, and
                    // clears the reservation either way.
                    0x03 => {
                        let ok = self.reservation == Some(addr);
                        if ok {
                            let v = self.rr(rs2);
                            self.write_mem(addr, size, v)?;
                        }
                        self.reservation = None;
                        self.wr(rd, !ok as u64);
                    }
                    op => {
                        let old = self.read_mem(addr, size)?;
                        let a = sext(old, width);
                        let b = self.rr(rs2);
                        let (sa, sb) = (a as i64, sext(b, width) as i64);
                        let (ua, ub) = (trunc(a, width), trunc(b, width));
                        let new = match op {
                            0x00 => a.wrapping_add(b), // AMOADD
                            0x01 => b,                 // AMOSWAP
                            0x04 => a ^ b,             // AMOXOR
                            0x08 => a | b,             // AMOOR
                            0x0c => a & b,             // AMOAND
                            0x10 => sa.min(sb) as u64, // AMOMIN
                            0x14 => sa.max(sb) as u64, // AMOMAX
                            0x18 => ua.min(ub),        // AMOMINU
                            0x1c => ua.max(ub),        // AMOMAXU
                            _ => return illegal,
                        };
                        self.write_mem(addr, size, new)?;
                        // rd gets the value that was in memory beforehand.
                        self.wr(rd, a);
                    }
                }
            }
            // MISC-MEM: FENCE and FENCE.I are no-ops on a single in-order hart.
            0x0f => {}
            // SYSTEM
            0x73 => match f3 {
                0x0 => match inst >> 20 {
                    0x000 => return Err(Exception::EnvironmentCall),
                    0x001 => return Err(Exception::Breakpoint),
                    // SRET. TSR lets machine mode trap a supervisor's return,
                    // which is how a hypervisor keeps control of it.
                    0x102 => {
                        let status = self.csrs.read(csr::MSTATUS);
                        if self.priv_mode < Priv::Supervisor
                            || (self.priv_mode == Priv::Supervisor && status & mstatus::TSR != 0)
                        {
                            return illegal;
                        }
                        self.trap_return(false);
                    }
                    // MRET
                    0x302 => {
                        if self.priv_mode < Priv::Machine {
                            return illegal;
                        }
                        self.trap_return(true);
                    }
                    // WFI. There is nothing to wait for in a single-hart
                    // model, so it retires immediately; TW still makes it
                    // trap below machine mode.
                    0x105 => {
                        let status = self.csrs.read(csr::MSTATUS);
                        if self.priv_mode < Priv::Machine && status & mstatus::TW != 0 {
                            return illegal;
                        }
                    }
                    // SFENCE.VMA. TVM traps it in supervisor mode; otherwise
                    // it drops the translation cache. The address and ASID
                    // operands are ignored: flushing everything is always a
                    // correct implementation of a narrower fence.
                    v if v >> 5 == 0x09 => {
                        let status = self.csrs.read(csr::MSTATUS);
                        if self.priv_mode < Priv::Supervisor
                            || (self.priv_mode == Priv::Supervisor && status & mstatus::TVM != 0)
                        {
                            return illegal;
                        }
                        self.flush_tlb();
                    }
                    _ => return illegal,
                },
                // Zicsr. The read must happen before the write so that
                // `csrrw rd, csr, rd` still returns the old value.
                _ => {
                    let addr = csr(inst);
                    // A CSRRW always writes; a set or clear with rs1 == x0 is
                    // a read, and must not be rejected as a write to a
                    // read-only register.
                    let writes = f3 & 0x3 == 0x1 || rs1 != 0;
                    if !self.csrs.exists(addr)
                        || !CsrFile::accessible(addr, self.priv_mode, writes)
                        || !self.counter_enabled(addr)
                    {
                        return illegal;
                    }
                    // TVM traps a supervisor's view of the address space, and
                    // that means satp as well as SFENCE.VMA -- reading the
                    // page table root is as good as walking it.
                    if addr == csr::SATP
                        && self.priv_mode == Priv::Supervisor
                        && self.csrs.read(csr::MSTATUS) & mstatus::TVM != 0
                    {
                        return illegal;
                    }
                    let old = self.csrs.read(addr);
                    let src = if f3 & 0x4 != 0 {
                        rs1 as u64
                    } else {
                        self.rr(rs1)
                    };
                    let new = match f3 & 0x3 {
                        0x1 => src,        // CSRRW / CSRRWI
                        0x2 => old | src,  // CSRRS / CSRRSI
                        0x3 => old & !src, // CSRRC / CSRRCI
                        _ => return illegal,
                    };
                    if writes {
                        self.csrs.write(addr, trunc(new, xb));
                        if matches!(addr, csr::MINSTRET | csr::MINSTRETH | csr::INSTRET) {
                            self.wrote_instret = true;
                        }
                    }
                    self.wr(rd, old);
                }
            },
            _ => return illegal,
        }
        Ok(())
    }

    /// Whether the unprivileged counters may be read at the current
    /// privilege. `mcounteren` gates supervisor and below, `scounteren` gates
    /// user; a cleared bit makes the read an illegal instruction rather than
    /// returning a wrong number.
    fn counter_enabled(&self, addr: u16) -> bool {
        let bit = match addr {
            csr::CYCLE | csr::CYCLEH => 0,
            csr::TIME | csr::TIMEH => 1,
            csr::INSTRET | csr::INSTRETH => 2,
            _ => return true,
        };
        if self.priv_mode < Priv::Machine && self.csrs.read(csr::MCOUNTEREN) >> bit & 1 == 0 {
            return false;
        }
        if self.priv_mode < Priv::Supervisor && self.csrs.read(csr::SCOUNTEREN) >> bit & 1 == 0 {
            return false;
        }
        true
    }

    /// The integer ALU, computed at `width` bits and sign-extended to 64.
    ///
    /// Writing it once at a parameterised width is what makes RV64's ADDW,
    /// SLLW, SRLW and SRAW fall out of the RV32 cases for free.
    fn alu(&self, f3: u32, f7: u32, a: u64, b: u64, width: u32) -> Option<u64> {
        let shamt = (b & (width as u64 - 1)) as u32;
        let (sa, sb) = (sext(a, width), sext(b, width));
        let v = match (f3, f7) {
            (0x0, 0x00) => a.wrapping_add(b),                  // ADD / ADDI
            (0x0, 0x20) => a.wrapping_sub(b),                  // SUB
            (0x1, 0x00) => a << shamt,                         // SLL
            (0x2, 0x00) => ((sa as i64) < (sb as i64)) as u64, // SLT
            (0x3, 0x00) => (trunc(a, width) < trunc(b, width)) as u64, // SLTU
            (0x4, 0x00) => a ^ b,                              // XOR
            (0x5, 0x00) => trunc(a, width) >> shamt,           // SRL
            (0x5, 0x20) => ((sa as i64) >> shamt) as u64,      // SRA
            (0x6, 0x00) => a | b,                              // OR
            (0x7, 0x00) => a & b,                              // AND
            _ => return None,
        };
        // SLT and SLTU produce a 0/1 that must not be sign-extended, but since
        // the result is never negative the extension is a no-op for them.
        Some(sext(v, width))
    }

    /// The M extension at `width` bits, which also covers RV64's MULW, DIVW,
    /// DIVUW, REMW and REMUW.
    ///
    /// Division by zero and signed overflow have defined results in RISC-V
    /// rather than trapping, which is why they are spelled out. The zero checks
    /// stay explicit rather than folding into `checked_div`, so each arm reads
    /// the way the spec table does.
    #[allow(clippy::manual_checked_ops)]
    fn muldiv(&self, f3: u32, a: u64, b: u64, width: u32) -> Option<u64> {
        let (sa, sb) = (sext(a, width) as i64, sext(b, width) as i64);
        let (ua, ub) = (trunc(a, width), trunc(b, width));
        let v = match f3 {
            0x0 => a.wrapping_mul(b), // MUL / MULW
            // The high-half multiplies have no W form, so they are only
            // reachable at the full register width.
            0x1 if width == 64 => ((sa as i128 * sb as i128) >> 64) as u64, // MULH
            0x1 if width == 32 => ((sa * sb) >> 32) as u64,
            // MULHSU is signed rs1 times *unsigned rs2*, so the unsigned
            // operand is ub -- using ua here silently computes rs1 twice.
            0x2 if width == 64 => ((sa as i128 * ub as i128) >> 64) as u64, // MULHSU
            0x2 if width == 32 => ((sa * ub as i64) >> 32) as u64,
            0x3 if width == 64 => ((ua as u128 * ub as u128) >> 64) as u64, // MULHU
            0x3 if width == 32 => (ua * ub) >> 32,
            0x4 => {
                if sb == 0 {
                    u64::MAX // DIV by zero: all ones
                } else {
                    sa.wrapping_div(sb) as u64 // wrapping covers MIN / -1
                }
            }
            0x5 => {
                if ub == 0 {
                    u64::MAX // DIVU by zero
                } else {
                    ua / ub
                }
            }
            0x6 => {
                if sb == 0 {
                    sa as u64 // REM by zero: the dividend
                } else {
                    sa.wrapping_rem(sb) as u64
                }
            }
            0x7 => {
                if ub == 0 {
                    ua // REMU by zero
                } else {
                    ua % ub
                }
            }
            _ => return None,
        };
        Some(sext(v, width))
    }
}

/// Why a program stopped. `Pass`/`Fail` come from the `tohost` protocol that
/// riscv-tests uses; the rest are this simulator's own stopping conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The payload wrote 1 to `tohost`.
    Pass,
    /// The payload wrote `(n << 1) | 1`; `n` is the number of the failing test.
    Fail(u64),
    /// An ECALL with no `tohost` symbol to interpret it.
    Ecall,
    /// A trap was raised with `mtvec` still zero, so there is no handler to
    /// enter. Left as a distinct outcome because it is the usual symptom of a
    /// test that never got as far as installing one.
    UnhandledTrap(Exception),
    /// Ran past the step budget -- almost always an infinite loop.
    StepLimit,
    /// The guest asked the firmware to power off.
    Shutdown,
}

impl Cpu {
    /// Loads an ELF image: its PT_LOAD segments, entry point, the `tohost`
    /// symbol if the payload exports one, and the XLEN implied by its class.
    pub fn load_elf(&mut self, elf: &crate::elf::Elf) -> Result<(), Exception> {
        self.xlen = if elf.is_64 { Xlen::Rv64 } else { Xlen::Rv32 };
        // The CSR file masks reads and reports misa by width, so it has to
        // learn the new width too.
        self.csrs.set_xlen(self.xlen);
        // The stack pointer was set for the constructor's width; re-normalise.
        let sp = self.regs[2];
        self.regs[2] = trunc(sp, self.xbits());
        for seg in &elf.segments {
            let fault = || Exception::StoreAccessFault(seg.addr);
            self.mem.load_at(seg.addr, &seg.data).map_err(|_| fault())?;
            if seg.zero_len > 0 {
                self.mem
                    .zero(seg.addr + seg.data.len() as u64, seg.zero_len)
                    .map_err(|_| fault())?;
            }
        }
        self.pc = elf.entry;
        self.mem.tohost = elf.symbols.get("tohost").copied();
        Ok(())
    }

    /// Sets the hart up the way machine-mode firmware hands control to a
    /// kernel, and enters it in supervisor mode.
    ///
    /// The delegation mask is the interesting part: everything a supervisor
    /// can handle is delegated to it, *except* cause 9, an ecall from
    /// supervisor mode. That one has to stay with machine mode, because it is
    /// how the kernel calls the firmware.
    pub fn boot_supervisor(&mut self, entry: u64, hartid: u64, dtb: u64) {
        const DELEGATED: u64 = 0xb1ff;
        self.csrs.force(csr::MEDELEG, DELEGATED);
        self.csrs
            .force(csr::MIDELEG, int::SSIP | int::STIP | int::SEIP);
        // The kernel reads `time` and `cycle` directly, so machine mode has
        // to permit it; without this every rdtime is an illegal instruction.
        self.csrs.force(csr::MCOUNTEREN, !0);
        self.csrs.force(csr::SCOUNTEREN, !0);

        self.sbi = true;
        self.priv_mode = Priv::Supervisor;
        self.pc = entry;
        // The boot protocol: a0 is the hart id, a1 points at the device tree.
        self.regs[10] = hartid;
        self.regs[11] = dtb;
    }

    /// Whether a trap would vector to address zero, meaning no handler has
    /// been installed yet. Under SBI the emulator is the machine-mode
    /// firmware and everything a kernel can handle is delegated, so the
    /// vector that matters is the supervisor's.
    fn no_handler(&self) -> bool {
        let which = if self.sbi { csr::STVEC } else { csr::MTVEC };
        self.csrs.read(which) == 0
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
            if self.shutdown {
                return Exit::Shutdown;
            }
            if let Some(v) = self.mem.tohost_value {
                // Bit 0 set means "terminate"; the rest is the payload's status,
                // where 0 is success and n identifies the failing test case.
                if v & 1 == 1 {
                    return match v >> 1 {
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
                // A bare payload with no `tohost` has no other way to say
                // it is finished, so its ecall ends the run. Under SBI an
                // ecall is ordinary traffic -- every userspace syscall is
                // one -- and the kernel's handler deals with it.
                Err(Exception::EnvironmentCall) if !self.sbi && self.mem.tohost.is_none() => {
                    return Exit::Ecall
                }
                Err(e) if self.no_handler() => return Exit::UnhandledTrap(e),
                Err(_) => {}
            }
        }
        Exit::StepLimit
    }
}
