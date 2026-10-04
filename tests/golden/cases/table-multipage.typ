// d18 golden: a 120-row table across multiple pages — the header must repeat
// on every page (table.header repeat), numbers right-aligned and formatted.
#set page(width: 12cm, height: 8cm, margin: 1cm)
#import "/packages/quarry/lib.typ" as qr
#let db = qr.sqlite(read("/tests/fixtures/generated/basic.sqlite", encoding: none),
  now: datetime(year: 2026, month: 1, day: 20), seed: 42)
#qr.sql-table(
  qr.query(db, "SELECT id, region, amount, price FROM orders ORDER BY id LIMIT 120"),
  columns: (
    (key: "id", title: [\#]),
    (key: "region", title: [Region]),
    (key: "amount", title: [Amount], format: qr.number()),
    (key: "price", title: [Price], format: qr.currency("USD")),
  ),
  zebra: true,
)
