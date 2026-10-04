#!/usr/bin/env bash
# Build the quarry-sqlite plugin: cargo → wasi-stub → wasm-opt → build/.
# Reproducible: pinned toolchain (rust-toolchain.toml), --locked, no wall-clock
# inputs. CI runs this twice and asserts identical hashes (F-22/D-22).
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${QUARRY_PREFIX:-$HOME/.local/quarry}"
export WASI_SDK="${WASI_SDK:-$PREFIX/opt/wasi-sdk}"
export PATH="$HOME/.cargo/bin:$PREFIX/bin:$PATH"
cd "$REPO"

cargo build -p quarry-plugin --release --target wasm32-wasip1 --locked

mkdir -p build
RAW=target/wasm32-wasip1/release/quarry_plugin.wasm

# Stub WASI imports; --list output is diffed against the committed allowlist so
# a NEW import (e.g. a dependency starting to use std::time) fails the build
# instead of becoming a silent no-op (the D-04 defect class).
wasi-stub "$RAW" -o build/quarry-sqlite.stubbed.wasm \
  | sed -n 's/^Stubbing function //p' | LC_ALL=C sort > build/wasi-imports.txt
if ! diff -u tools/wasi-imports.allowlist build/wasi-imports.txt; then
  echo "error: WASI import set changed — inspect and update tools/wasi-imports.allowlist deliberately" >&2
  exit 1
fi

wasm-opt -Oz --strip-debug --strip-producers \
  --enable-bulk-memory --enable-sign-ext --enable-mutable-globals \
  --enable-nontrapping-float-to-int --enable-multivalue \
  build/quarry-sqlite.stubbed.wasm -o build/quarry-sqlite.wasm

sha256sum build/quarry-sqlite.wasm | tee build/quarry-sqlite.wasm.sha256
ls -la build/quarry-sqlite.wasm
