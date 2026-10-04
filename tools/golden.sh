#!/usr/bin/env bash
# Golden rendering tests: compile each tests/golden/cases/*.typ to PNGs with
# the pinned Typst and compare byte-for-byte against tests/golden/ref/.
#   tools/golden.sh check   — compare (CI mode, fails on drift)
#   tools/golden.sh bless   — regenerate the references
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${QUARRY_PREFIX:-$HOME/.local/quarry}"
TYPST="${TYPST_BIN:-$PREFIX/bin/typst}"
MODE="${1:-check}"
OUT="$REPO/tests/golden/out"
REF="$REPO/tests/golden/ref"
rm -rf "$OUT" && mkdir -p "$OUT" "$REF"
python3 "$REPO/tools/gen_fixtures.py" >/dev/null
fail=0
for case in "$REPO"/tests/golden/cases/*.typ; do
  name="$(basename "$case" .typ)"
  "$TYPST" compile --root "$REPO" --format png --ppi 96 "$case" "$OUT/$name-{n}.png"
  if [ "$MODE" = bless ]; then
    rm -f "$REF/$name"-*.png
    cp "$OUT/$name"-*.png "$REF/"
    echo "blessed $name ($(ls "$OUT/$name"-*.png | wc -l) pages)"
  else
    for png in "$OUT/$name"-*.png; do
      base="$(basename "$png")"
      if ! cmp -s "$png" "$REF/$base"; then
        echo "GOLDEN MISMATCH: $base (bless with tools/golden.sh bless after review)"
        fail=1
      fi
    done
    # page-count drift also fails
    n_out=$(ls "$OUT/$name"-*.png | wc -l)
    n_ref=$(ls "$REF/$name"-*.png 2>/dev/null | wc -l)
    if [ "$n_out" != "$n_ref" ]; then
      echo "GOLDEN MISMATCH: $name page count $n_out vs $n_ref"
      fail=1
    fi
  fi
done
[ "$fail" = 0 ] && echo "golden: OK"
exit $fail
