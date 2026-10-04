#!/usr/bin/env bash
# Compile every self-asserting document in tests/typst with the pinned Typst.
# A compile failure IS the test failure (documents use assert.eq throughout).
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${QUARRY_PREFIX:-$HOME/.local/quarry}"
TYPST="${TYPST_BIN:-$PREFIX/bin/typst}"
python3 "$REPO/tools/gen_fixtures.py" >/dev/null
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
for doc in "$REPO"/tests/typst/*.typ; do
  name="$(basename "$doc")"
  "$TYPST" compile --root "$REPO" \
    --input quarry-now=2026-01-20T00:00:00Z \
    --input quarry-seed=42 \
    "$doc" "$tmp/${name%.typ}.pdf"
  echo "ok   $name"
done
echo "typst documents: all green ($("$TYPST" --version))"
