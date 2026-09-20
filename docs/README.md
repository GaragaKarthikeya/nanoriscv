# Reference material

None of this is committed — run `scripts/fetch-docs.sh` to pull it.

## Specifications — `docs/spec/`

| File | What it is | Use it for |
| --- | --- | --- |
| `riscv-unprivileged-20240411.pdf` | Volume I, **ratified** 2024-04-11 | Instruction semantics. Cite section numbers from here. |
| `riscv-privileged-20240411.pdf` | Volume II, **ratified** 2024-04-11 | CSRs, traps, privilege modes, PMP. |
| `riscv-spec-latest.pdf` | Both volumes, nightly build off `main` | Extensions ratified after the frozen release. Draft — do not cite its section numbers. |
| `norm-rules.json` | Normative rules extracted from the nightly | Machine-readable checklist of "the hart MUST ..." statements. |

The sections that matter for the current milestone:

- **Vol I §2.1–2.6** — RV32I base: the 47 instructions, the six formats, and the
  immediate-encoding rationale that explains why the bit scrambling in
  `emu/src/decode.rs` is worth it.
- **Vol I §4.2** — Zicsr. Note the read-before-write ordering and the rule that
  a set/clear with `rs1 == x0` must not write the CSR.
- **Vol I §4.5** — M extension. Division by zero and signed overflow have
  *defined* results; they do not trap.
- **Vol II §3.1** — `mstatus`, `mtvec`, `mepc`, `mcause`, `mtval`.
- **Vol II §3.3.1** — trap entry and `mret`.

## Encodings — `docs/opcodes/`

A vendored checkout of [riscv/riscv-opcodes](https://github.com/riscv/riscv-opcodes),
the machine-readable source of truth for instruction bit patterns.

- `extensions/rv_i`, `extensions/rv_m` — one line per instruction, giving the
  fixed bit fields. This is what the decoder and the RTL decode tables are
  checked against.
- `csrs.csv` — CSR numbers and names.
- `causes.csv` — `mcause` values.
- `encoding.h` — generated C header; handy for cross-checking a constant.

Regenerate the tables with `make -C docs/opcodes` if a hand-written table ever
disagrees with one of these files: the file wins.
