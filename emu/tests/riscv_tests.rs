//! Runs the official riscv-tests rv32ui and rv32um suites.
//!
//! These are the same binaries every other RISC-V implementation is measured
//! against, which makes them a far stronger signal than the hand-written tests
//! in `rv32im.rs` -- those cover corners this suite happens to miss, so both
//! are kept.
//!
//! The binaries are not committed. Build them with:
//!
//!     scripts/fetch-docs.sh && scripts/build-tests.sh
//!
//! If they are absent this test reports that and passes, so a fresh clone can
//! still run `cargo test` without a RISC-V toolchain set up.

use std::path::{Path, PathBuf};

use nanoemu::cpu::{Cpu, Exit, Xlen};
use nanoemu::elf::Elf;

/// 64 MiB, matching the CLI, so a test that runs here runs there.
const MEM: usize = 64 * 1024 * 1024;
/// Generous enough for every test in the suite; only a hang exceeds it.
const MAX_STEPS: u64 = 2_000_000;

fn suite_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is emu/, so the build output is one level up.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../build/tests")
}

fn binaries() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(suite_dir()) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "elf"))
        .collect();
    v.sort();
    v
}

fn run_one(path: &Path) -> Exit {
    let bytes = std::fs::read(path).expect("test binary is readable");
    let elf = Elf::parse(&bytes).expect("test binary is a valid RV32 ELF");
    // The width comes from the ELF class, so one runner covers both suites.
    let mut cpu = Cpu::new(MEM, Xlen::Rv64);
    cpu.load_elf(&elf).expect("test binary fits in memory");
    assert!(
        cpu.mem.tohost.is_some(),
        "{} exports no tohost symbol, so its result cannot be read",
        path.display()
    );
    cpu.run(MAX_STEPS)
}

fn run_suite(prefix: &str) {
    let binaries: Vec<_> = binaries()
        .into_iter()
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(prefix))
        .collect();

    if binaries.is_empty() {
        eprintln!(
            "skipping {prefix}: no binaries in {}",
            suite_dir().display()
        );
        eprintln!("build them with scripts/fetch-docs.sh && scripts/build-tests.sh");
        return;
    }

    // Every test runs before anything is reported, so one failure does not
    // hide the others -- knowing whether one test or thirty broke is the
    // difference between a typo and a wrong idea.
    let mut failures = Vec::new();
    for path in &binaries {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        match run_one(path) {
            Exit::Pass => {}
            other => failures.push(format!("{name}: {other:?}")),
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} {prefix} tests failed:\n  {}",
        failures.len(),
        binaries.len(),
        failures.join("\n  ")
    );
    eprintln!("{prefix}: {} tests passed", binaries.len());
}

#[test]
fn rv32ui_base_integer_suite() {
    run_suite("rv32ui-p-");
}

#[test]
fn rv32um_mul_div_suite() {
    run_suite("rv32um-p-");
}

#[test]
fn rv64ui_base_integer_suite() {
    run_suite("rv64ui-p-");
}

#[test]
fn rv64um_mul_div_suite() {
    run_suite("rv64um-p-");
}
