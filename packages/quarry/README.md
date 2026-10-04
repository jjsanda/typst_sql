# quarry — compile-time SQL for Typst

Query SQLite (and, through the sidecar, DataFusion analytics, Postgres and
MySQL) **at compile time**, into typed Typst values, formatted tables and
chart-ready data.

```typst
#import "@preview/quarry:0.1.0" as qr

#let db = qr.sqlite(read("sales.sqlite", encoding: none))

#let regions = qr.query(db, ```sql
  SELECT region, SUM(amount) AS total, COUNT(*) AS n
  FROM orders WHERE fiscal_year = :year
  GROUP BY region ORDER BY total DESC
```, year: 2026)

#qr.sql-table(
  regions,
  columns: (
    (key: "region", title: [Region]),
    (key: "total",  title: [Revenue], format: qr.currency("USD")),
    (key: "n",      title: [Orders],  format: qr.number()),
  ),
  zebra: true,
  totals: (total: "sum", n: "sum"),
)

Total revenue: #qr.currency("USD")(qr.value(db, "SELECT SUM(amount) FROM orders"))
```

The query lives beside the prose it explains. Schema changes fail loudly at
compile time — with a caret and a "did you mean" — instead of silently
shifting a CSV column. Values stay typed end to end: dates become `datetime`,
BLOBs become `bytes` you can hand to `image()`, integers keep all 64 bits.

## Why quarry

**Correctness is the feature.** The one existing Typst SQL package computes
wrong numbers silently: stubbed `sqrt`/`pow` returning 0.0, 64-bit integers
truncated to 32, `1.5e10` parsed as `1.5`, and `date('now')` reporting
**1970-01-01** — which makes every "last 30 days" filter match all rows ever
written. quarry links a real libc/libm, injects the clock and the random seed
explicitly, and proves the result: a **523-query differential suite** runs
identical queries against native SQLite and the shipped WASM build and
requires byte-identical envelopes (documented ≤2 ULP tolerance for
transcendental functions only), plus one named regression test per incumbent
defect. Correct, or loud — never silently wrong.

**Reproducible by construction.** The same source and the same data produce
the same PDF. Time is an explicit input (`now: datetime(…)`, or
`--input quarry-now=…` from your commit timestamp); `random()` is seeded;
plugin results are pure and memoizable. For remote data, `quarry sync`
materializes results into a committed content-addressed cache with a
lockfile: builds are byte-identical offline forever, data drift shows up in
`git diff` as row counts and digests, and CI compiles **without database
credentials**. No LaTeX tool offers this.

**Errors you can use — and catch.**

```
quarry: no such table: custmers
  │ SELECT * FROM custmers
  │               ^^^^^^^^
  = did you mean "customers"?
  = source: main (sqlite 3.53.4)
```

`qr.query` fails the compile with that; `qr.try-query` hands you
`(ok, value, error, rendered)` so a template can degrade gracefully — and so
every error path is testable inside a document.

## The dual path

| | in-WASM (default) | sidecar (`quarry` CLI) |
|---|---|---|
| install | none — works in the Typst web app | `cargo install`, prebuilt binaries |
| engines | SQLite 3.53.4 | SQLite, DataFusion (CSV/Parquet/NDJSON), Postgres, MySQL |
| data size | RAM-bound: budget ≈3× the database (measured) | unlimited |
| speed | interpreted: 3.5–63× native (measured; fine for report-scale) | native |
| network | never (Typst forbids it, by design) | at sync time only; compile reads cache |

Both paths produce the identical result envelope — switching is
configuration, not a rewrite.

### Sidecar in 60 seconds

```console
$ quarry init                 # scaffold quarry.toml + .quarry/
$ $EDITOR quarry.toml         # declare sources; credentials via env:VAR only
$ quarry sync report.typ      # discover queries, execute, cache, lock
$ typst compile report.typ    # reads only the cache — no network, no secrets
```

```typst
#let wh = qr.remote("warehouse")
#let churn = qr.query(wh, ```sql SELECT month, churn_rate FROM metrics WHERE cohort = :c```, c: "2026-enterprise")
```

Commit `.quarry/` and `quarry.lock`. `quarry sync --watch` pairs with
`typst watch`; modes: `strict` (CI: missing cache = error naming the query
and the fix), `auto` (TTL refresh while authoring), `offline` (never
connect). `quarry lint` flags concatenated SQL and literal credentials;
`quarry diff` shows what a refresh changed.

## API tour

```typst
qr.sqlite(bytes, now: …, seed: …)     // open; or a dict of up to 8 named sources
qr.query(db, sql, ..params)           // envelope: columns, dict rows, stats
qr.try-query(db, sql, ..params)       // catchable variant
qr.value / qr.column / qr.row         // scalar / array / first-row accessors
qr.sql-table(env, …)                  // renderer: repeat headers, zebra, groups,
                                      //   subtotals, totals, row limits, hooks
qr.number / currency / percent / date / unit   // formatters, locale-aware
qr.xy / qr.series / qr.cetz-points    // chart adapters (lilaq, cetz-plot)
qr.pivot / qr.sparkline               // crosstabs, inline microcharts
qr.schema(db) / qr.schema-doc(db)     // introspection → data-dictionary appendix
qr.sql-lib(read("queries.sql"))       // named queries from a .sql file
qr.query(db, sql, expect: (min-rows: 1, columns: ("region",)))  // assertions
```

Multi-source joins: `qr.sqlite((sales: read(…), refs: read(…)))` then
`SELECT … FROM sales.orders JOIN refs.regions USING (region_id)`.

Every public function has a runnable example in `tests/typst/` — the test
suite and the documentation are the same artifact.

## Honest limitations

- **Memory (in-WASM path):** the database is copied into plugin memory once
  and snapshotted once by `plugin.transition` — budget ≈3× the file size in
  host RAM (measured: a 26 MB database costs ~55 MB). Past ~100 MB, use the
  sidecar.
- **Speed (in-WASM path):** Typst runs plugins on an interpreter; measured
  penalty 3.5×–63× vs native (see docs/phase0-findings.md). Dozens of queries
  over tens of MB stay comfortably fast; heavy analytics belong in the
  sidecar.
- **No network at compile time, ever.** Typst's sandbox forbids it,
  deliberately; quarry turns that into the lockfile workflow rather than
  fighting it.
- **WAL databases** are normalized on the fly when cleanly checkpointed
  (with a warning); otherwise you get an error with the exact `sqlite3`
  command to fix the file.
- **`localtime` is disabled** (no timezone database in the sandbox);
  timezone rendering is a formatting decision in the document.
- **Discovery caveat (sidecar):** remote query results that never reach page
  content need `#qr.declare(env)` or a `[sources.X.queries.NAME]` declaration
  so `quarry sync` can see them.
- **Analytics is sidecar-only** — an in-WASM analytical engine failed the
  size/speed kill criterion, measured, not guessed (docs/phase0-findings.md).
- **One live native connection per process** (engine global state); the CLI
  serializes execution. The WASM path is unaffected.
- Postgres/MySQL drivers are integration-tested against real servers in
  nightly CI, not in the default local gate; the `NUMERIC` type deliberately
  errors with a `CAST` suggestion instead of guessing (docs/backend-contract.md).

## Building and testing

```console
$ ./tools/bootstrap.sh   # pinned toolchain, user-local, no sudo
$ just test              # the full gate:
                         #   unit + integration (65 native, 10 host)
                         #   523-query differential (byte-identical)
                         #   named regressions d01–d27
                         #   self-asserting Typst documents
                         #   golden renderings (PNG-exact)
                         #   824-input robustness sweep
                         #   size budget
$ just test-sidecar      # end-to-end CLI: discovery → cache → offline rebuild
$ just test-repro        # two builds, identical hashes
```

Vendored engine: **SQLite 3.53.4**, hash-verified, version-asserted at build,
test and doc level (they cannot drift — d24). Release binaries are built
twice in CI on independent runners and published only when the hashes match.

## Documentation

- docs/envelope-v1.md — the wire format (normative)
- docs/backend-contract.md — write your own backend
- docs/feature-matrix.md — every SQLite flag, with its reason
- docs/security-model.md — sandbox, read-only layers, credentials, threat model
- docs/phase0-findings.md — the measurements this design stands on
- examples/ — a full report template

## License

Apache-2.0 (see [LICENSE](LICENSE)). SQLite is public domain.
