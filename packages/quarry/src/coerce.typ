// Typed coercion (F-13, fixing D-16): SQLite values → Typst native types,
// driven by the column's declared type — never guessed from the value alone.
//
// - date / datetime / time columns: ISO text, Unix epoch integers and Julian
//   day reals all become Typst `datetime`;
// - boolean columns: 0/1 become false/true;
// - blobs already arrive as `bytes` (CBOR byte strings, D-06);
// - everything else passes through untouched.
//
// Coercion is best-effort per value: a value that does not parse stays as
// stored, visibly — turning junk into a fabricated date would be a silent
// wrong answer, the exact defect class quarry exists to prevent.

#import "util.typ": julian-to-datetime, parse-iso, parse-time, unix-to-datetime

#let _coerce-temporal(v, kind) = {
  if type(v) == str {
    let parsed = if kind == "time" { parse-time(v) } else { parse-iso(v) }
    if parsed != none { parsed } else { v }
  } else if type(v) == int {
    // Unix epoch seconds (SQLite's third date convention).
    unix-to-datetime(v)
  } else if type(v) == float {
    // Julian day number.
    julian-to-datetime(v)
  } else {
    v
  }
}

#let _coerce-bool(v) = {
  if v == 0 { false } else if v == 1 { true } else { v }
}

// Apply coercion to an ok envelope. `spec` is:
//   auto        → coerce by column logical type (the default),
//   false/none  → no coercion,
//   dictionary  → per-column override: key → "datetime"|"date"|"time"|"boolean"|"none"
#let coerce-envelope(env, spec) = {
  if spec == false or spec == none { return env }
  let plan = (:)
  for col in env.columns {
    let want = if type(spec) == dictionary and col.name in spec {
      spec.at(col.name)
    } else {
      col.type
    }
    if want in ("date", "datetime", "time") {
      plan.insert(col.name, "temporal-" + want)
    } else if want == "boolean" {
      plan.insert(col.name, "boolean")
    }
  }
  if plan.len() == 0 { return env }
  env.rows = env.rows.map(row => {
    // positional-rows mode returns arrays; coercion applies to dict rows only
    if type(row) != dictionary { return row }
    for (name, action) in plan.pairs() {
      let v = row.at(name, default: none)
      if v == none { continue }
      if action == "boolean" {
        row.insert(name, _coerce-bool(v))
      } else {
        row.insert(name, _coerce-temporal(v, action.slice(9)))
      }
    }
    row
  })
  env
}

// Result assertions (F-43): declare expectations, fail the build when they
// break. Catches the failure mode nobody notices — a schema change that
// quietly empties a table into a valid-looking blank section.
#let check-expect(env, expect, sql) = {
  if expect == none { return }
  let rows = env.at("rows", default: ())
  if "min-rows" in expect and rows.len() < expect.at("min-rows") {
    panic(
      "quarry: expectation failed: query returned " + str(rows.len())
        + " rows, expected at least " + str(expect.at("min-rows")) + "\n  │ " + sql,
    )
  }
  if "max-rows" in expect and rows.len() > expect.at("max-rows") {
    panic(
      "quarry: expectation failed: query returned " + str(rows.len())
        + " rows, expected at most " + str(expect.at("max-rows")) + "\n  │ " + sql,
    )
  }
  if "columns" in expect {
    let have = env.columns.map(c => c.name)
    for want in expect.at("columns") {
      if want not in have {
        panic(
          "quarry: expectation failed: column \"" + want
            + "\" missing from result (have: " + have.join(", ") + ")\n  │ " + sql,
        )
      }
    }
  }
  if "non-null" in expect {
    for colname in expect.at("non-null") {
      for (i, row) in env.rows.enumerate() {
        if type(row) == dictionary and row.at(colname, default: none) == none {
          panic(
            "quarry: expectation failed: column \"" + colname
              + "\" is NULL in row " + str(i) + "\n  │ " + sql,
          )
        }
      }
    }
  }
}
