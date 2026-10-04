# The backend contract

The core promise (04-project-description.md §4): **a document written against
one backend keeps working when the data moves to another.** One document API,
one result format, swappable engines. This file is the contract third-party
backends implement.

## Surface

Every backend answers four operations and lands on envelope v1
(docs/envelope-v1.md):

```
capabilities() -> { name, version, engine-version, sql-dialect,
                    supports: {…}, limits: {…} }
open(sources)  -> handle            # WASM: via plugin.transition()
query(handle, request) -> envelope  # ok/err envelope, ALWAYS (errors are data)
close(handle)  -> ()
```

The in-WASM backend additionally exports `describe`, `schema`, `probe`,
`cache_key` and `normalize_sql` (see crates/quarry-plugin/src/lib.rs for the
exact ABI: one i32 byte-length per argument, results via
`wasm_minimal_protocol_send_result_to_host`, return 0 = data / 1 = hard
error).

Sidecar backends implement the executor half instead
(crates/quarry-cli/src/exec/): given a request and a source declaration,
produce envelope bytes. The cache layer, key derivation, lockfile and
document integration are shared and not backend work.

## Non-negotiables

1. **Envelope v1, byte-disciplined.** No CBOR tags, integers never widened,
   BLOBs as byte strings, deterministic field order.
2. **Errors are data.** SQL failures return `ok: false` envelopes so
   `qr.try-query` works; hard failures are reserved for protocol misuse.
3. **Determinism.** Identical (open args, request) ⇒ byte-identical envelope,
   regardless of call order or instance reuse. No wall-clock, RNG or
   allocation-order leakage into results. (The purity suite runs every
   backend candidate through fresh-vs-dirty instance comparisons.)
4. **Read-only.** A backend that can write anything is not a quarry backend.
5. **Caps truncate, never fail** — `truncated: true` plus a warning.
6. **Declared capabilities are tested claims.** Whatever `supports` says is
   exercised by a test; whatever it denies is proven absent (d19).

## Shipped backends

| backend | where | engine | notes |
|---|---|---|---|
| sqlite (in-WASM) | plugin, zero install, web-app-capable | vendored SQLite 3.53.4, real libm via wasi-libc | the Phase-1 foundation |
| sqlite (sidecar) | `quarry` CLI | the same crate, native | full speed, no size ceiling |
| analytics | sidecar | Apache DataFusion (native) | CSV/Parquet/NDJSON via `files`/`tables`; sidecar-only per the measured R-01 kill criterion (docs/phase0-findings.md) |
| postgres | sidecar | native driver, rustls | read-only txn, statement_timeout, `:name`/`?` → `$n` bridge |
| mysql | sidecar | native driver, rustls | read-only session, max_execution_time |

## Dialects

Per Q-07 there is **no SQL dialect abstraction**: users write the backend's
SQL. The engine fails early with capability information instead of failing
deep. The one convenience: quarry's placeholder styles are bridged where the
wire protocol demands it (Postgres `$n`).

## Type mapping summary

| backend | integer | real | text | blob | bool | temporal | decimal/numeric |
|---|---|---|---|---|---|---|---|
| sqlite | i64 exact | f64 | UTF-8 (lossy for invalid) | bytes | 0/1 + `BOOLEAN` decltype coercion | ISO text / epoch int / julian real + decltype coercion | numeric affinity passthrough |
| analytics | all int widths → i64 (u64 > i63 → text) | f32/f64 → f64 | utf8 | binary | native bool | arrow canonical ISO text, logical date/datetime/time | rendered as text, logical numeric |
| postgres | int2/4/8 → i64 | float4/8 → f64 | text/varchar/bpchar/name | bytea | native bool | date/time/timestamp(tz) → ISO text | **unmapped by design** — `CAST` explicitly (`::float8`, `::text`); the driver errors rather than guessing |
| mysql | signed → i64, unsigned > i63 → text | float/double → f64 | non-binary bytes → text | binary bytes → blob | tinyint(1) arrives as integer | DATE/DATETIME/TIMESTAMP/TIME → ISO text | decimal → text, logical numeric |

Everything unmappable errors loudly with a `CAST` suggestion — quarry never
guesses a value into a maybe-wrong shape.
