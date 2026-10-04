// SQL argument handling: raw-block extraction (F-29) and typed parameter
// conversion (F-11). Binding is the only supported way to make SQL dynamic —
// string concatenation is documented as unsupported and linted by the CLI.

#import "util.typ": iso-display

// Accept SQL as a string, a ```sql raw block``` (editors highlight it), or an
// array of either (joined into a multi-statement script).
#let extract-sql(x) = {
  if type(x) == str {
    x
  } else if type(x) == content and x.func() == raw {
    x.text
  } else if type(x) == array {
    x.map(extract-sql).join(";\n")
  } else {
    panic(
      "quarry: expected SQL as a string, a ```sql raw block``` or an array of those, got "
        + str(type(x)),
    )
  }
}

// Convert one Typst value into its CBOR-transportable parameter form.
// Special shapes ({"@dt": …}, {"@dur": …}, {"@json": …}) carry types CBOR
// tags would normally carry — Typst's cbor() rejects tags, so quarry never
// uses them (envelope rule).
#let to-param(v, name) = {
  let t = type(v)
  if v == none or t == bool or t == int or t == float or t == str or t == bytes {
    v
  } else if t == datetime {
    ("@dt": iso-display(v))
  } else if t == duration {
    ("@dur": v.seconds())
  } else if t == array or t == dictionary {
    ("@json": json.encode(v))
  } else if t == content and v.func() == raw {
    v.text
  } else if t == decimal {
    // Exactness first: decimals travel as text, SQLite compares them as
    // numeric when the column has numeric affinity.
    str(v)
  } else {
    panic(
      "quarry: parameter " + name + " has unsupported type " + str(t)
        + " — bind int, float, str, bytes, bool, none, datetime, duration, array or dictionary",
    )
  }
}

// Named arguments of query() that configure the request rather than bind SQL
// parameters.
#let option-keys = (
  "params", "expect", "coerce", "max-rows", "max-bytes", "multi-statement",
  "positional-rows", "row-offset", "row-limit", "source",
)

// Split the trailing named arguments of query(…) into (request options,
// bound parameters); positional binding comes via `params: (…)`.
#let split-args(named) = {
  let options = (:)
  let bound = (:)
  for (k, v) in named.pairs() {
    if k in option-keys {
      options.insert(k, v)
    } else {
      bound.insert(k, to-param(v, ":" + k))
    }
  }
  (options: options, bound: bound)
}

// Build the request map the engine parses (crates/quarry-engine/src/request.rs).
#let build-request(sql, named) = {
  let split = split-args(named)
  let options = split.options
  let req = (sql: extract-sql(sql))

  if "params" in options {
    if split.bound.len() > 0 {
      panic(
        "quarry: mixing positional params: (…) with named parameters ("
          + split.bound.keys().join(", ") + ") is not supported — use one style",
      )
    }
    let pos = options.params
    if type(pos) != array { pos = (pos,) }
    req.insert("params", pos.enumerate().map(((i, v)) => to-param(v, "?" + str(i + 1))))
  } else if split.bound.len() > 0 {
    req.insert("params", split.bound)
  }

  let engine-opts = (:)
  for key in ("max-rows", "max-bytes", "multi-statement", "positional-rows", "row-offset", "row-limit") {
    if key in options { engine-opts.insert(key, options.at(key)) }
  }
  if "source" in options { req.insert("source", options.source) }
  if engine-opts.len() > 0 { req.insert("options", engine-opts) }
  (request: req, expect: options.at("expect", default: none), coerce: options.at("coerce", default: auto))
}
