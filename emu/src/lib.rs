//! nanoemu — the reference RISC-V simulator for nanoriscv.
//!
//! Its job is to be obviously correct rather than fast: it is the golden model
//! the SystemVerilog core is checked against, instruction by instruction.

pub mod compress;
pub mod cpu;
pub mod csr;
pub mod decode;
pub mod elf;
pub mod memory;
pub mod mmu;
pub mod plic;
pub mod sbi;
pub mod trap;
pub mod uart;

pub use cpu::{Cpu, Exit, Xlen};
pub use elf::Elf;
pub use memory::DRAM_BASE;
pub use trap::{Exception, Priv};
