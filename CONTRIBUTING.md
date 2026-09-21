# Contributing

## Getting a working checkout

```
git clone https://github.com/GaragaKarthikeya/nanoriscv.git
cd nanoriscv
cd emu && cargo test        # hand-written tests: no toolchain, no dependencies
```

The emulator has **no dependencies**, so the hand-written suite runs on a bare
Rust toolchain. The official riscv-tests suite needs a cross-compiler:

```
make docs        # fetch the specs, riscv-opcodes and riscv-tests (once)
make isa         # compile the 243 test binaries
make test-isa    # run them, with per-suite output
```

`scripts/build-tests.sh` prefers `riscv64-unknown-elf-gcc` and falls back to
`clang-18` plus an LLD, so a machine without a RISC-V GNU toolchain still
works. CI takes the fallback path.

Note that `emu/tests/riscv_tests.rs` **passes when the binaries are absent**,
so that a fresh clone can run `cargo test` without a toolchain. A green
`cargo test` therefore does not by itself mean the official suite ran — check
for the per-suite `N tests passed` lines.

## Before opening a pull request

```
cd emu
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --release
```

CI runs exactly these, plus the full 236-binary suite.

## Which specification to cite

Cite the **ratified 2024-04-11** PDFs, never the nightly — section numbers in
a nightly move. When a hand-written table and `docs/opcodes/` disagree about
an encoding, `docs/opcodes/` wins: it is generated from the same source the
spec listings are.

## What a change should come with

This emulator is the *golden model* — the definition of correct behaviour that
the RTL core is verified against. That sets the bar: a behavioural change is a
claim about what the hardware must do.

So every fix to a spec corner comes with a test that fails without it. The
README's "Notes on things that are easy to get wrong" list is the format to
follow — each entry there exists because the wrong version was plausible and
passed everything else.

A test suite that has never failed has not been shown to be *capable* of
failing. After a change to how results are collected, break something on
purpose and confirm the suite notices.

## Style

`Cpu::step()` is the whole contract: fetch one instruction, execute it, and
either retire it or take a trap. Nothing is pipelined and nothing is cached,
because this model's only job is to be obviously right. Optimisations that
cost clarity are the wrong trade here — the RTL core is where speed lives.

Keep the dependency count at zero unless there is a strong reason not to.
