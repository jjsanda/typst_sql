#!/usr/bin/env bash
# F-24: every user downloads the WASM on first @preview resolution; growth is
# a decision, not an accident. Raise the budget deliberately, in review.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
BUDGET=1600000
actual=$(stat -c%s "$REPO/build/quarry-sqlite.wasm")
echo "quarry-sqlite.wasm: $actual bytes (budget $BUDGET)"
if [ "$actual" -gt "$BUDGET" ]; then
  echo "SIZE BUDGET EXCEEDED — raise deliberately in tools/check_size_budget.sh" >&2
  exit 1
fi
