EMU := emu

.PHONY: test test-isa isa build run demo fmt lint docs clean

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

# Builds and runs the bare-metal console demo, which prints through the UART.
demo: build
	mkdir -p build
	riscv64-unknown-elf-gcc -march=rv64imac_zicsr -mabi=lp64 \
	  -nostdlib -nostartfiles -T examples/bare.ld \
	  examples/hello.S -o build/hello.elf
	$(EMU)/target/release/nanoemu build/hello.elf --quiet

fmt:
	cd $(EMU) && cargo fmt

lint:
	cd $(EMU) && cargo clippy --all-targets -- -D warnings

docs:
	scripts/fetch-docs.sh

clean:
	cd $(EMU) && cargo clean
