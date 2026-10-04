#!/usr/bin/env bash
# Assemble the publishable package tree in packages/quarry/: sources are
# already in place; this stamps the built WASM + its hash and sanity-checks
# the manifest against the artifact (D-24 discipline at packaging time).
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

[ -f build/quarry-sqlite.wasm ] || { echo "run tools/build_wasm.sh first" >&2; exit 1; }
cp build/quarry-sqlite.wasm packages/quarry/
cp build/quarry-sqlite.wasm.sha256 packages/quarry/
cp README.md packages/quarry/README.md
cp LICENSE packages/quarry/LICENSE

# the packaged tree must reference the same engine version everywhere
version=$(cat crates/quarry-engine/vendor/sqlite/VERSION)
grep -q "$version" packages/quarry/README.md || {
  echo "package README does not mention SQLite $version (D-24)" >&2; exit 1;
}
python3 - <<PY
import tomllib
with open("packages/quarry/typst.toml", "rb") as f:
    m = tomllib.load(f)["package"]
for key in ("name", "version", "entrypoint", "authors", "license", "description"):
    assert m.get(key), f"typst.toml missing {key}"
print(f"package quarry {m['version']} assembled")
PY
ls -la packages/quarry/
