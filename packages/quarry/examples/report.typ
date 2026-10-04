// A complete report template (F-41): monthly business review over a SQLite
// extract. Compile from the repository root:
//
//   typst compile --root . packages/quarry/examples/report.typ \
//     --input quarry-now=2026-01-20T00:00:00Z
//
// Swap the read() for your own database; everything else adapts.

#import "/packages/quarry/lib.typ" as qr

#set page(margin: 2cm, numbering: "1 / 1")
#set text(font: "New Computer Modern", size: 10pt)
#show heading.where(level: 1): set text(size: 17pt)

#let db = qr.sqlite(
  read("/tests/fixtures/generated/basic.sqlite", encoding: none),
  seed: 1,
)

#let year = 2026
#let orders = qr.query(db, ```sql
  SELECT region, SUM(amount) AS total, COUNT(*) AS n, AVG(price) AS avg_price
  FROM orders WHERE fiscal_year = :year
  GROUP BY region ORDER BY total DESC
```, year: year, expect: (min-rows: 1, columns: ("region", "total")))

#align(center)[
  #text(size: 21pt)[*Business Review #year*]\
  #text(fill: luma(100))[
    generated #qr.value(db, "SELECT date('now')") ·
    #qr.number()(qr.value(db, "SELECT COUNT(*) FROM orders WHERE fiscal_year = :y", y: year))
    orders analysed
  ]
]

= Revenue by region

#qr.sql-table(
  orders,
  columns: (
    (key: "region", title: [Region]),
    (key: "total", title: [Revenue], format: qr.currency("USD", digits: 0)),
    (key: "n", title: [Orders], format: qr.number()),
    (key: "avg_price", title: [Avg. price], format: qr.currency("USD")),
  ),
  zebra: true,
  totals: (total: "sum", n: "sum"),
  caption: [Revenue and order counts, FY#year],
)

The leading region is *#orders.rows.first().region* with
#qr.currency("USD", digits: 0)(orders.rows.first().total) across
#orders.rows.first().n orders. Daily trend:
#qr.sparkline(
  qr.column(db, ```sql
    SELECT SUM(amount) FROM orders WHERE fiscal_year = :y
    GROUP BY strftime('%d', created_at) ORDER BY strftime('%d', created_at)
  ```, y: year),
  fill: blue.lighten(85%),
)

= Year-over-year by region

#qr.sql-table(
  qr.pivot(
    qr.query(db, "SELECT region, fiscal_year, amount FROM orders"),
    rows: "region", cols: "fiscal_year", values: "amount",
  ),
  caption: [Revenue crosstab (all fiscal years)],
)

= Largest orders

#qr.sql-table(
  qr.query(db, ```sql
    SELECT id, region, amount, created_at FROM orders
    WHERE fiscal_year = :y ORDER BY amount DESC
  ```, y: year),
  columns: (
    (key: "id", title: [\#]),
    (key: "region", title: [Region]),
    (key: "amount", title: [Amount], format: qr.currency("USD", digits: 0),
     cell: (v, row, i) => if v > 8500 { text(fill: red.darken(20%))[#qr.currency("USD", digits: 0)(v)] }),
    (key: "created_at", title: [Created], format: qr.date()),
  ),
  max-rows: 8,
  zebra: true,
)

= Data dictionary

This report documents its own source (audit teams appreciate it):

#qr.schema-doc(qr.schema(db), heading-level: 2)
