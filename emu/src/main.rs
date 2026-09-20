//! Runs a flat RV32 binary loaded at DRAM_BASE.
//!
//!     nanoemu <image.bin> [--trace] [--max-steps N]
//!
//! Execution stops at ECALL, at an unhandled trap, or after --max-steps.

use std::process::ExitCode;

use nanoemu::cpu::Cpu;
use nanoemu::decode::REG_NAMES;
use nanoemu::trap::Exception;

const MEM_SIZE: usize = 64 * 1024 * 1024;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut trace = false;
    let mut max_steps = u64::MAX;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--trace" => trace = true,
            "--max-steps" => {
                max_steps = match args.next().and_then(|v| v.parse().ok()) {
                    Some(n) => n,
                    None => {
                        eprintln!("--max-steps needs a number");
                        return ExitCode::from(2);
                    }
                }
            }
            other => path = Some(other.to_string()),
        }
    }

    let Some(path) = path else {
        eprintln!("usage: nanoemu <image.bin> [--trace] [--max-steps N]");
        return ExitCode::from(2);
    };

    let image = match std::fs::read(&path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };

    let mut cpu = Cpu::new(MEM_SIZE);
    cpu.mem.load(&image);

    let mut steps = 0;
    let exit = loop {
        if steps >= max_steps {
            eprintln!("stopped after {steps} steps without terminating");
            break 3;
        }
        if trace {
            let pc = cpu.pc;
            eprintln!("{:08x}: a0={:08x} ra={:08x}", pc, cpu.regs[10], cpu.regs[1]);
        }
        steps += 1;
        match cpu.step() {
            Ok(()) => {}
            Err(Exception::EnvironmentCall) => break 0,
            Err(e) => {
                eprintln!(
                    "unhandled trap at pc={:08x}: {e:?}",
                    cpu.csrs.read(nanoemu::csr::MEPC)
                );
                break 1;
            }
        }
    };

    eprintln!("retired {steps} instructions");
    for (i, name) in REG_NAMES.iter().enumerate() {
        eprint!(
            "{name:>4}={:08x}{}",
            cpu.regs[i],
            if i % 4 == 3 { "\n" } else { " " }
        );
    }
    ExitCode::from(exit)
}
