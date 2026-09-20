//! Reports the first unexpected trap in a test binary, with enough context to
//! find the instruction that caused it.
use nanoemu::cpu::{Cpu, Xlen};
use nanoemu::{csr, elf::Elf, trap::Exception};

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let elf = Elf::parse(&bytes).unwrap();
    let mut cpu = Cpu::new(64 << 20, Xlen::Rv64);
    cpu.load_elf(&elf).unwrap();
    for _ in 0..2_000_000 {
        let pc = cpu.pc;
        if let Err(e) = cpu.step() {
            let half = cpu.mem.read(pc, 2).unwrap_or(0);
            let word = cpu.mem.read(pc, 4).unwrap_or(0);
            println!(
                "trap at pc={pc:08x} cause={} tval={:x} half={half:04x} word={word:08x}\n  {e:?}",
                cpu.csrs.read(csr::MCAUSE),
                cpu.csrs.read(csr::MTVAL)
            );
            if !matches!(e, Exception::EnvironmentCall) {
                return;
            }
        }
        if cpu.mem.tohost_value.is_some() {
            println!("terminated, tohost={:?}", cpu.mem.tohost_value);
            return;
        }
    }
    println!("step limit");
}
