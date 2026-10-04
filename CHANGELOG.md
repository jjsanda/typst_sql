# Changelog

## 0.1.0 — unreleased

The first release: "SQLite done right" (Phase 1) plus the sidecar
(Phase 2/3 core).

### Added
- In-WASM SQLite 3.53.4 backend: real libc/libm (wasi-libc), injected clock
  and seed, zero-copy `plugin.transition()` open, up to 8 named sources with
  cross-source joins, FTS5 + R-tree + JSON + math functions.
- Typed CBOR result envelope v1: dict rows, 64-bit exact integers, BLOBs as
  bytes, column type metadata, deterministic bytes, catchable errors with
  offsets and spelling hints.
- Document API: `sqlite`/`try-sqlite`/`probe`, `query`/`try-query`/`value`/
  `column`/`row`, `describe`, `schema`/`schema-doc`, `capabilities`,
  parameter binding (named + positional, typed incl. datetime/duration/
  bytes/JSON), result assertions (`expect:`), SQL as raw blocks, `.sql`
  query libraries.
- Presentation: `sql-table` (repeating headers, zebra, group headers,
  subtotals, totals, row limits, cell hooks, empty states), locale-aware
  formatters (number/currency/percent/date/unit), chart adapters
  (`xy`/`xticks`/`series`/`cetz-points`), `pivot`, `sparkline`.
- Sidecar CLI `quarry`: init/sync(--watch)/status/verify/prune/explain/
  lint/diff; content-addressed cache + manifest + committed lockfile;
  strict/auto/offline modes; sqlite, DataFusion analytics (CSV/Parquet/
  NDJSON), Postgres (read-only txn, timeouts, rustls) and MySQL (read-only
  session) executors; secrets only by reference (`env:`/`file:`).
- Test infrastructure: 523-query native-vs-wasm differential (byte-identical),
  named regression per incumbent defect (d01–d27), Typst-identical wasmi host
  replica with purity (fresh-vs-dirty) checks, golden renderings, adversarial
  input sweep, reproducible-build check, size budget, CI + nightly fuzz/drift
  workflows.
