# nanoriscv

A RISC-V processor built from scratch, twice: once as a Rust instruction-set
simulator, then as a SystemVerilog core verified against it.

The simulator is RV32 **and** RV64 in one implementation. That is not
indecision: the RTL core will be RV32 while the software side is heading for
RV64 and a Linux boot, and a single model keeps those two from drifting apart.

The simulator comes first and is not a throwaway. It is the *golden model* — the
definition of correct behaviour that the hardware is checked against. A core
without one is verified by staring at waveforms; a core with one is verified by
running a program on both and stopping at the first instruction where they
disagree.

## Status

**RV32IMAC and RV64IMAC pass the official riscv-tests suite: 148/148.**

| Suite | Tests |
| --- | --- |
| `rv32ui` / `rv32um` / `rv32ua` / `rv32uc` | 42 + 8 + 10 + 1 |
| `rv64ui` / `rv64um` / `rv64ua` / `rv64uc` | 54 + 13 + 19 + 1 |
| hand-written (`rv32im.rs`, `rv64.rs`, `ac.rs`) | 21 + 13 + 18 |

Five upstream tests are skipped by name in `tests/riscv_tests.rs`: the
`amocas` ones are **Zacas**, a separate extension from A, which upstream
happens to file in the `ua` directory.

Still missing before the emulator is "complete" in the RV64GC sense: F and D
(floating point), supervisor mode with Sv39 paging, and the CLINT/PLIC/UART
devices a Linux boot needs.

```
make docs        # fetch specs, encodings and riscv-tests (once, after cloning)
make isa         # compile the official test binaries
make test        # everything: hand-written tests + the official suite
make test-isa    # just the official suite, with per-suite output
make build       # release build of the nanoemu binary
make run IMG=x   # run an ELF executable, or a flat binary at 0x8000_0000
```

### Building the official suite

`scripts/build-tests.sh` prefers `riscv64-unknown-elf-gcc`, which is what
upstream expects, and falls back to `clang-18` plus an LLD (the system one, or
the `rust-lld` inside the Rust toolchain) on a machine without a RISC-V GNU
toolchain. The test sources are untouched either way; only the driver differs.

Note `-march=rv32im_zicsr_zifencei`. Since GCC 12 those two are separate
extensions, and GCC 14 rejects a `csrr` whose extension is not named. Clang is
lenient about it, which is exactly the kind of difference that makes a build
work on one machine and not the next.

## Layout

```
emu/         the Rust simulator (crate: nanoemu)
  src/decode.rs   instruction field and immediate extraction
  src/cpu.rs      the hart: fetch, execute, trap, and the run loop
  src/csr.rs      machine-mode control and status registers
  src/memory.rs   flat little-endian DRAM at 0x8000_0000
  src/compress.rs the C extension, expanded to base instructions at fetch
  src/elf.rs      ELF32/ELF64 loader and symbol lookup
  src/trap.rs     exception causes
  tests/rv32im.rs      hand-written RV32 tests, one per spec corner
  tests/rv64.rs        RV64 behaviour and the dual-width seams
  tests/ac.rs          the A and C extensions
  examples/diag.rs     prints the first unexpected trap in a test binary
  tests/riscv_tests.rs the official rv32/rv64 ui and um suites
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

### C is expanded, not executed

Every RVC instruction is defined by the spec as an alias for exactly one base
instruction, so `src/compress.rs` expands each 16-bit word at fetch and the
execute path never learns that C exists. Nothing downstream grows a second
case, and the RTL core can use the same trick.

Two consequences worth stating. Instructions now need only 2-byte alignment,
so the fetch check is `pc & 1` rather than `pc & 3` — requiring 4 would reject
perfectly legal jump targets. And the RVC immediate fields are scrambled
rather than contiguous, which looks gratuitous on paper but keeps each bit in
a fixed position relative to the 32-bit encodings, so hardware routes wires
instead of multiplexing.

### One ALU, two widths

RV64's `*W` instructions have exactly RV32 semantics plus a sign-extension.
So the ALU is written once and parameterised by a `width` of 32 or 64, and
`ADDW`, `SLLW`, `SRLW`, `SRAW`, `MULW` and `DIVW` fall out of the RV32 cases
for free rather than being a second implementation to keep in step.

Registers hold the **zero-extended** XLEN-wide value. Sign-extended canonical
form would have been marginally simpler inside the ALU, but zero-extension
means an RV32 register file reads back exactly as the 32-bit RTL core's will,
which is what the lockstep diff needs.

Memory lives at `0x8000_0000`, the convention SiFive boards and the riscv-tests
linker script use, so upstream test binaries will load without relinking.

## Roadmap

1. ~~**RV32IM emulator**~~ — base integer set, M extension, Zicsr, traps. **Done.**
2. ~~**riscv-tests**~~ — the official `rv32ui` and `rv32um` suites, via an ELF32
   loader and the `tohost` handshake. **Done: 50/50.**
3. **Complete the emulator** — in progress.
   - ~~RV64IM: widen to 64-bit~~ **Done, `rv64ui`/`rv64um` pass.**
   - A, C and F/D extensions, for a full RV64GC.
   - Supervisor mode, Sv39 paging, CLINT, PLIC and a UART — enough to boot
     Linux, with QEMU as a second reference when the two disagree.
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
  table full of plausible-looking addresses under garbage names. (In
  `Elf64_Shdr` it really is at 40, and ELF64 reorders the program header too.)
- `MULHSU` is signed `rs1` times *unsigned* `rs2`. Taking the unsigned operand
  from `rs1` passes every other multiply test and fails only this one.
- `SRAI` is selected by funct6 `0b010000`, which sits in `imm[11:6]` — that is
  `0x400`, not funct7's `0x20` shifted up by six. Getting it wrong produces a
  plausible instruction that decodes as illegal.
- The same halfword means different things at different widths: `0x2505` is
  `C.ADDIW` on RV64 and `C.JAL` on RV32. A decompressor that ignores XLEN is
  silently wrong rather than visibly broken.
- An all-zero halfword must be illegal, so that execution running into a
  zeroed page traps instead of wandering.
- An AMO returns the value that was in memory *beforehand*, and unlike
  ordinary loads and stores it must be naturally aligned.
- On RV64, `LUI` sign-extends. Every address at or above `0x8000_0000` has bit
  31 set, so `lui` cannot be used to build a DRAM address — it lands at the top
  of the address space instead.

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
