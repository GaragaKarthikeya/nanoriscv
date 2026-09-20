//! Flat little-endian memory with a fixed DRAM base, plus the `tohost`
//! handshake the riscv-tests suite uses to report pass/fail.

use crate::trap::Exception;

/// Where DRAM appears in the physical address space. Matches the convention
/// used by SiFive boards and the riscv-tests default linker script.
pub const DRAM_BASE: u64 = 0x8000_0000;

pub struct Memory {
    dram: Vec<u8>,
    /// Address the payload writes to signal termination, if it declares one.
    pub tohost: Option<u64>,
    /// Last value written to `tohost`; `Some(0)` never occurs (0 means "running").
    pub tohost_value: Option<u64>,
}

impl Memory {
    pub fn new(size: usize) -> Self {
        Memory {
            dram: vec![0; size],
            tohost: None,
            tohost_value: None,
        }
    }

    /// Copies `image` to `DRAM_BASE`, the reset entry point.
    pub fn load(&mut self, image: &[u8]) {
        self.dram[..image.len()].copy_from_slice(image);
    }

    /// Places `data` at an arbitrary physical address, for an ELF segment.
    pub fn load_at(&mut self, addr: u64, data: &[u8]) -> Result<(), Exception> {
        let i = self
            .index(addr, data.len() as u64)
            .ok_or(Exception::StoreAccessFault(addr))?;
        self.dram[i..i + data.len()].copy_from_slice(data);
        Ok(())
    }

    /// Clears `len` bytes, for the .bss tail of a segment.
    pub fn zero(&mut self, addr: u64, len: u64) -> Result<(), Exception> {
        let i = self
            .index(addr, len)
            .ok_or(Exception::StoreAccessFault(addr))?;
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
    pub fn read(&self, addr: u64, size: u64) -> Result<u64, Exception> {
        let i = self
            .index(addr, size)
            .ok_or(Exception::LoadAccessFault(addr))?;
        let mut v = 0u64;
        for b in (0..size as usize).rev() {
            v = (v << 8) | self.dram[i + b] as u64;
        }
        Ok(v)
    }

    pub fn write(&mut self, addr: u64, size: u64, value: u64) -> Result<(), Exception> {
        let i = self
            .index(addr, size)
            .ok_or(Exception::StoreAccessFault(addr))?;
        for b in 0..size as usize {
            self.dram[i + b] = (value >> (8 * b)) as u8;
        }
        if self.tohost == Some(addr) && value != 0 {
            self.tohost_value = Some(value);
        }
        Ok(())
    }
}
