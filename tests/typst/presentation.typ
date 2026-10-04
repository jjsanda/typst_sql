#import "/packages/quarry/lib.typ" as qr

#let db = qr.sqlite(read("/tests/fixtures/generated/basic.sqlite", encoding: none),
  now: datetime(year: 2026, month: 1, day: 20), seed: 42)

// -- formatters ---------------------------------------------------------------
#assert.eq(qr.number(digits: 2)(1234567.891), "1,234,567.89")
#assert.eq(qr.number(digits: 0)(1234567.891), "1,234,568")
#assert.eq(qr.number()(1234567), "1,234,567")
#assert.eq(qr.number(group: false)(1234567), "1234567")
#assert.eq(qr.number(digits: 2, locale: "de")(1234567.5), "1.234.567,50")
#assert.eq(qr.number(sign: true)(42), "+42")
#assert.eq(qr.number()(-1234), sym.minus + "1,234")
#assert.eq(qr.currency("USD")(1999.5), "$1,999.50")
#assert.eq(qr.currency("JPY")(1999.5), "¥2,000")
#assert.eq(qr.percent()(0.324), "32.4" + sym.space.thin + "%")
#assert.eq(qr.percent(of: 1.0, digits: 0)(0.5), "50" + sym.space.thin + "%")
#assert.eq(qr.date()(datetime(year: 2026, month: 3, day: 5)), "2026-03-05")
#assert.eq(qr.date()("2026-03-05 12:00:00"), "2026-03-05")
#assert.eq(qr.unit("kg", digits: 1)(12.55), "12.6" + sym.space.thin + "kg")
#assert.eq(qr.auto-format(none, "any"), sym.dash.em)
#assert.eq(qr.auto-format(9223372036854775807, "integer"), "9,223,372,036,854,775,807")

// -- sql-table ---------------------------------------------------------------
#let regions = qr.query(db, ```sql
  SELECT region, region_group, SUM(amount) AS total, COUNT(*) AS n
  FROM orders JOIN regions USING (region_id)
  GROUP BY region ORDER BY region_group, total DESC
```)
#qr.sql-table(
  regions,
  columns: (
    (key: "region", title: [Region]),
    (key: "total", title: [Revenue], format: qr.currency("USD", digits: 0)),
    (key: "n", title: [Orders], format: qr.number()),
  ),
  zebra: true,
  totals: (total: "sum", n: "sum"),
  group-by: "region_group",
  caption: [Revenue by region],
)

// empty state
#assert.eq(qr.sql-table(qr.query(db, "SELECT * FROM orders WHERE 0"), on-empty: [nothing]), [nothing])

// max-rows note + custom cell hook
#qr.sql-table(
  qr.query(db, "SELECT id, region, amount FROM orders ORDER BY id"),
  max-rows: 5,
  cell: (v, row, key, i) => if key == "amount" and v > 8000 { text(red)[#v] } else { none },
)

// -- charts ------------------------------------------------------------------
#let (xs, ys) = qr.xy(regions, x: "region", y: "total")
#assert.eq(xs, (0, 1, 2, 3))
#assert.eq(ys.len(), 4)
#assert.eq(qr.xticks(regions, x: "region").first().at(1), regions.rows.first().region)
#let numeric = qr.query(db, "SELECT fiscal_year AS x, sum(amount) AS y FROM orders GROUP BY 1 ORDER BY 1")
#let (nx, ny) = qr.xy(numeric, x: "x", y: "y")
#assert.eq(nx, (2025, 2026))
#let by-region = qr.query(db, "SELECT fiscal_year, region, sum(amount) AS s FROM orders GROUP BY 1, 2 ORDER BY 1, 2")
#let ser = qr.series(by-region, x: "fiscal_year", y: "s", by: "region")
#assert.eq(ser.len(), 4)
#assert.eq(ser.first().xs.len(), 2)
#assert.eq(qr.cetz-points(numeric, x: "x", y: "y").len(), 2)

// -- pivot -------------------------------------------------------------------
#let piv = qr.pivot(by-region, rows: "region", cols: "fiscal_year", values: "s", agg: "sum")
#assert.eq(piv.rows.len(), 4)
#assert.eq(piv.columns.len(), 3)
#assert.eq(piv.rows.first().at("2025") + piv.rows.first().at("2026"),
  qr.value(db, "SELECT sum(amount) FROM orders WHERE region = :r", r: piv.rows.first().region))
#qr.sql-table(piv)

// -- sparkline ---------------------------------------------------------------
#qr.sparkline((3, 1, 4, 1, 5, 9, 2, 6), fill: blue.lighten(80%))
#qr.sparkline((1, 1, 1))

// -- sql-lib -----------------------------------------------------------------
#let lib = qr.sql-lib("-- comment before\n-- name: totals\nSELECT count(*) AS n FROM orders\n-- name: one\nSELECT 1 AS one")
#assert.eq(qr.value(db, lib.totals), 200)
#assert.eq(qr.value(db, lib.one), 1)

// -- schema-doc --------------------------------------------------------------
#qr.schema-doc(qr.schema(db), heading-level: 4)

Presentation OK
