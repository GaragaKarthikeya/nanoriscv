//! The subset of machine-mode CSRs the core needs to take and return from a trap.

pub const MSTATUS: u16 = 0x300;
pub const MIE: u16 = 0x304;
pub const MTVEC: u16 = 0x305;
pub const MSCRATCH: u16 = 0x340;
pub const MEPC: u16 = 0x341;
pub const MCAUSE: u16 = 0x342;
pub const MTVAL: u16 = 0x343;
pub const MIP: u16 = 0x344;
pub const CYCLE: u16 = 0xC00;
pub const TIME: u16 = 0xC01;
pub const INSTRET: u16 = 0xC02;

pub struct CsrFile {
    regs: [u64; 4096],
}

impl CsrFile {
    pub fn new() -> Self {
        CsrFile { regs: [0; 4096] }
    }

    pub fn read(&self, addr: u16) -> u64 {
        self.regs[addr as usize]
    }

    pub fn write(&mut self, addr: u16, value: u64) {
        // 0xC00..0xC1F are read-only counters; the CPU updates them directly.
        if addr & 0xC00 == 0xC00 && addr < 0xD00 {
            return;
        }
        self.regs[addr as usize] = value;
    }

    /// Bypasses the read-only check, for counters the CPU itself advances.
    pub fn force(&mut self, addr: u16, value: u64) {
        self.regs[addr as usize] = value;
    }
}

impl Default for CsrFile {
    fn default() -> Self {
        Self::new()
    }
}
