# RTL core

Empty on purpose. The SystemVerilog core lands here once the emulator is a
trustworthy golden model, because the plan is to verify the core by diffing it
against `nanoemu` instruction by instruction rather than by eyeballing waveforms.

Intended shape, for when work starts:

- `core/` — a classic 5-stage RV32I pipeline: fetch, decode, execute, memory,
  writeback, with a forwarding unit and a load-use interlock.
- `tb/` — Verilator testbench. Each cycle a retire happens, the DUT's
  architectural state is compared against a `nanoemu` step; the first mismatch
  fails the test and prints both states.
- `soc/` — the wrapper that puts the core on the ZCU104, reusing `boards/zcu104`
  from the parent repo.

Verilator is already on this machine (`/usr/local/bin/verilator`).
