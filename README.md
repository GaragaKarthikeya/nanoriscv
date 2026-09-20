# nanoriscv

A RISC-V processor built from scratch, twice: once as a Rust instruction-set
simulator, then as a SystemVerilog core verified against it.

The simulator comes first and is not a throwaway. It is the *golden model* — the
definition of correct behaviour that the hardware is checked against. A core
without one is verified by staring at waveforms; a core with one is verified by
running a program on both and stopping at the first instruction where they
disagree.

## Status

**Milestone 1 is done: RV32IM, machine mode, 21 passing tests.**

The emulator implements the RV32I base integer set, the M extension, the Zicsr
CSR instructions, and machine-mode trap entry and return.

```
make test          # run the test suite
make docs          # fetch the specs and encoding tables (once, after cloning)
make build         # release build of the nanoemu binary
make run IMG=x.bin # run a flat binary loaded at 0x8000_0000
```

## Layout

```
emu/         the Rust simulator (crate: nanoemu)
  src/decode.rs   instruction field and immediate extraction
  src/cpu.rs      the hart: fetch, execute, trap
  src/csr.rs      machine-mode control and status registers
  src/memory.rs   flat little-endian DRAM at 0x8000_0000
  src/trap.rs     exception causes
  tests/          behavioural tests, one per spec corner
rtl/         the SystemVerilog core (not started — see rtl/README.md)
docs/        specs and encoding tables (gitignored; see docs/README.md)
scripts/     fetch-docs.sh
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
2. **riscv-tests** — run the official `rv32ui` and `rv32um` suites. This replaces
   home-grown confidence with the same bar every other implementation clears.
   Needs an ELF loader and the `tohost` handshake (the memory module already has
   the hook).
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
