# quarry — one command per task; `just test` is the whole gate.
export PATH := env_var("HOME") + "/.cargo/bin:" + env_var("HOME") + "/.local/quarry/bin:" + env_var("PATH")
export WASI_SDK := env_var_or_default("WASI_SDK", env_var("HOME") + "/.local/quarry/opt/wasi-sdk")

default: test

# install the pinned toolchain (idempotent, user-local, no sudo)
bootstrap:
    ./tools/bootstrap.sh

# build the WASM plugin: cargo → wasi-stub (allowlist-checked) → wasm-opt
build-wasm:
    ./tools/build_wasm.sh
    cp build/quarry-sqlite.wasm packages/quarry/
    cp build/quarry-sqlite.wasm.sha256 packages/quarry/

# deterministic fixtures + differential corpus
fixtures:
    python3 tools/gen_fixtures.py
    python3 tools/gen_corpus.py

# the full gate: everything below, fail-fast
test: build-wasm test-unit test-diff test-regress test-typst test-golden test-fuzz-lite size-budget
    @echo "──────────────────────────────────────"
    @echo "quarry: full test gate green"

# Rust unit + integration tests (native engine, wasmi host, purity).
# The engine/host suites run in release (wasmi interpretation speed matters);
# the CLI's logic tests run in dev — release would re-link the DataFusion
# graph under LTO for no benefit.
test-unit:
    cargo test --release -p quarry-engine -p quarry-host -p quarry-difftest
    cargo test -p quarry-cli

# ≥500-query native-vs-wasm differential, byte-identical
test-diff:
    cargo run --release -p quarry-difftest

# the named d01–d27 regression suite
test-regress:
    cargo test -p quarry-host --release --test regression

# self-asserting Typst documents (also the runnable API examples)
test-typst:
    ./tools/run_typst_tests.sh

# golden renderings (PNG byte-compare); `just bless` after reviewed changes
test-golden:
    ./tools/golden.sh check

bless:
    ./tools/golden.sh bless

# deterministic robustness sweep (stable-toolchain fuzz)
test-fuzz-lite:
    cargo run --release -p quarry-difftest --bin fuzz_lite

# document suite across every installed Typst version (F-23)
test-matrix:
    ./tools/typst_matrix.sh

# two clean builds must hash identically (F-22/D-22)
test-repro:
    ./tools/test_repro.sh

size-budget:
    ./tools/check_size_budget.sh

# Phase-0 measurements → docs/phase0-findings.md input
phase0:
    cargo run --release -p quarry-host --bin phase0

# libFuzzer targets (needs nightly + cargo-fuzz; nightly CI runs these)
fuzz target="fuzz_open" time="60":
    cd crates/quarry-fuzz && cargo +nightly fuzz run {{target}} -- -max_total_time={{time}}

# assemble the publishable package tree
package: build-wasm
    ./tools/package.sh

# end-to-end sidecar integration (discovery → cache → offline rebuild → lint)
test-sidecar:
    cargo build --profile native-release -p quarry-cli
    ./tools/test_sidecar.sh

# Phase-3 integration against a real portable PostgreSQL server
test-postgres:
    cargo build --profile native-release -p quarry-cli --bin quarry
    PG_ROOT="$(./tools/fetch_postgres.sh)" ./tools/test_postgres.sh
