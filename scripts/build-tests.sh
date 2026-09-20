#!/usr/bin/env bash
# Builds the official riscv-tests suites for both XLENs.
#
# Prefers the RISC-V GNU toolchain, which is what upstream expects. Falls back
# to clang plus the LLD inside the Rust toolchain, which is what this project
# used before a GNU toolchain was available and still works on a machine
# without one.
set -euo pipefail

cd "$(dirname "$0")/.."
TESTS=docs/riscv-tests
OUT=build/tests

if [ ! -d "$TESTS/isa" ]; then
  echo "missing $TESTS -- run scripts/fetch-docs.sh first" >&2
  exit 1
fi

# zicsr and zifencei have been separate extensions since GCC 12, and GCC 14
# rejects a csrr whose extension is not named in -march. Clang is lenient about
# it; spelling them out keeps one flag string working for both.
ISA32=rv32im_zicsr_zifencei
ISA64=rv64im_zicsr_zifencei

GNU=riscv64-unknown-elf-gcc
if command -v "$GNU" >/dev/null 2>&1; then
  TOOLCHAIN="$GNU"
  compile() { # <march> <mabi> <src> <out.elf> <logfile>
    "$GNU" -march="$1" -mabi="$2" -nostdlib -nostartfiles -fno-builtin \
      -DXLEN="${1:2:2}" -I"$TESTS/env/p" -I"$TESTS/env" -I"$TESTS/isa/macros/scalar" \
      -T "$TESTS/env/p/link.ld" "$3" -o "$4" 2>"$5"
  }
else
  CC=${CC:-clang-18}
  LLD=$(command -v ld.lld-18 || find "$HOME/.rustup/toolchains" -name rust-lld -type f 2>/dev/null | head -1)
  if ! command -v "$CC" >/dev/null 2>&1 || [ -z "$LLD" ]; then
    echo "need either $GNU, or $CC with an LLD" >&2
    exit 1
  fi
  TOOLCHAIN="$CC + $(basename "$LLD")"
  compile() {
    local emu=elf32lriscv
    [ "${1:2:2}" = 64 ] && emu=elf64lriscv
    "$CC" --target=riscv"${1:2:2}"-unknown-elf -march="$1" -mabi="$2" \
      -nostdlib -nostartfiles -fno-builtin \
      -DXLEN="${1:2:2}" -I"$TESTS/env/p" -I"$TESTS/env" -I"$TESTS/isa/macros/scalar" \
      -c "$3" -o "$4.o" 2>"$5" &&
    "$LLD" -flavor gnu -m "$emu" -T "$TESTS/env/p/link.ld" "$4.o" -o "$4" 2>>"$5" &&
    rm -f "$4.o"
  }
fi

echo "toolchain: $TOOLCHAIN"
mkdir -p "$OUT"

built=0
failed=0
build_suite() { # <suite dir name> <march> <mabi>
  local suite=$1 march=$2 mabi=$3
  [ -d "$TESTS/isa/$suite" ] || return 0
  for src in "$TESTS/isa/$suite"/*.S; do
    local name="$suite-p-$(basename "$src" .S)"
    if compile "$march" "$mabi" "$src" "$OUT/$name.elf" "$OUT/$name.log"; then
      rm -f "$OUT/$name.log"
      built=$((built + 1))
    else
      echo "build failed: $name (see $OUT/$name.log)" >&2
      failed=$((failed + 1))
    fi
  done
}

build_suite rv32ui "$ISA32" ilp32
build_suite rv32um "$ISA32" ilp32
build_suite rv64ui "$ISA64" lp64
build_suite rv64um "$ISA64" lp64

echo "built $built test binaries into $OUT${failed:+, $failed failed}"
[ "$failed" -eq 0 ]
