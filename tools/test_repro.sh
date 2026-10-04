#!/usr/bin/env bash
# d22/F-22: two from-scratch builds of the plugin must be byte-identical.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"
./tools/build_wasm.sh >/dev/null
h1=$(sha256sum build/quarry-sqlite.wasm | cut -d' ' -f1)
touch crates/quarry-plugin/src/lib.rs crates/quarry-engine/src/lib.rs crates/quarry-engine/vendor/sqlite/sqlite3.c
./tools/build_wasm.sh >/dev/null
h2=$(sha256sum build/quarry-sqlite.wasm | cut -d' ' -f1)
echo "build A: $h1"
echo "build B: $h2"
[ "$h1" = "$h2" ] && echo "reproducible: OK" || { echo "REPRODUCIBILITY FAILURE"; exit 1; }
