# Result envelope v1 (normative)

The single wire format every backend produces and every document consumes —
in-WASM SQLite, sidecar SQLite, DataFusion analytics, Postgres, MySQL. A
document cannot tell where its data came from; that is the backend contract's
central promise. Versioned so it can evolve without breaking documents.

## Encoding rules (non-negotiable)

- CBOR, definite-length maps and arrays only.
- **No CBOR tags anywhere.** Typst's `cbor()` rejects tags with an
  uncatchable error; the tag-freeness of every envelope is asserted by a unit
  test that walks the encoder output.
- Integers are CBOR major type 0/1 — never widened to floats. SQLite integers
  are i64 and Typst integers are i64; the full range round-trips exactly
  (regression d02).
- Floats are 64-bit CBOR floats (major 7). (Encoders may use the shortest
  faithful float encoding; consumers must accept any width.)
- BLOBs are CBOR byte strings (major 2) — they decode to Typst `bytes`,
  directly usable by `image()` (d06).
- Field order is fixed as listed below, making identical results
  byte-identical — the property the differential suite and Typst's
  memoization both key on.
- Typed values that CBOR tags would normally carry use tag-free map shapes
  in *requests*: `{"@dt": "2026-01-15 12:00:00"}` (datetime → TEXT),
  `{"@dur": seconds}` (duration → REAL), `{"@json": text}` (collections →
  JSON TEXT).

## Success envelope

```
{
  "version": 1,
  "ok": true,
  "columns": [
    { "name": tstr,          ; unique per envelope (dupes become name_2, name_3…)
      "type": tstr,          ; logical type, see vocabulary
      "decltype": tstr/null, ; the declared column type, verbatim
      "nullable": bool,      ; observational: a NULL appeared in this result
      "table": tstr/null,    ; origin table when known
      "origin": tstr/null }  ; origin column when known
  ],
  "rows": [ { "<column-name>": <value>, … } ],   ; or [[…]] with positional-rows
  "stats": {
    "row-count": uint,
    "truncated": bool,       ; a cap stopped the result — never a hard failure
    "source": tstr,
    "statements": uint,      ; statements executed (D-07: all of them, always)
    "clock": { "value": tstr/null, "origin": "explicit"/"input"/"today"/"sidecar"/"none" },
    "seed": int/null,
    "engine": tstr,          ; e.g. "sqlite 3.53.4"
    "placeholder": true      ; ONLY on discovery placeholders, absent otherwise
  },
  "warnings": [ { "code": tstr, "message": tstr } ]
}
```

`multi-statement: "all"` replaces `columns`/`rows` with
`"results": [ { "columns", "rows", "row-count", "truncated" } ]`.

Sidecar-cached envelopes additionally carry `"cached": { "fetched", "digest",
"kind" }`, added by the document layer after reading the cache.

## Error envelope

Errors are **data** (plugin return code 0) — that is what makes
`qr.try-query` possible and every error path testable in-document (D-17,
D-25). Hard traps are reserved for protocol violations and `open` failures
under `plugin.transition` (whose results Typst discards).

```
{
  "version": 1,
  "ok": false,
  "error": {
    "code": tstr,            ; stable machine code, see below
    "message": tstr,
    "sqlite-code": int/null,
    "sqlite-extended": int/null,
    "sql": tstr/null,
    "statement-index": uint/null,
    "offset": int/null,      ; byte offset of the offending token in sql
    "length": int/null,      ; token length for the caret
    "hint": tstr/null,       ; e.g. "did you mean \"customers\"?"
    "source": tstr/null,
    "engine": tstr
  }
}
```

### Error codes

| code | meaning |
|---|---|
| `protocol-error` | malformed request CBOR / misuse of the plugin ABI |
| `source-invalid` | the bytes are not a usable SQLite database |
| `source-unsupported` | recognizably SQLite, but a shape we refuse (future format, unclean WAL) |
| `clock-required` | date/time SQL used with `now: none` and no injected clock (D-04's fix) |
| `read-only-violation` | write/DDL/transaction statements outside the temp schema |
| `binding-error` | missing, unused, mistyped or mismatched parameters |
| `sqlite-error` | any other SQLite-reported error (syntax, missing table, …) |
| `sql-error` | remote-backend SQL error (DataFusion/Postgres/MySQL), cached like data |
| `multi-statement-rejected` | more than one statement with `multi-statement: "reject"` |
| `open-failed` | internal failure class (allocation, engine bookkeeping) |

## Logical type vocabulary

`integer` · `real` · `numeric` · `text` · `blob` · `boolean` · `date` ·
`datetime` · `time` · `json` · `null` · `any`

Derived from the declared column type first (affinity rules plus the
`BOOLEAN`/`DATE`/`DATETIME`/`TIMESTAMP`/`TIME`/`JSON` conventions), then from
observed storage classes for expression columns. The Typst layer coerces on
this: temporal columns become `datetime` (from ISO text, Unix epoch integers
or Julian-day reals — resolved by the declared type, never guessed), boolean
columns become `bool`.

## Request map (document → engine)

```
{
  "sql": tstr / [tstr],
  "params": { name: value } / [value…] / absent,
  "source": tstr,                       ; sidecar routing + cache keys
  "options": {
    "max-rows": uint/null (100 000),
    "max-bytes": uint/null (64 MiB),
    "multi-statement": "last"/"all"/"reject" ("last"),
    "positional-rows": bool (false),
    "row-offset": uint (0),
    "row-limit": uint/null
  }
}
```

Caps truncate with `stats.truncated = true` plus a warning — never a hard
failure (D-12). Every statement always executes (D-07); `"last"` returns the
final result set that produced columns.
