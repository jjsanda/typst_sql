// Chart adapters (F-19): shape query results for lilaq and cetz-plot.
// quarry never draws — it feeds the packages that do (non-goal 3).

/// Two positional arrays for spreading into lilaq:
/// `lq.bar(..qr.xy(env, x: "region", y: "total"))`.
/// Categorical x (strings) becomes 0..n indices — pair with qr.xticks for the
/// labels, since plotting libraries want numbers on the axis.
#let xy(env, x: none, y: none, categorical: auto) = {
  if x == none or y == none { panic("quarry: xy needs x: and y: column keys") }
  let xs = env.rows.map(r => r.at(x, default: none))
  let ys = env.rows.map(r => r.at(y, default: none))
  let cat = if categorical == auto { xs.any(v => type(v) == str) } else { categorical }
  if cat {
    (range(xs.len()), ys)
  } else {
    (xs, ys)
  }
}

/// Tick specification for categorical axes: array of (position, label).
/// lilaq: `lq.diagram(xaxis: (ticks: qr.xticks(env, x: "region")), …)`.
#let xticks(env, x: none) = {
  if x == none { panic("quarry: xticks needs an x: column key") }
  env.rows.enumerate().map(((i, r)) => (i, str(r.at(x, default: ""))))
}

/// Split rows into series by a discriminator column: array of
/// (label, xs, ys) dictionaries, one per distinct `by` value (in row order).
#let series(env, x: none, y: none, by: none) = {
  if x == none or y == none or by == none {
    panic("quarry: series needs x:, y: and by: column keys")
  }
  let order = ()
  let groups = (:)
  for r in env.rows {
    let key = str(r.at(by, default: ""))
    if key not in groups {
      groups.insert(key, ())
      order.push(key)
    }
    groups.at(key).push(r)
  }
  order.map(key => (
    label: key,
    xs: groups.at(key).map(r => r.at(x, default: none)),
    ys: groups.at(key).map(r => r.at(y, default: none)),
  ))
}

/// Points for cetz-plot's `plot.add`: array of (x, y) pairs.
#let cetz-points(env, x: none, y: none) = {
  if x == none or y == none { panic("quarry: cetz-points needs x: and y: column keys") }
  env.rows.map(r => (r.at(x, default: 0), r.at(y, default: 0)))
}
