//! Traces the traps a test binary takes, to find where it goes wrong.
//!
//! A riscv-tests binary traps on purpose -- the p-environment prologue probes
//! optional CSRs and relies on mtvec absorbing the illegal-instruction trap --
//! so stopping at the first one says nothing. This prints them all and reports
//! how the test finished.
use nanoemu::cpu::{Cpu, Exit, Xlen};
use nanoemu::{csr, elf::Elf};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: diag <elf> [max-traps]");
    let max_traps: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(40);

    let bytes = std::fs::read(&path).unwrap();
    let elf = Elf::parse(&bytes).unwrap();
    let mut cpu = Cpu::new(64 << 20, Xlen::Rv64);
    cpu.load_elf(&elf).unwrap();

    let mut shown = 0;
    for _ in 0..2_000_000 {
        let pc = cpu.pc;
        let before = cpu.priv_mode;
        let r = cpu.step();
        if let Err(e) = r {
            if shown < max_traps {
                shown += 1;
                println!(
                    "{shown:3}. pc={pc:08x} {before:?} -> {:?}  cause={} tval={:x} gp={}\n     {e:?}",
                    cpu.priv_mode,
                    cpu.csrs.read(csr::MCAUSE),
                    cpu.csrs.read(csr::MTVAL),
                    cpu.regs[3],
                );
            }
        }
        if let Some(v) = cpu.mem.tohost_value {
            let exit = if v & 1 == 1 {
                match v >> 1 {
                    0 => Exit::Pass,
                    n => Exit::Fail(n),
                }
            } else {
                Exit::Ecall
            };
            println!("finished: {exit:?}  (tohost={v:#x}, {shown} traps shown)");
            return;
        }
    }
    println!("step limit, {shown} traps shown");
}
