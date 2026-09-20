//! Runs a RISC-V payload.
//!
//!     nanoemu <image> [--rv32|--rv64] [--trace] [--max-steps N] [--quiet]
//!     nanoemu --kernel <Image> --dtb <file.dtb> [--mem MiB]
//!
//! A bare image is an ELF executable, or a flat binary loaded at DRAM_BASE if
//! the file has no ELF magic. A kernel is a raw `Image`, entered in supervisor
//! mode with the emulator standing in for machine-mode firmware.
//!
//! Output written to the UART at 0x1000_0000 is echoed to stdout.

use std::process::ExitCode;

use nanoemu::cpu::{Cpu, Exit, Xlen};
use nanoemu::decode::REG_NAMES;
use nanoemu::elf::Elf;
use nanoemu::DRAM_BASE;

/// Where a RISC-V kernel expects to be loaded: 2 MiB into RAM, leaving room
/// below it for the firmware that normally lives there.
const KERNEL_OFFSET: u64 = 0x20_0000;
const DEFAULT_MAX_STEPS: u64 = 100_000_000;

struct Options {
    image: Option<String>,
    kernel: Option<String>,
    dtb: Option<String>,
    mem_mib: usize,
    xlen: Xlen,
    trace: bool,
    quiet: bool,
    max_steps: u64,
}

fn usage() -> ExitCode {
    eprintln!("usage: nanoemu <image> [--rv32|--rv64] [--trace] [--max-steps N] [--quiet]");
    eprintln!("       nanoemu --kernel <Image> --dtb <file.dtb> [--mem MiB] [--max-steps N]");
    ExitCode::from(2)
}

fn parse() -> Result<Options, ExitCode> {
    let mut o = Options {
        image: None,
        kernel: None,
        dtb: None,
        mem_mib: 0,
        xlen: Xlen::Rv64,
        trace: false,
        quiet: false,
        max_steps: DEFAULT_MAX_STEPS,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        let r: Result<(), String> = match arg.as_str() {
            "--trace" => {
                o.trace = true;
                Ok(())
            }
            "--quiet" => {
                o.quiet = true;
                Ok(())
            }
            "--rv32" => {
                o.xlen = Xlen::Rv32;
                Ok(())
            }
            "--rv64" => {
                o.xlen = Xlen::Rv64;
                Ok(())
            }
            "--kernel" => value().map(|v| o.kernel = Some(v)),
            "--dtb" => value().map(|v| o.dtb = Some(v)),
            "--mem" => value().and_then(|v| {
                v.parse()
                    .map(|m| o.mem_mib = m)
                    .map_err(|_| "--mem wants a number of MiB".into())
            }),
            "--max-steps" => value().and_then(|v| {
                v.parse()
                    .map(|m| o.max_steps = m)
                    .map_err(|_| "--max-steps wants a number".into())
            }),
            other if other.starts_with("--") => Err(format!("unknown flag {other}")),
            other => {
                o.image = Some(other.to_string());
                Ok(())
            }
        };
        if let Err(e) = r {
            eprintln!("{e}");
            return Err(usage());
        }
    }
    if o.mem_mib == 0 {
        // A kernel needs considerably more room than a test binary.
        o.mem_mib = if o.kernel.is_some() { 256 } else { 64 };
    }
    Ok(o)
}

fn read(path: &str) -> Result<Vec<u8>, ExitCode> {
    std::fs::read(path).map_err(|e| {
        eprintln!("cannot read {path}: {e}");
        ExitCode::from(2)
    })
}

fn main() -> ExitCode {
    let o = match parse() {
        Ok(o) => o,
        Err(code) => return code,
    };

    let mem = o.mem_mib * 1024 * 1024;
    let mut cpu = Cpu::new(mem, o.xlen);
    // Anything the guest writes to the UART goes to our stdout, so a program
    // with a console driver prints where you would expect.
    cpu.mem.uart.echo = true;

    let outcome = if let Some(kernel) = &o.kernel {
        match boot_kernel(&mut cpu, kernel, o.dtb.as_deref(), mem as u64) {
            Ok(()) => {}
            Err(code) => return code,
        }
        run(&mut cpu, &o)
    } else {
        let Some(path) = &o.image else {
            return usage();
        };
        let image = match read(path) {
            Ok(i) => i,
            Err(code) => return code,
        };
        // Anything without the ELF magic is treated as a flat image.
        if image.starts_with(b"\x7fELF") {
            match Elf::parse(&image) {
                Ok(elf) => {
                    if let Err(e) = cpu.load_elf(&elf) {
                        eprintln!("cannot load {path}: it does not fit in memory ({e:?})");
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
        run(&mut cpu, &o)
    };

    report(&cpu, outcome, o.quiet || o.kernel.is_some())
}

/// Places a kernel and its device tree, then enters supervisor mode the way
/// firmware would.
fn boot_kernel(cpu: &mut Cpu, kernel: &str, dtb: Option<&str>, mem: u64) -> Result<(), ExitCode> {
    let Some(dtb_path) = dtb else {
        eprintln!("--kernel needs --dtb: the kernel is handed a device tree, not a guess");
        return Err(usage());
    };
    let image = read(kernel)?;
    let tree = read(dtb_path)?;

    let kernel_addr = DRAM_BASE + KERNEL_OFFSET;
    // The device tree goes near the top of RAM, clear of the kernel and of
    // whatever it decides to allocate early.
    let dtb_addr = DRAM_BASE + mem - 0x20_0000;

    if cpu.mem.load_at(kernel_addr, &image).is_err() {
        eprintln!("kernel does not fit in {} MiB of RAM", mem / (1 << 20));
        return Err(ExitCode::from(2));
    }
    if cpu.mem.load_at(dtb_addr, &tree).is_err() {
        eprintln!("device tree does not fit in memory");
        return Err(ExitCode::from(2));
    }

    eprintln!(
        "booting {} at {kernel_addr:#x}, dtb at {dtb_addr:#x}, {} MiB RAM",
        kernel,
        mem / (1 << 20)
    );
    cpu.boot_supervisor(kernel_addr, 0, dtb_addr);
    Ok(())
}

fn run(cpu: &mut Cpu, o: &Options) -> Exit {
    if !o.trace {
        return cpu.run(o.max_steps);
    }
    // Tracing needs per-step control, so the run loop is unrolled here.
    let width = (cpu.xlen.bits() / 4) as usize;
    for _ in 0..o.max_steps {
        eprintln!(
            "{:0width$x}  a0={:0width$x} ra={:0width$x}",
            cpu.pc, cpu.regs[10], cpu.regs[1]
        );
        let outcome = cpu.run(1);
        if outcome != Exit::StepLimit {
            return outcome;
        }
    }
    Exit::StepLimit
}

fn report(cpu: &Cpu, outcome: Exit, quiet: bool) -> ExitCode {
    let code = match outcome {
        Exit::Pass | Exit::Ecall | Exit::Shutdown => 0,
        Exit::Fail(n) => {
            eprintln!("FAIL: test case {n}");
            1
        }
        Exit::UnhandledTrap(e) => {
            eprintln!(
                "unhandled trap, no handler installed: {e:?}\n  pc={:#x} mode={:?} satp={:#x}",
                cpu.pc,
                cpu.priv_mode,
                cpu.csrs.read(nanoemu::csr::SATP)
            );
            1
        }
        Exit::StepLimit => {
            eprintln!(
                "step limit reached without terminating\n  pc={:#x} mode={:?} satp={:#x} mtime={} sepc={:#x} scause={:#x}",
                cpu.pc,
                cpu.priv_mode,
                cpu.csrs.read(nanoemu::csr::SATP),
                cpu.mem.clint.mtime,
                cpu.csrs.read(nanoemu::csr::SEPC),
                cpu.csrs.read(nanoemu::csr::SCAUSE),
            );
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
