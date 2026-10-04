// Phase-4 conveniences that ride on the core:
// - schema-doc (F-30): a rendered data dictionary,
// - pivot (F-35): crosstab over an envelope,
// - sparkline (F-36): inline microchart with zero chart-package dependency,
// - sql-lib (F-33): named queries from an external .sql file.

#import "format.typ": auto-format

/// Render a data-dictionary appendix from qr.schema(db) output: per source,
/// per table: columns, types, keys, row counts. Reports that document their
/// own data source — a real differentiator for compliance users.
#let schema-doc(
  schema,
  heading-level: 2,
  show-indexes: false,
  header-fill: luma(230),
) = {
  for source in schema.sources {
    heading(level: heading-level, [Source: #raw(source.name)])
    for t in source.tables {
      heading(level: heading-level + 1, [
        #raw(t.name)
        #if t.kind == "view" [ _(view)_ ]
        #if t.at("row-count", default: none) != none [
          — #auto-format(t.at("row-count"), "integer") rows
        ]
      ])
      table(
        columns: (auto, auto, auto, 1fr),
        align: (left, left, center, left),
        table.header(
          table.cell(fill: header-fill)[*Column*],
          table.cell(fill: header-fill)[*Type*],
          table.cell(fill: header-fill)[*Constraints*],
          table.cell(fill: header-fill)[*References*],
        ),
        ..t.columns.map(c => {
          let constraints = ()
          if c.at("primary-key", default: false) { constraints.push("PK") }
          if c.at("not-null", default: false) { constraints.push("NOT NULL") }
          let fk = t.at("foreign-keys", default: ()).find(f => f.from == c.name)
          (
            raw(c.name),
            raw(if c.type == none or c.type == "" { "—" } else { c.type }),
            constraints.join(", "),
            if fk != none { raw(fk.table + "." + if fk.to == none { fk.from } else { fk.to }) } else { [] },
          )
        }).flatten(),
      )
      if show-indexes and t.at("indexes", default: ()).len() > 0 {
        [Indexes: #t.indexes.map(ix => raw(ix.name)).join(", ")]
      }
    }
  }
}

#let _agg-values(kind, values) = {
  let nums = values.filter(v => type(v) == int or type(v) == float)
  if kind == "count" {
    values.filter(v => v != none).len()
  } else if nums.len() == 0 {
    none
  } else if kind == "sum" {
    nums.sum()
  } else if kind == "avg" {
    nums.sum() / nums.len()
  } else if kind == "min" {
    calc.min(..nums)
  } else if kind == "max" {
    calc.max(..nums)
  } else if type(kind) == function {
    kind(values)
  } else {
    panic("quarry: pivot agg must be sum/avg/min/max/count or a function")
  }
}

/// Crosstab (F-35): pivot an envelope so distinct `cols` values become
/// columns. Returns an envelope-shaped dict renderable by qr.sql-table.
///
/// `qr.pivot(env, rows: "region", cols: "fiscal_year", values: "amount")`
#let pivot(env, rows: none, cols: none, values: none, agg: "sum", missing: none) = {
  if rows == none or cols == none or values == none {
    panic("quarry: pivot needs rows:, cols: and values: column keys")
  }
  let col-order = ()
  let row-order = ()
  let buckets = (:)
  for r in env.rows {
    let rk = str(r.at(rows, default: ""))
    let ck = str(r.at(cols, default: ""))
    if rk not in row-order { row-order.push(rk) }
    if ck not in col-order { col-order.push(ck) }
    let key = rk + "\u{0}" + ck
    if key not in buckets { buckets.insert(key, ()) }
    buckets.at(key).push(r.at(values, default: none))
  }
  let out-columns = (
    (name: rows, type: "text", decltype: none, nullable: false, table: none, origin: none),
  )
  for ck in col-order {
    out-columns.push((name: ck, type: "numeric", decltype: none, nullable: true, table: none, origin: none))
  }
  let out-rows = row-order.map(rk => {
    let row = ((rows): rk)
    for ck in col-order {
      let vals = buckets.at(rk + "\u{0}" + ck, default: ())
      row.insert(ck, if vals.len() == 0 { missing } else { _agg-values(agg, vals) })
    }
    row
  })
  (
    version: 1,
    ok: true,
    columns: out-columns,
    rows: out-rows,
    stats: env.at("stats", default: (:)),
    warnings: env.at("warnings", default: ()),
  )
}

/// Inline sparkline (F-36): a tiny line chart in running text, drawn with
/// std curve — no chart package needed.
/// `Revenue trend: #qr.sparkline(qr.column(db, "SELECT total FROM monthly"))`
#let sparkline(
  values,
  width: 6em,
  height: 1em,
  stroke: 0.6pt + blue.darken(20%),
  fill: none,
  baseline: 20%,
) = {
  let nums = values.filter(v => type(v) == int or type(v) == float).map(float)
  if nums.len() < 2 { return box(width: width, height: height) }
  let lo = calc.min(..nums)
  let hi = calc.max(..nums)
  let span = if hi - lo == 0 { 1.0 } else { hi - lo }
  let n = nums.len()
  let pts = nums.enumerate().map(((i, v)) => (
    width * (i / (n - 1)),
    height * (1 - (v - lo) / span),
  ))
  let segments = pts.slice(1).map(p => curve.line(p))
  box(
    width: width,
    height: height,
    baseline: baseline,
    {
      if fill != none {
        place(curve(
          fill: fill,
          stroke: none,
          curve.move((0 * width, height)),
          curve.line(pts.first()),
          ..segments,
          curve.line((width, height)),
          curve.close(),
        ))
      }
      place(curve(stroke: stroke, curve.move(pts.first()), ..segments))
    },
  )
}

/// Load a named-query library from a .sql file (F-33). Sections start with
/// `-- name: some-name`; text before the first marker is ignored.
///
/// ```sql
/// -- name: totals-by-region
/// SELECT region, sum(amount) AS total FROM orders GROUP BY region;
/// -- name: top-customers
/// SELECT * FROM customers LIMIT :n;
/// ```
/// `#let lib = qr.sql-lib(read("queries.sql"))` → `qr.query(db, lib.totals-by-region)`
#let sql-lib(source) = {
  let text = if type(source) == bytes { str(source) } else { source }
  let out = (:)
  let current = none
  let buffer = ()
  for line in text.split("\n") {
    let trimmed = line.trim()
    if trimmed.starts-with("-- name:") {
      if current != none {
        out.insert(current, buffer.join("\n").trim())
      }
      current = trimmed.slice(8).trim()
      if current == "" { panic("quarry: sql-lib: '-- name:' with an empty name") }
      buffer = ()
    } else if current != none {
      buffer.push(line)
    }
  }
  if current != none {
    out.insert(current, buffer.join("\n").trim())
  }
  if out.len() == 0 {
    panic("quarry: sql-lib found no '-- name: …' sections")
  }
  out
}
