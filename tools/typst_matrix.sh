#!/usr/bin/env bash
# F-23: run the document suite across every installed Typst version.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${QUARRY_PREFIX:-$HOME/.local/quarry}"
status=0
for dir in "$PREFIX"/opt/typst-*; do
  [ -x "$dir/typst" ] || continue
  ver="$(basename "$dir" | sed 's/^typst-//')"
  echo "=== Typst $ver ==="
  if TYPST_BIN="$dir/typst" "$REPO/tools/run_typst_tests.sh"; then
    echo "=== Typst $ver: OK ==="
  else
    echo "=== Typst $ver: FAILED ==="
    status=1
  fi
done
exit $status
