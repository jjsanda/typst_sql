# Phase-0 findings

The roadmap (market_analysis/06-roadmap.md) forbade building on unvalidated
assumptions. All five questions are now answered **with data**, measured
against the shipped `quarry-sqlite.wasm` (1.48 MB) under wasmi 1.0.9 with
Typst's exact host configuration, on the development machine (7-core x86-64).
Reproduce with `just phase0`.

## Q1 — Typst's plugin memory ceiling

**Answer: Typst imposes none.** Verified two ways: by reading the Typst
plugin host (wasmi `Config::default()` with only relaxed-SIMD disabled — no
`StoreLimits`, no fuel, no memory cap), and empirically: 64 MB of transient
allocations inside one plugin call succeed under the replica host.

The *practical* ceiling is host RAM: after `plugin.transition()` the resident
cost is roughly **(1× database in linear memory) + (1× snapshot copy) + the
`read()` bytes Typst interns** — measured 1.05×–1.16× of the database for the
snapshot alone at 8–26 MB (small databases pay a fixed ~1.4 MB engine
footprint instead). Budget ≈ 3× database size end to end; a 200 MB database
wants ~600 MB of RAM. Documented in the README; the sidecar is the right tool
past that point.

## Q2 — wasmi interpretation penalty

| query | native | wasmi | penalty |
|---|---|---|---|
| small aggregate (200 rows) | 0.12 ms | 4.66 ms | 37.6× |
| 200-row scan | 0.65 ms | 20.25 ms | 31.2× |
| sort 200 rows | 0.60 ms | 26.59 ms | 44.6× |
| recursive CTE (2k rows) | 0.54 ms | 33.90 ms | 62.6× |
| count(*) over 26 MB | 10.0 ms | 34.6 ms | 3.5× |
| group-by over 26 MB (76k rows) | 200 ms | 4 435 ms | 22.2× |
| indexed point lookup in 26 MB | 0.17 ms | 1.61 ms | 9.3× |

**Answer: 3.5×–63×, typically 20–45×.** Report-scale workloads (dozens of
queries over ≤ tens of MB) stay comfortably interactive; heavy analytics over
large data belongs in the sidecar. This is exactly the dual-path split the
architecture assumed — now with numbers.

## Q3 — in-WASM analytical engine size

**Answer: not viable; analytics ships sidecar-only — the R-01 kill criterion
applied as designed.** Two independent disqualifiers:
- SQLite alone, optimized (`-Oz`, LTO, wasm-opt), is 1.48 MB; a
  DataFusion/Arrow build sits far beyond the ~5 MB budget (DuckDB-WASM, the
  comparable, is ~9.6 MB gzipped).
- the measured interpreter penalty above lands analytical workloads 20×+
  slower — squarely past the ~20× kill line.

The sidecar's analytics source uses **native DataFusion** (the engine
04-project-description.md §7.2 recommends), losing only web-app support for
that one backend.

## Q4 — `plugin.transition()` snapshot cost

| database | transition (open + snapshot) | snapshot size |
|---|---|---|
| 60 KB | 8.6 ms | 1.44 MB (fixed engine footprint) |
| 8.7 MB | 27 ms | 1.05–1.16× database |
| 26 MB | 85 ms | 1.05× database |

**Answer: linear, cheap, once.** The snapshot happens once per
`qr.sqlite(…)`, never per query (verified: 50 queries after one open leave
the snapshot byte-identical — regression d13). Zero-copy `open_N` +
`SQLITE_DESERIALIZE_READONLY` keep the database at one copy inside linear
memory.

## Q5 — discovery before queries resolve

**Answer: works, with one honest caveat.** Compiling with
`--input quarry-discover=1` turns remote queries into placeholder envelopes
carrying their requests, so a cold checkout compiles; `typst eval
'query(<quarry-request>)…'` extracts them (`typst query` fallback for
≤ 0.14). Caveat: metadata must reach page content — results used purely in
code (chart arrays, inline values) need either a one-line `#qr.declare(env)`
or an explicit `[sources.X.queries.NAME]` declaration in quarry.toml.
`quarry sync` unions all three channels, and re-verifies every cache key
against the document's own computation, refusing on mismatch.

## Consequences already applied

- Dual path confirmed with numbers; README documents the ≈3× RAM rule and
  the interpreter penalty table.
- Analytics = sidecar-only DataFusion (native), per the kill criterion.
- One transition per `qr.sqlite()`; per-query transitions would re-snapshot
  and are not exposed by the API at all.
- The engine's process-global state (clock, seeded RNG, THREADSAFE=0) means
  **one live native connection per process**; the CLI executes queries
  sequentially and the API documents it. (Found the hard way: a benchmark
  holding two native connections deadlocked — the lock did its job.)
