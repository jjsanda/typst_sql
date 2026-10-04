// The table renderer (F-18, fixing D-18). The bar: nobody should ever need to
// hand-write a #table splat over query results again.
//
// - per-column title/align/width/format/cell;
// - table.header(repeat: true) so headers repeat across page breaks — the
//   structural fix the incumbent's renderer could not express;
// - zebra striping that stays aligned across group headers;
// - group headers (group-by) with optional per-group subtotals;
// - a totals row (sum/avg/min/max/count or any function) over ALL rows,
//   annotated when display rows were limited;
// - row limiting with an honest "… and N more" note (F-34);
// - empty-state content and a cell-level hook for conditional styling.

#import "format.typ": auto-format

#let _agg(kind, values) = {
  let nums = values.filter(v => type(v) == int or type(v) == float)
  if kind == "count" { return values.filter(v => v != none).len() }
  if nums.len() == 0 { return none }
  if kind == "sum" {
    nums.sum()
  } else if kind == "avg" {
    nums.sum() / nums.len()
  } else if kind == "min" {
    calc.min(..nums)
  } else if kind == "max" {
    calc.max(..nums)
  } else {
    panic("quarry: unknown total kind \"" + kind + "\" (use sum/avg/min/max/count or a function)")
  }
}

// Normalize the columns: argument into full specs.
#let _column-specs(env, columns) = {
  let by-name = (:)
  for c in env.columns { by-name.insert(c.name, c) }
  let specs = if columns == auto or columns == none {
    env.columns.map(c => (key: c.name))
  } else {
    columns.map(c => if type(c) == str { (key: c) } else { c })
  }
  specs.map(spec => {
    let meta = by-name.at(spec.key, default: (name: spec.key, type: "any"))
    let numeric = meta.type in ("integer", "real", "numeric")
    (
      key: spec.key,
      title: spec.at("title", default: [*#spec.key*]),
      align: spec.at("align", default: if numeric { right } else { left }),
      width: spec.at("width", default: auto),
      format: spec.at("format", default: none),
      cell: spec.at("cell", default: none),
      type: meta.type,
    )
  })
}

// Render one value through the hook chain: table-level cell hook, per-column
// cell hook, per-column format, auto-format.
#let _render-value(spec, row, index, cell-hook) = {
  let v = row.at(spec.key, default: none)
  if cell-hook != none {
    let custom = cell-hook(v, row, spec.key, index)
    if custom != none { return custom }
  }
  if spec.cell != none {
    let custom = (spec.cell)(v, row, index)
    if custom != none { return custom }
  }
  if spec.format != none { (spec.format)(v) } else { auto-format(v, spec.type) }
}

// One data row → array of table cells.
#let _data-row(specs, row, index, cell-hook, zebra, zebra-fill) = {
  let fill = if zebra and calc.odd(index) { zebra-fill } else { none }
  specs.map(spec => table.cell(fill: fill, _render-value(spec, row, index, cell-hook)))
}

// A totals/subtotals row → array of table cells.
#let _totals-row(specs, rows, totals, label, fill: none) = {
  specs.enumerate().map(((i, spec)) => {
    let content = if spec.key in totals {
      let kind = totals.at(spec.key)
      let values = rows.map(r => r.at(spec.key, default: none))
      let total = if type(kind) == function { kind(values) } else { _agg(kind, values) }
      let rendered = if spec.format != none { (spec.format)(total) } else { auto-format(total, spec.type) }
      [*#rendered*]
    } else if i == 0 {
      label
    } else {
      []
    }
    table.cell(fill: fill, stroke: (top: 0.6pt), content)
  })
}

/// Render a query envelope as a table. See the module comment for features;
/// `..table-args` passes through to #table (stroke, inset, fill, …).
#let sql-table(
  env,
  columns: auto,
  zebra: false,
  zebra-fill: luma(245),
  header-fill: luma(230),
  header-repeat: true,
  totals: (:),
  totals-label: [*Total*],
  group-by: none,
  group-header: auto,
  group-totals: false,
  group-fill: luma(238),
  max-rows: none,
  max-rows-note: auto,
  on-empty: [_No rows._],
  cell: none,
  caption: none,
  ..table-args,
) = {
  if "results" in env {
    panic("quarry: sql-table cannot render a multi-statement: \"all\" envelope — pick one of env.results")
  }
  // Discovery: a sidecar placeholder envelope carries its request; rendering
  // it emits the <quarry-request> metadata `quarry sync` extracts, so any
  // query whose result reaches the page is discovered automatically.
  let declaration = if "request" in env {
    [#metadata(env.at("request")) <quarry-request>]
  } else {
    none
  }
  let all-rows = env.rows
  if all-rows.len() == 0 {
    return if declaration == none { on-empty } else { declaration + on-empty }
  }
  if type(all-rows.first()) != dictionary {
    panic("quarry: sql-table needs dictionary rows — do not use positional-rows for rendering")
  }
  let specs = _column-specs(env, columns)
  let ncol = specs.len()

  let display-rows = all-rows
  let truncated-note = none
  if max-rows != none and all-rows.len() > max-rows {
    display-rows = all-rows.slice(0, max-rows)
    truncated-note = if max-rows-note == auto {
      [_… and #(all-rows.len() - max-rows) more rows_]
    } else if type(max-rows-note) == function {
      max-rows-note(all-rows.len() - max-rows)
    } else {
      max-rows-note
    }
  }
  // When the ENGINE already truncated (stats.truncated), totals over the
  // received rows would silently lie — annotate the label.
  let engine-truncated = env.at("stats", default: (:)).at("truncated", default: false)

  let body-cells = ()
  if group-by == none {
    for (i, row) in display-rows.enumerate() {
      body-cells += _data-row(specs, row, i, cell, zebra, zebra-fill)
    }
  } else {
    // Consecutive-run grouping: SQL (ORDER BY) stays in charge of ordering.
    let current = none
    let started = false
    let bucket = ()
    let index = 0
    for row in display-rows {
      let g = row.at(group-by, default: none)
      if not started or g != current {
        if started and group-totals and totals.len() > 0 and bucket.len() > 0 {
          body-cells += _totals-row(specs, bucket, totals, [_subtotal_])
        }
        bucket = ()
        started = true
        current = g
        let header-content = if group-header == auto {
          [*#auto-format(g, "any")*]
        } else {
          group-header(g)
        }
        body-cells.push(table.cell(colspan: ncol, fill: group-fill, header-content))
      }
      bucket.push(row)
      body-cells += _data-row(specs, row, index, cell, zebra, zebra-fill)
      index += 1
    }
    if group-totals and totals.len() > 0 and bucket.len() > 0 {
      body-cells += _totals-row(specs, bucket, totals, [_subtotal_])
    }
  }

  if totals.len() > 0 {
    let label = if engine-truncated {
      [#totals-label #super[_truncated_]]
    } else { totals-label }
    body-cells += _totals-row(specs, all-rows, totals, label)
  }
  if truncated-note != none {
    body-cells.push(table.cell(colspan: ncol, align: center, truncated-note))
  }

  let tbl = table(
    columns: specs.map(s => s.width),
    align: specs.map(s => s.align),
    ..table-args,
    table.header(
      repeat: header-repeat,
      ..specs.map(s => table.cell(fill: header-fill, s.title)),
    ),
    ..body-cells,
  )
  let rendered = if caption != none {
    figure(tbl, caption: caption)
  } else {
    tbl
  }
  if declaration == none { rendered } else { declaration + rendered }
}
