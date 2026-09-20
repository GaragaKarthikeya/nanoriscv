EMU := emu

.PHONY: test test-isa isa build run fmt lint docs clean

# Runs the hand-written tests and, if they have been built, the official suite.
test:
	cd $(EMU) && cargo test

# Just the official riscv-tests suites, with per-suite output.
test-isa:
	cd $(EMU) && cargo test --test riscv_tests -- --nocapture

# Compile the official suite. Needs scripts/fetch-docs.sh to have run.
isa:
	scripts/build-tests.sh

build:
	cd $(EMU) && cargo build --release

# make run IMG=path/to/image.bin
run: build
	$(EMU)/target/release/nanoemu $(IMG)

fmt:
	cd $(EMU) && cargo fmt

lint:
	cd $(EMU) && cargo clippy --all-targets -- -D warnings

docs:
	scripts/fetch-docs.sh

clean:
	cd $(EMU) && cargo clean
