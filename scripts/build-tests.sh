#!/usr/bin/env bash
# Builds the official riscv-tests rv32ui and rv32um suites.
#
# The upstream Makefile wants a riscv64-unknown-elf GNU toolchain. There isn't
# one on this machine and installing it needs root, so this builds the same
# sources with clang's RISC-V backend and the LLD that ships inside the Rust
# toolchain. The test sources are untouched -- only the driver differs.
set -euo pipefail

cd "$(dirname "$0")/.."
TESTS=docs/riscv-tests
OUT=build/tests

if [ ! -d "$TESTS/isa" ]; then
  echo "missing $TESTS -- run scripts/fetch-docs.sh first" >&2
  exit 1
fi

CC=${CC:-clang-18}
LLD=$(find "$HOME/.rustup/toolchains" -name rust-lld -type f 2>/dev/null | head -1)
if [ -z "$LLD" ]; then
  echo "no rust-lld found; install a rustup toolchain or set LLD" >&2
  exit 1
fi

mkdir -p "$OUT"
CFLAGS=(
  --target=riscv32-unknown-elf
  -march=rv32im_zifencei -mabi=ilp32
  -nostdlib -nostartfiles -fno-builtin
  -DXLEN=32 -D__riscv_xlen=32
  -I"$TESTS/env/p" -I"$TESTS/env" -I"$TESTS/isa/macros/scalar"
)

built=0
failed=0
for suite in rv32ui rv32um; do
  for src in "$TESTS/isa/$suite"/*.S; do
    name="$suite-p-$(basename "$src" .S)"
    if "$CC" "${CFLAGS[@]}" -c "$src" -o "$OUT/$name.o" 2>"$OUT/$name.log" &&
       "$LLD" -flavor gnu -m elf32lriscv -T "$TESTS/env/p/link.ld" \
              "$OUT/$name.o" -o "$OUT/$name.elf" 2>>"$OUT/$name.log"; then
      rm -f "$OUT/$name.o" "$OUT/$name.log"
      built=$((built + 1))
    else
      echo "build failed: $name (see $OUT/$name.log)" >&2
      failed=$((failed + 1))
    fi
  done
done

echo "built $built test binaries into $OUT${failed:+, $failed failed}"
[ "$failed" -eq 0 ]
