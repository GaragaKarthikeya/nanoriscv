EMU := emu

.PHONY: test build run fmt lint docs clean

test:
	cd $(EMU) && cargo test

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
