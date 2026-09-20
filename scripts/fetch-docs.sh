#!/usr/bin/env bash
# Fetches the reference material nanoriscv is built against. Everything this
# downloads is gitignored, so run it once after cloning.
set -euo pipefail

cd "$(dirname "$0")/.."
SPEC=docs/spec
mkdir -p "$SPEC"

# Ratified specs. These are the ones to cite in comments and commit messages:
# they are frozen, so a section number stays valid.
RATIFIED=20240411
BASE=https://github.com/riscv/riscv-isa-manual/releases/download

fetch() {
  local url=$1 out=$2
  if [ -f "$out" ]; then
    echo "have  $out"
    return
  fi
  echo "fetch $out"
  curl -fsSL --retry 3 -o "$out" "$url"
}

fetch "$BASE/$RATIFIED/unpriv-isa-asciidoc.pdf" "$SPEC/riscv-unprivileged-$RATIFIED.pdf"
fetch "$BASE/$RATIFIED/priv-isa-asciidoc.pdf"   "$SPEC/riscv-privileged-$RATIFIED.pdf"

# Latest nightly of the combined manual, for extensions ratified after the
# frozen release above. Treat it as a draft; do not cite its section numbers.
LATEST=$(curl -fsSL https://api.github.com/repos/riscv/riscv-isa-manual/releases/latest \
  | grep -o 'https://[^"]*/riscv-spec\.pdf' | head -1)
fetch "$LATEST" "$SPEC/riscv-spec-latest.pdf"

# Machine-readable instruction encodings — the source of truth for the decoder
# and for the RTL decode tables.
clone() {
  local url=$1 dir=$2 flags=${3:-}
  if [ -d "$dir" ]; then
    echo "have  $dir"
    return
  fi
  echo "clone $dir"
  # shellcheck disable=SC2086
  git clone --depth 1 $flags "$url" "$dir"
  rm -rf "$dir/.git"
}

clone https://github.com/riscv/riscv-opcodes.git docs/opcodes

# The official conformance suite. --recursive picks up riscv-test-env, which
# holds the p-environment headers and linker script the tests include.
clone https://github.com/riscv-software-src/riscv-tests.git docs/riscv-tests --recursive

echo
echo "next: scripts/build-tests.sh"
