//! Runs an RV32 payload: either an ELF32 executable, or a flat binary loaded
//! at DRAM_BASE if the file has no ELF magic.
//!
//!     nanoemu <image> [--rv32|--rv64] [--trace] [--max-steps N] [--quiet]
//!
//! An ELF selects its own width from its class; --rv32/--rv64 only apply to a
//! flat image, and default to RV64.
//!
//! Output written to the UART at 0x1000_0000 is echoed to stdout.
//!
//! The exit code is 0 on success, 1 on a test failure or unhandled trap, and
//! 3 if the step budget ran out.

use std::process::ExitCode;

use nanoemu::cpu::{Cpu, Exit, Xlen};
use nanoemu::decode::REG_NAMES;
use nanoemu::elf::Elf;

const MEM_SIZE: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_STEPS: u64 = 100_000_000;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut trace = false;
    let mut quiet = false;
    let mut xlen = Xlen::Rv64;
    let mut max_steps = DEFAULT_MAX_STEPS;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--trace" => trace = true,
            "--quiet" => quiet = true,
            "--rv32" => xlen = Xlen::Rv32,
            "--rv64" => xlen = Xlen::Rv64,
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
        eprintln!("usage: nanoemu <image> [--rv32|--rv64] [--trace] [--max-steps N] [--quiet]");
        return ExitCode::from(2);
    };

    let image = match std::fs::read(&path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };

    // A flat image has no class to read, so it needs the width stated; an ELF
    // overrides this from its own header.
    let mut cpu = Cpu::new(MEM_SIZE, xlen);
    // Anything the guest writes to the UART goes to our stdout, so a program
    // with a console driver prints where you would expect.
    cpu.mem.uart.echo = true;
    // Anything without the ELF magic is treated as a flat image at DRAM_BASE.
    if image.starts_with(b"\x7fELF") {
        match Elf::parse(&image) {
            Ok(elf) => {
                if let Err(e) = cpu.load_elf(&elf) {
                    eprintln!("cannot load {path}: segment does not fit in memory ({e:?})");
                    return ExitCode::from(2);
                }
            }
            Err(e) => {
                eprintln!("cannot load {path}: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        cpu.mem.load(&image);
    }

    if trace {
        // Tracing needs per-step control, so the run loop is inlined here.
        let mut outcome = Exit::StepLimit;
        for _ in 0..max_steps {
            // Field width follows XLEN, so an RV64 trace is not silently
            // truncated to its low word.
            let w = (cpu.xlen.bits() / 4) as usize;
            eprintln!(
                "{:0w$x}  a0={:0w$x} ra={:0w$x}",
                cpu.pc, cpu.regs[10], cpu.regs[1]
            );
            outcome = cpu.run(1);
            if outcome != Exit::StepLimit {
                break;
            }
        }
        return report(&cpu, outcome, quiet);
    }

    let outcome = cpu.run(max_steps);
    report(&cpu, outcome, quiet)
}

fn report(cpu: &Cpu, outcome: Exit, quiet: bool) -> ExitCode {
    let code = match outcome {
        Exit::Pass | Exit::Ecall => 0,
        Exit::Fail(n) => {
            eprintln!("FAIL: test case {n}");
            1
        }
        Exit::UnhandledTrap(e) => {
            eprintln!("unhandled trap with mtvec unset: {e:?}");
            1
        }
        Exit::StepLimit => {
            eprintln!("step limit reached without terminating");
            3
        }
    };

    if quiet {
        return ExitCode::from(code);
    }

    eprintln!(
        "retired {} instructions",
        cpu.csrs.read(nanoemu::csr::INSTRET)
    );
    let w = (cpu.xlen.bits() / 4) as usize;
    let per_row = if w > 8 { 2 } else { 4 };
    for (i, name) in REG_NAMES.iter().enumerate() {
        let end = if i % per_row == per_row - 1 {
            "\n"
        } else {
            "  "
        };
        eprint!("{name:>4}={:0w$x}{end}", cpu.regs[i]);
    }
    ExitCode::from(code)
}
