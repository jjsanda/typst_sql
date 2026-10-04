// Golden: group headers, subtotals, totals, empty state, formatters, sparkline.
#set page(width: 14cm, height: auto, margin: 1cm)
#import "/packages/quarry/lib.typ" as qr
#let db = qr.sqlite(read("/tests/fixtures/generated/basic.sqlite", encoding: none),
  now: datetime(year: 2026, month: 1, day: 20), seed: 42)
#let env = qr.query(db, ```sql
  SELECT region_group, region, SUM(amount) AS total, COUNT(*) AS n, AVG(price) AS avg_price
  FROM orders JOIN regions ON regions.name = orders.region
  GROUP BY region ORDER BY region_group, total DESC
```)
#qr.sql-table(
  env,
  columns: (
    (key: "region", title: [Region]),
    (key: "total", title: [Revenue], format: qr.currency("EUR", locale: "de")),
    (key: "n", title: [Orders]),
    (key: "avg_price", title: [Avg], format: qr.number(digits: 2)),
  ),
  group-by: "region_group",
  group-totals: true,
  totals: (total: "sum", n: "sum"),
  zebra: true,
  caption: [Grouped, subtotalled, totalled],
)
#qr.sql-table(qr.query(db, "SELECT 1 AS x WHERE 0"), on-empty: [_Empty as expected._])
Trend #qr.sparkline((1, 4, 2, 8, 5, 7), fill: blue.lighten(85%)) inline.
#qr.sql-table(qr.pivot(
  qr.query(db, "SELECT region, fiscal_year, amount FROM orders"),
  rows: "region", cols: "fiscal_year", values: "amount",
), caption: [Pivot])
