#!/usr/bin/env bash
# quarry toolchain bootstrap — idempotent, user-local, no sudo required.
#
# Installs pinned versions of every tool the build needs:
#   Rust 1.97.1 (+ wasm32-wasip1), wasi-sdk 33, Typst 0.15.1, binaryen 131,
#   wasi-stub 0.3.0, and the vendored SQLite 3.53.4 amalgamation (hash-verified).
#
# Everything lands under $QUARRY_PREFIX (default ~/.local/quarry) and ~/.cargo.
set -euo pipefail

PREFIX="${QUARRY_PREFIX:-$HOME/.local/quarry}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$PREFIX/bin" "$PREFIX/opt"

RUST_VERSION=1.97.1
WASI_SDK_VERSION=33
TYPST_VERSION=0.15.1
BINARYEN_VERSION=131
WASI_STUB_VERSION=0.3.0
SQLITE_VERSION=3.53.4
SQLITE_ZIP=sqlite-amalgamation-3530400.zip
SQLITE_YEAR=2026
SQLITE_SHA3=628a44cfe82c66aed1ccbbe85a562d2e33ebe64b3288981ed76285612227934e

log() { printf '\n== %s\n' "$*"; }

# 1. Rust ---------------------------------------------------------------
if ! command -v "$HOME/.cargo/bin/rustc" >/dev/null 2>&1; then
  log "Installing Rust $RUST_VERSION"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain "$RUST_VERSION" --profile minimal \
         --component rustfmt --component clippy --target wasm32-wasip1 --no-modify-path
else
  log "Rust present: $("$HOME/.cargo/bin/rustc" --version)"
  "$HOME/.cargo/bin/rustup" toolchain install "$RUST_VERSION" --profile minimal \
    --component rustfmt --component clippy >/dev/null
  "$HOME/.cargo/bin/rustup" target add wasm32-wasip1 --toolchain "$RUST_VERSION" >/dev/null
fi
export PATH="$HOME/.cargo/bin:$PATH"

# 2. wasi-sdk -----------------------------------------------------------
if [ ! -x "$PREFIX/opt/wasi-sdk/bin/clang" ]; then
  log "Installing wasi-sdk $WASI_SDK_VERSION"
  curl -fL --retry 3 -o /tmp/wasi-sdk.tar.gz \
    "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-$WASI_SDK_VERSION/wasi-sdk-$WASI_SDK_VERSION.0-x86_64-linux.tar.gz"
  tar -xzf /tmp/wasi-sdk.tar.gz -C "$PREFIX/opt"
  ln -sfn "$PREFIX/opt/wasi-sdk-$WASI_SDK_VERSION.0-x86_64-linux" "$PREFIX/opt/wasi-sdk"
  rm -f /tmp/wasi-sdk.tar.gz
else
  log "wasi-sdk present"
fi

# 3. Typst (pinned stable + older versions for the compatibility matrix) ----
install_typst() {
  local ver="$1" dest="$PREFIX/opt/typst-$1"
  if [ ! -x "$dest/typst" ]; then
    log "Installing Typst $ver"
    mkdir -p "$dest"
    curl -fL --retry 3 "https://github.com/typst/typst/releases/download/v$ver/typst-x86_64-unknown-linux-musl.tar.xz" \
      | tar -xJ -C "$dest" --strip-components=1
  fi
}
install_typst "$TYPST_VERSION"
ln -sf "$PREFIX/opt/typst-$TYPST_VERSION/typst" "$PREFIX/bin/typst"
# Older versions for the F-23 matrix; failures are non-fatal.
install_typst 0.14.0 || echo "warn: typst 0.14.0 unavailable"
install_typst 0.13.1 || echo "warn: typst 0.13.1 unavailable"

# 4. binaryen (wasm-opt) ------------------------------------------------
if [ ! -x "$PREFIX/opt/binaryen-version_$BINARYEN_VERSION/bin/wasm-opt" ]; then
  log "Installing binaryen $BINARYEN_VERSION"
  curl -fL --retry 3 \
    "https://github.com/WebAssembly/binaryen/releases/download/version_$BINARYEN_VERSION/binaryen-version_$BINARYEN_VERSION-x86_64-linux.tar.gz" \
    | tar -xz -C "$PREFIX/opt"
fi
ln -sf "$PREFIX/opt/binaryen-version_$BINARYEN_VERSION/bin/wasm-opt" "$PREFIX/bin/wasm-opt"

# 4b. just (task runner) ------------------------------------------------
if [ ! -x "$PREFIX/bin/just" ]; then
  log "Installing just"
  curl -fL --retry 3 "https://github.com/casey/just/releases/download/1.36.0/just-1.36.0-x86_64-unknown-linux-musl.tar.gz"     | tar -xz -C "$PREFIX/bin" just
fi

# 5. wasi-stub ----------------------------------------------------------
if ! command -v "$HOME/.cargo/bin/wasi-stub" >/dev/null 2>&1; then
  log "Installing wasi-stub $WASI_STUB_VERSION"
  cargo install wasi-stub --version "$WASI_STUB_VERSION" --locked
fi

# 6. SQLite amalgamation, vendored + hash-verified ----------------------
VENDOR="$REPO/crates/quarry-engine/vendor/sqlite"
if [ ! -f "$VENDOR/sqlite3.c" ]; then
  log "Vendoring SQLite $SQLITE_VERSION"
  curl -fL --retry 3 -o /tmp/sqlite.zip "https://sqlite.org/$SQLITE_YEAR/$SQLITE_ZIP"
  python3 - "$SQLITE_SHA3" <<'PY'
import hashlib, sys
h = hashlib.sha3_256(open('/tmp/sqlite.zip', 'rb').read()).hexdigest()
assert h == sys.argv[1], f"SQLite download hash mismatch: {h}"
print("sqlite zip sha3-256 OK")
PY
  mkdir -p "$VENDOR"
  (
    rm -rf /tmp/sqlite-amalg && mkdir /tmp/sqlite-amalg && cd /tmp/sqlite-amalg
    python3 -c "import zipfile; zipfile.ZipFile('/tmp/sqlite.zip').extractall('.')"
    find . -name 'sqlite3.c' -exec cp {} "$VENDOR/" \;
    find . -name 'sqlite3.h' -exec cp {} "$VENDOR/" \;
    find . -name 'sqlite3ext.h' -exec cp {} "$VENDOR/" \;
  )
  printf '%s' "$SQLITE_VERSION" > "$VENDOR/VERSION"
  printf '%s' "$SQLITE_SHA3" > "$VENDOR/SHA3-256"
  rm -rf /tmp/sqlite.zip /tmp/sqlite-amalg
else
  log "SQLite amalgamation already vendored ($(cat "$VENDOR/VERSION"))"
fi

log "Bootstrap complete"
echo "  rustc:    $("$HOME/.cargo/bin/rustc" --version)"
echo "  clang:    $("$PREFIX/opt/wasi-sdk/bin/clang" --version | head -1)"
echo "  typst:    $("$PREFIX/bin/typst" --version)"
echo "  wasm-opt: $("$PREFIX/bin/wasm-opt" --version)"
echo "  sqlite:   $(cat "$VENDOR/VERSION") (vendored)"
echo
echo 'Add to PATH:  export PATH="$HOME/.cargo/bin:'"$PREFIX"'/bin:$PATH"'
echo 'And set:      export WASI_SDK="'"$PREFIX"'/opt/wasi-sdk"'
