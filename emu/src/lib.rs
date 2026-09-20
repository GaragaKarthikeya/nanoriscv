//! nanoemu — the reference RISC-V simulator for nanoriscv.
//!
//! Its job is to be obviously correct rather than fast: it is the golden model
//! the SystemVerilog core is checked against, instruction by instruction.

pub mod cpu;
pub mod csr;
pub mod decode;
pub mod memory;
pub mod trap;

pub use cpu::Cpu;
pub use memory::DRAM_BASE;
pub use trap::Exception;
