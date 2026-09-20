# nanoriscv

A RISC-V processor built from scratch, twice: once as a Rust instruction-set
simulator, then as a SystemVerilog core verified against it.

The simulator comes first and is not a throwaway. It is the *golden model* — the
definition of correct behaviour that the hardware is checked against. A core
without one is verified by staring at waveforms; a core with one is verified by
running a program on both and stopping at the first instruction where they
disagree.

## Status

**Milestones 1 and 2 are done: RV32IM passes the official riscv-tests suite.**

All 50 upstream `rv32ui` and `rv32um` tests pass, alongside 21 hand-written
tests covering corners the suite happens to miss.

```
make docs        # fetch specs, encodings and riscv-tests (once, after cloning)
make isa         # compile the official test binaries
make test        # everything: hand-written tests + the official suite
make test-isa    # just the official suite, with per-suite output
make build       # release build of the nanoemu binary
make run IMG=x   # run an ELF32 executable, or a flat binary at 0x8000_0000
```

### Building the official suite without a GNU toolchain

Upstream riscv-tests expects `riscv64-unknown-elf-gcc`. This machine has no
RISC-V GNU toolchain and installing one needs root, so `scripts/build-tests.sh`
compiles the same untouched sources with `clang-18`'s RISC-V backend and links
them with the LLD that already ships inside the Rust toolchain
(`rust-lld`). Only the driver differs; the test sources do not.

## Layout

```
emu/         the Rust simulator (crate: nanoemu)
  src/decode.rs   instruction field and immediate extraction
  src/cpu.rs      the hart: fetch, execute, trap, and the run loop
  src/csr.rs      machine-mode control and status registers
  src/memory.rs   flat little-endian DRAM at 0x8000_0000
  src/elf.rs      ELF32 loader and symbol lookup
  src/trap.rs     exception causes
  tests/rv32im.rs      hand-written tests, one per spec corner
  tests/riscv_tests.rs the official rv32ui / rv32um suites
rtl/         the SystemVerilog core (not started — see rtl/README.md)
docs/        specs, encodings, riscv-tests (gitignored; see docs/README.md)
build/       compiled test binaries (gitignored)
scripts/     fetch-docs.sh, build-tests.sh
```

## How the emulator is organised

`Cpu::step()` is the whole contract: it fetches one instruction, executes it,
and either retires it or takes a trap. Nothing is pipelined and nothing is
cached, because this model's only job is to be obviously right. When the RTL
core arrives, `step()` is the unit the testbench compares against — after each
one, the core's retire-stage state must match `pc`, `x1..x31` and the machine
CSRs exactly.

Memory lives at `0x8000_0000`, the convention SiFive boards and the riscv-tests
linker script use, so upstream test binaries will load without relinking.

## Roadmap

1. ~~**RV32IM emulator**~~ — base integer set, M extension, Zicsr, traps. **Done.**
2. ~~**riscv-tests**~~ — the official `rv32ui` and `rv32um` suites, via an ELF32
   loader and the `tohost` handshake. **Done: 50/50.**
3. **RV64 + supervisor mode** — widen to 64-bit, add S-mode and Sv39 paging, the
   CLINT timer, and a UART. Enough to boot Linux.
4. **RTL core** — a 5-stage RV32I pipeline in SystemVerilog, verified by
   lockstep diff against milestone 1.
5. **FPGA** — put it on the ZCU104 using the parent repo's board support.

Milestones 2 and 4 are independent; 4 only needs 1.

## Which spec to trust

Cite the **ratified 2024-04-11** PDFs, not the nightly — section numbers in a
nightly move. When a hand-written table and `docs/opcodes/` disagree about an
encoding, `docs/opcodes/` wins; it is generated from the same source the spec
listings are.

## Notes on things that are easy to get wrong

These all have tests, because each one was worth a test:

- `AUIPC` adds to the address of the `AUIPC` itself, not the next instruction.
- `JALR` clears bit 0 of the computed target after the add, not before.
- Shift amounts are the low 5 bits only — a shift by 33 is a shift by 1.
- `BLT` and `BLTU` must genuinely differ; sharing a comparison hides the bug.
- `DIV` by zero yields all-ones and `REM` by zero yields the dividend. Signed
  overflow (`INT_MIN / -1`) is defined too. None of these trap.
- A CSR set/clear with `rs1 == x0` must not write the CSR at all.
- `mepc` holds the address of the *faulting* instruction.
- In `Elf32_Shdr`, `sh_link` is at offset 24. Putting it at 40 yields a symbol
  table full of plausible-looking addresses under garbage names.

## How results get out of a test

A riscv-tests binary reports through a `tohost` symbol rather than a return
code. `RVTEST_PASS` sets `gp` to 1 and executes `ecall`; the test's own trap
handler stores `gp` to `tohost`. On failure `gp` is `(n << 1) | 1`, where `n`
identifies the failing case. So the emulator watches for a store to the address
of that symbol: bit 0 set means terminate, and the rest is the status.

This is why traps are not stopping conditions in `Cpu::run()` — these tests
install a handler and trap deliberately as part of the test. The run ends when
the payload reports, or when a trap is taken with `mtvec` still unset.

## Checking the harness itself

A test suite that passes on the first run has not yet been shown to be capable
of failing. After this suite first went green, `SRA` was changed to a logical
shift and the suite re-run: `rv32ui-p-sra` failed and nothing else did. Worth
repeating after any change to how results are collected.
