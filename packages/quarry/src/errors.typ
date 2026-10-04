// Diagnostics rendering (F-16/F-17, fixing D-17): a real compiler-style
// message with the SQL line, a caret under the offending token, the hint and
// the source — shared by the hard-failure path (qr.query panics with it) and
// the catchable path (qr.try-query returns it as `rendered`).

// Locate (line index, column, line text) for a byte offset in the SQL.
#let _locate(sql, offset) = {
  let lines = sql.split("\n")
  let consumed = 0
  for (i, line) in lines.enumerate() {
    let end = consumed + line.len()
    if offset <= end {
      return (line-no: i + 1, column: offset - consumed, line: line)
    }
    consumed = end + 1 // the newline
  }
  (line-no: lines.len(), column: 0, line: lines.last())
}

// Build the plain-text diagnostic for an error dict from the engine envelope.
#let render-error(e) = {
  let out = "quarry: " + e.message
  if e.at("sql", default: none) != none {
    let sql = e.sql
    if e.at("offset", default: none) != none {
      let loc = _locate(sql, e.offset)
      let len = calc.max(1, e.at("length", default: 1))
      out += "\n  │ " + loc.line
      out += "\n  │ " + " " * loc.column + "^" * calc.min(len, loc.line.len() - loc.column + 1)
    } else {
      let first = sql.split("\n").first().trim()
      if first.len() > 0 { out += "\n  │ " + first }
    }
  }
  if e.at("hint", default: none) != none {
    out += "\n  = " + e.hint
  }
  let source = e.at("source", default: none)
  let engine = e.at("engine", default: none)
  if source != none or engine != none {
    out += "\n  = source: " + if source != none { source } else { "?" }
    if engine != none { out += " (" + engine + ")" }
  }
  out
}

// Hard failure: qr.query and friends end compilation with the rendered
// diagnostic. The catchable variant is qr.try-query.
#let fail(e) = panic(render-error(e))

// Shape returned by try-query: (ok, value, error, rendered).
#let try-result(env) = {
  if env.ok {
    (ok: true, value: env, error: none, rendered: none)
  } else {
    (ok: false, value: none, error: env.error, rendered: render-error(env.error))
  }
}
