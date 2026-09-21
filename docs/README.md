# Reference material

None of this is committed — run `scripts/fetch-docs.sh` to pull it.

## Specifications — `docs/spec/`

| File | What it is | Use it for |
| --- | --- | --- |
| `riscv-unprivileged-20240411.pdf` | Volume I, **ratified** 2024-04-11 | Instruction semantics. Cite section numbers from here. |
| `riscv-privileged-20240411.pdf` | Volume II, **ratified** 2024-04-11 | CSRs, traps, privilege modes, PMP. |
| `riscv-spec-latest.pdf` | Both volumes, nightly build off `main` | Extensions ratified after the frozen release. Draft — do not cite its section numbers. |
| `norm-rules.json` | Normative rules extracted from the nightly | Machine-readable checklist of "the hart MUST ..." statements. |

The sections that matter, now that the emulator is RV64GC with privilege and
paging. Section numbers are given where they have been checked against the
ratified PDFs; the rest are named by chapter, because a number copied from the
nightly is exactly the mistake this file exists to prevent.

Base and integer arithmetic:

- **Vol I §2.1–2.6** — RV32I base: the 47 instructions, the six formats, and the
  immediate-encoding rationale that explains why the bit scrambling in
  `emu/src/decode.rs` is worth it.
- **Vol I §4.2** — Zicsr. Note the read-before-write ordering and the rule that
  a set/clear with `rs1 == x0` must not write the CSR.
- **Vol I §4.5** — M extension. Division by zero and signed overflow have
  *defined* results; they do not trap.
- **Vol I, "RV64I Base Integer Instruction Set"** — the `*W` instructions and
  the shift-amount widening. The whole of `emu/src/cpu.rs`'s dual-width ALU is
  this chapter.

Extensions:

- **Vol I, "A" Standard Extension** — the AMOs return the *prior* memory value
  and must be naturally aligned, unlike ordinary loads and stores. LR/SC
  reservation rules.
- **Vol I, "C" Standard Extension** — each RVC instruction is defined as an
  alias for exactly one base instruction, which is what licenses the
  expand-at-fetch approach in `emu/src/compress.rs`. The immediate-field tables
  are the part to read closely; they are scrambled on purpose.
- **Vol I, "F" and "D" Standard Extensions** — NaN-boxing of single-precision
  values in `f` registers, the canonical NaN, the five rounding modes, and the
  `fcsr` accumulated flags. Tininess is judged *after* rounding.

Privilege, traps and memory:

- **Vol II §3.1** — `mstatus`, `mtvec`, `mepc`, `mcause`, `mtval`.
- **Vol II §3.3.1** — trap entry and `mret`.
- **Vol II, machine-mode delegation** — `medeleg`/`mideleg`, and the rule that
  a trap only drops to supervisor mode when the hart is not already in machine
  mode.
- **Vol II, "Supervisor-Level ISA"** — `sstatus`, `sie` and `sip` as masked
  *views* of their machine counterparts, which is why `emu/src/csr.rs` aliases
  them onto one storage rather than keeping two in sync. Also `TVM`, `TW` and
  `TSR`.
- **Vol II, Sv32 and Sv39 page-based virtual memory** — the walk, superpage
  alignment (a superpage's low PPN bits must be zero), and `SUM`/`MXR`.

## Encodings — `docs/opcodes/`

A vendored checkout of [riscv/riscv-opcodes](https://github.com/riscv/riscv-opcodes),
the machine-readable source of truth for instruction bit patterns.

- `extensions/rv_i`, `extensions/rv_m` — one line per instruction, giving the
  fixed bit fields. This is what the decoder and the RTL decode tables are
  checked against.
- `csrs.csv` — CSR numbers and names.
- `causes.csv` — `mcause` values.
- `encoding.h` — generated C header; handy for cross-checking a constant.

## Conformance suite — `docs/riscv-tests/`

A checkout of [riscv-software-src/riscv-tests](https://github.com/riscv-software-src/riscv-tests),
cloned with submodules so that `env/p/` (the physical-memory environment's
headers and linker script) comes along.

- `isa/rv32u*/`, `isa/rv64u*/` — the user-mode suites: `ui`, `um`, `ua`, `uc`,
  `uf` and `ud` at both widths. Each `rv32` file includes its `rv64`
  counterpart with `XLEN=32`, which is why one source tree covers both.
- `isa/rv32si/`, `isa/rv32mi/`, `isa/rv64si/`, `isa/rv64mi/` — the privileged
  suites: traps, delegation and paging.

All sixteen run, and all sixteen pass — 236/236, less the seven skipped by
name in `emu/tests/riscv_tests.rs` for features in other specifications
(Zacas, Sdtrig) and one RV32 file excluded upstream. See the table in the
top-level README.
- `env/p/riscv_test.h` — the prologue every test expands: register init, PMP
  setup, trap vector, and the `tohost` reporting macros.
- `env/p/link.ld` — links the tests at `0x8000_0000`.

`scripts/build-tests.sh` compiles these into `build/tests/`.

Regenerate the tables with `make -C docs/opcodes` if a hand-written table ever
disagrees with one of these files: the file wins.
