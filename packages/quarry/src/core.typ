// The quarry core API: opening sources, running queries, typed results.
//
// Design rules (04-project-description.md §5), each traceable to an incumbent
// defect: free functions, not dictionary methods; rows are dictionaries;
// values are bound, never concatenated; errors are catchable where asked.

#import "coerce.typ": check-expect, coerce-envelope
#import "errors.typ": fail, try-result
#import "params.typ": build-request, extract-sql
#import "util.typ": datetime-to-unix-us, iso-display, parse-iso

#let _plugin = plugin("../quarry-sqlite.wasm")

// -- clock and seed resolution (F-03, fixing D-04/D-05) -----------------------

// now: auto → --input quarry-now=<ISO-8601>, else datetime.today() (date-only).
// now: none → hard-error mode: any date/time SQL function fails loudly.
// now: datetime or ISO string → explicit.
#let resolve-now(now) = {
  if now == auto {
    let input = sys.inputs.at("quarry-now", default: none)
    if input != none {
      let dt = parse-iso(input)
      if dt == none {
        panic("quarry: --input quarry-now=" + input + " is not ISO-8601 (expected e.g. 2026-07-30 or 2026-07-30T12:00:00Z)")
      }
      (us: datetime-to-unix-us(dt), origin: "input", display: iso-display(dt))
    } else {
      let today = datetime.today()
      (us: datetime-to-unix-us(today), origin: "today", display: iso-display(today))
    }
  } else if now == none {
    (us: none, origin: "none", display: none)
  } else if type(now) == datetime {
    (us: datetime-to-unix-us(now), origin: "explicit", display: iso-display(now))
  } else if type(now) == str {
    let dt = parse-iso(now)
    if dt == none { panic("quarry: now: " + now + " is not ISO-8601") }
    (us: datetime-to-unix-us(dt), origin: "explicit", display: iso-display(dt))
  } else {
    panic("quarry: now: expects auto, none, a datetime or an ISO-8601 string")
  }
}

#let resolve-seed(seed) = {
  if seed == auto {
    let input = sys.inputs.at("quarry-seed", default: none)
    if input != none { int(input) } else { 0 }
  } else if type(seed) == int {
    seed
  } else {
    panic("quarry: seed: expects auto or an integer")
  }
}

// -- opening ------------------------------------------------------------------

#let _open-fns = (
  _plugin.open_1, _plugin.open_2, _plugin.open_3, _plugin.open_4,
  _plugin.open_5, _plugin.open_6, _plugin.open_7, _plugin.open_8,
)

/// Open one or more SQLite databases for querying.
///
/// - src: `bytes` (one database) or a dictionary `(name: bytes, …)` of up to
///   8 named sources, enabling cross-source joins (F-20):
///   `SELECT * FROM sales.orders JOIN refs.regions USING (region_id)`.
/// - now: the injected clock (F-03). See resolve-now above.
/// - seed: the injected random seed; same seed ⇒ same random() stream.
///
/// Returns an opaque handle for qr.query and friends.
#let sqlite(src, now: auto, seed: auto) = {
  let (names, blobs) = if type(src) == bytes {
    ((), (src,))
  } else if type(src) == dictionary {
    if src.len() == 0 { panic("quarry: sqlite() got an empty source dictionary") }
    if src.len() > 8 { panic("quarry: at most 8 sources per connection (got " + str(src.len()) + ")") }
    for (k, v) in src.pairs() {
      if type(v) != bytes {
        panic("quarry: source " + k + " must be bytes — use read(\"…\", encoding: none)")
      }
    }
    (src.keys(), src.values())
  } else {
    panic("quarry: sqlite() expects bytes or a dictionary of named sources, got " + str(type(src)))
  }

  let clock = resolve-now(now)
  let seed-val = resolve-seed(seed)
  let meta = (
    now: clock.us,
    "clock-origin": clock.origin,
    "clock-display": clock.display,
    seed: seed-val,
  )
  if names.len() > 0 { meta.insert("sources", names) }

  let open-fn = _open-fns.at(blobs.len() - 1)
  let mod = plugin.transition(open-fn, cbor.encode(meta), ..blobs)
  (
    quarry: "sqlite",
    mod: mod,
    sources: if names.len() > 0 { names } else { ("main",) },
    clock: clock,
    seed: seed-val,
  )
}

/// Catchable pre-check (uses the header probe, no transition): returns
/// (ok: bool, info | error). Lets a template degrade gracefully on a bad file.
#let try-sqlite(src, now: auto, seed: auto) = {
  let first = if type(src) == bytes { src } else { src.values().first() }
  let probe = cbor(_plugin.probe(first))
  if not probe.ok {
    return (ok: false, error: probe, db: none)
  }
  (ok: true, error: none, db: sqlite(src, now: now, seed: seed))
}

/// Header probe without opening: page size/count, WAL flag, validity.
#let probe(src) = cbor(_plugin.probe(src))

#let _is-db(db) = type(db) == dictionary and "quarry" in db

#let _guard-db(db, who) = {
  if not _is-db(db) {
    panic(
      "quarry: " + who + " expects a database handle from qr.sqlite(…) or qr.remote(…), got "
        + repr(db).slice(0, calc.min(60, repr(db).len())),
    )
  }
}

// -- querying (F-11/F-12/F-13/F-17) -------------------------------------------

#let _run(db, sql, named) = {
  let built = build-request(extract-sql(sql), named)
  let env = if db.quarry == "remote" {
    // resolved through the sidecar cache (sidecar.typ supplies the hook,
    // avoiding an import cycle)
    (db.run)(db, built.request)
  } else {
    cbor((db.mod.query)(cbor.encode(built.request)))
  }
  if env.ok {
    env = coerce-envelope(env, built.coerce)
    // Discovery placeholders are empty by construction; checking
    // expectations against them would fail every cold-checkout compile.
    if not env.at("stats", default: (:)).at("placeholder", default: false) {
      check-expect(env, built.expect, built.request.sql)
    }
  }
  (env: env, built: built)
}

/// Run SQL, return the result envelope. Fails the compile with a rendered
/// diagnostic on any error. `..args`: named SQL parameters, plus options
/// (params:, expect:, coerce:, max-rows:, max-bytes:, multi-statement:,
/// positional-rows:, row-offset:, row-limit:).
#let query(db, sql, ..args) = {
  _guard-db(db, "query")
  let r = _run(db, sql, args.named())
  if not r.env.ok { fail(r.env.error) }
  r.env
}

/// Like query, but never aborts: returns (ok, value, error, rendered).
#let try-query(db, sql, ..args) = {
  _guard-db(db, "try-query")
  try-result(_run(db, sql, args.named()).env)
}

/// Scalar convenience: first column of the first row (none when empty).
#let value(db, sql, ..args) = {
  let env = query(db, sql, ..args)
  if env.rows.len() == 0 { return none }
  let row = env.rows.first()
  if env.columns.len() == 0 { return none }
  row.at(env.columns.first().name, default: none)
}

/// One column as an array.
#let column(db, sql, ..args) = {
  let env = query(db, sql, ..args)
  if env.columns.len() == 0 { return () }
  let key = env.columns.first().name
  env.rows.map(r => r.at(key, default: none))
}

/// First row as a dictionary (none when empty).
#let row(db, sql, ..args) = {
  let env = query(db, sql, ..args)
  if env.rows.len() == 0 { none } else { env.rows.first() }
}

/// The rows of an envelope (accessor for symmetry).
#let rows(env) = env.rows

// -- metadata -----------------------------------------------------------------

/// Statement description without execution: columns, parameters, read-only.
#let describe(db, sql) = {
  _guard-db(db, "describe")
  let req = (sql: extract-sql(sql))
  let out = cbor((db.mod.describe)(cbor.encode(req)))
  if "ok" in out and not out.ok { fail(out.error) }
  out
}

/// Full schema introspection (F-30): tables, columns, keys, indexes, counts.
#let schema(db, row-counts: true) = {
  _guard-db(db, "schema")
  let out = cbor((db.mod.schema)(cbor.encode(row-counts)))
  if "ok" in out and not out.ok { fail(out.error) }
  out
}

/// Backend capability declaration (works on any handle or the bare package).
#let capabilities(db: none) = {
  if db != none {
    cbor((db.mod.capabilities)())
  } else {
    cbor(_plugin.capabilities())
  }
}

/// Content-addressed cache key for a request (used by the sidecar flow; the
/// CLI computes the identical key with the same code, natively).
#let cache-key(source, sql, ..args) = {
  let built = build-request(extract-sql(sql), args.named())
  let req = built.request
  req.insert("source", source)
  cbor(_plugin.cache_key(cbor.encode(req)))
}

/// Cache key from an already-built request map (sidecar internal).
#let cache-key-request(req) = cbor(_plugin.cache_key(cbor.encode(req)))

/// Canonicalized SQL (comments stripped, whitespace collapsed, literals kept).
#let normalize-sql(sql) = cbor(_plugin.normalize_sql(bytes(extract-sql(sql))))
