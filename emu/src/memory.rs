//! Physical memory: flat little-endian DRAM, the CLINT, and the `tohost`
//! handshake the riscv-tests suite uses to report pass/fail.
//!
//! A failed access here is `Unmapped`. Whether that becomes a load fault, a store
//! fault or an instruction fault depends on what the access was for, and only
//! the caller knows that.

/// A physical access that landed in neither DRAM nor a device.
///
/// Deliberately says nothing about *why*: whether that is an instruction,
/// load or store fault depends on what the access was for, and only the
/// caller knows that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unmapped;

impl std::fmt::Display for Unmapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("address is not mapped to memory or a device")
    }
}

impl std::error::Error for Unmapped {}

/// Where DRAM appears in the physical address space. Matches the convention
/// used by SiFive boards and the riscv-tests default linker script.
pub const DRAM_BASE: u64 = 0x8000_0000;

/// The core-local interruptor: software interrupt, timer compare, and the
/// machine timer itself.
pub const CLINT_BASE: u64 = 0x0200_0000;
pub const CLINT_END: u64 = CLINT_BASE + 0xC000;
const CLINT_MSIP: u64 = CLINT_BASE;
const CLINT_MTIMECMP: u64 = CLINT_BASE + 0x4000;
const CLINT_MTIME: u64 = CLINT_BASE + 0xBFF8;

#[derive(Default)]
pub struct Clint {
    pub msip: u32,
    pub mtimecmp: u64,
    /// Advanced by the hart, one tick per instruction. Real hardware runs
    /// this off a fixed-frequency oscillator, but nothing here depends on the
    /// rate, only on it increasing.
    pub mtime: u64,
}

pub struct Memory {
    dram: Vec<u8>,
    pub clint: Clint,
    /// Address the payload writes to signal termination, if it declares one.
    pub tohost: Option<u64>,
    /// Last value written to `tohost`; `Some(0)` never occurs (0 means "running").
    pub tohost_value: Option<u64>,
}

impl Memory {
    pub fn new(size: usize) -> Self {
        Memory {
            dram: vec![0; size],
            clint: Clint::default(),
            tohost: None,
            tohost_value: None,
        }
    }

    /// Copies `image` to `DRAM_BASE`, the reset entry point.
    pub fn load(&mut self, image: &[u8]) {
        self.dram[..image.len()].copy_from_slice(image);
    }

    /// Places `data` at an arbitrary physical address, for an ELF segment.
    pub fn load_at(&mut self, addr: u64, data: &[u8]) -> Result<(), Unmapped> {
        let i = self.index(addr, data.len() as u64).ok_or(Unmapped)?;
        self.dram[i..i + data.len()].copy_from_slice(data);
        Ok(())
    }

    /// Clears `len` bytes, for the .bss tail of a segment.
    pub fn zero(&mut self, addr: u64, len: u64) -> Result<(), Unmapped> {
        let i = self.index(addr, len).ok_or(Unmapped)?;
        self.dram[i..i + len as usize].fill(0);
        Ok(())
    }

    fn index(&self, addr: u64, size: u64) -> Option<usize> {
        let off = addr.checked_sub(DRAM_BASE)?;
        if off.checked_add(size)? > self.dram.len() as u64 {
            return None;
        }
        Some(off as usize)
    }

    /// `size` is in bytes and must be 1, 2, 4 or 8.
    pub fn read(&self, addr: u64, size: u64) -> Result<u64, Unmapped> {
        if (CLINT_BASE..CLINT_END).contains(&addr) {
            return Ok(self.clint_read(addr, size));
        }
        let i = self.index(addr, size).ok_or(Unmapped)?;
        let mut v = 0u64;
        for b in (0..size as usize).rev() {
            v = (v << 8) | self.dram[i + b] as u64;
        }
        Ok(v)
    }

    pub fn write(&mut self, addr: u64, size: u64, value: u64) -> Result<(), Unmapped> {
        if (CLINT_BASE..CLINT_END).contains(&addr) {
            self.clint_write(addr, size, value);
            return Ok(());
        }
        let i = self.index(addr, size).ok_or(Unmapped)?;
        for b in 0..size as usize {
            self.dram[i + b] = (value >> (8 * b)) as u8;
        }
        if self.tohost == Some(addr) && value != 0 {
            self.tohost_value = Some(value);
        }
        Ok(())
    }

    /// The CLINT's registers are 32- or 64-bit; a narrower access reads the
    /// corresponding slice, which is how RV32 software reaches a 64-bit
    /// `mtime` in two halves.
    fn clint_read(&self, addr: u64, size: u64) -> u64 {
        let (base, full) = self.clint_field(addr);
        let shift = (addr - base) * 8;
        let v = full >> shift;
        if size >= 8 {
            v
        } else {
            v & ((1u64 << (size * 8)) - 1)
        }
    }

    fn clint_write(&mut self, addr: u64, size: u64, value: u64) {
        let (base, full) = self.clint_field(addr);
        let shift = (addr - base) * 8;
        let mask = if size >= 8 {
            u64::MAX
        } else {
            ((1u64 << (size * 8)) - 1) << shift
        };
        let merged = (full & !mask) | ((value << shift) & mask);
        match base {
            CLINT_MSIP => self.clint.msip = merged as u32 & 1,
            CLINT_MTIMECMP => self.clint.mtimecmp = merged,
            _ => self.clint.mtime = merged,
        }
    }

    /// Maps an address to the register it falls in, and that register's value.
    fn clint_field(&self, addr: u64) -> (u64, u64) {
        if addr >= CLINT_MTIME {
            (CLINT_MTIME, self.clint.mtime)
        } else if addr >= CLINT_MTIMECMP {
            (CLINT_MTIMECMP, self.clint.mtimecmp)
        } else {
            (CLINT_MSIP, self.clint.msip as u64)
        }
    }
}
