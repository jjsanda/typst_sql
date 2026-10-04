#import "/packages/quarry/src/core.typ" as qr

#let db = qr.sqlite(read("/tests/fixtures/generated/basic.sqlite", encoding: none),
  now: datetime(year: 2026, month: 1, day: 20), seed: 42)

// basic query, dict rows
#let regions = qr.query(db, ```sql
  SELECT region, SUM(amount) AS total, COUNT(*) AS n
  FROM orders WHERE fiscal_year = :year GROUP BY region ORDER BY total DESC
```, year: 2026)
#assert.eq(regions.rows.len(), 4)
#assert.eq(type(regions.rows.first().total), int)

// accessors
#assert.eq(qr.value(db, "SELECT count(*) FROM orders"), 200)
#assert.eq(qr.column(db, "SELECT name FROM regions ORDER BY region_id").len(), 4)
#assert.eq(qr.row(db, "SELECT * FROM regions WHERE region_id = 1").name, "EMEA")
#assert.eq(qr.value(db, "SELECT count(*) FROM orders WHERE region = ?", params: ("EMEA",)), 50)

// coercion: datetime columns become datetime
#let o = qr.row(db, "SELECT created_at FROM orders WHERE id = 1")
#assert.eq(type(o.created_at), datetime)
#assert.eq(o.created_at.year(), 2026)

// bool coercion
#let t = qr.row(db, "SELECT bool_col FROM types_test WHERE id = 1")
#assert.eq(t.bool_col, true)

// datetime binding round-trip
#assert.eq(
  qr.value(db, "SELECT count(*) FROM orders WHERE created_at < :cut",
    cut: datetime(year: 2026, month: 1, day: 5)), 31)

// injected clock
#assert.eq(qr.value(db, "SELECT date('now')"), "2026-01-20")

// try-query catchability
#let bad = qr.try-query(db, "SELECT * FROM custmers")
#assert.eq(bad.ok, false)
#assert.eq(bad.error.hint, "did you mean \"customers\"?")
#assert(bad.rendered.contains("custmers"))

// expect assertions
#let _ = qr.query(db, "SELECT name FROM regions", expect: (min-rows: 4, columns: ("name",)))

// describe + schema + capabilities
#assert.eq(qr.describe(db, "SELECT id, region FROM orders WHERE id = :id").statements.first().parameters, (":id",))
#assert(qr.schema(db).sources.first().tables.map(t => t.name).contains("orders"))
#assert.eq(qr.capabilities(db: db).at("sql-dialect"), "sqlite")

// multi-source
#let multi = qr.sqlite((
  sales: read("/tests/fixtures/generated/basic.sqlite", encoding: none),
  refs: read("/tests/fixtures/generated/refs.sqlite", encoding: none),
), now: none, seed: 0)
#assert.eq(qr.value(multi, "SELECT currency FROM sales.regions JOIN refs.region_meta USING (region_id) WHERE name = 'EMEA'"), "EUR")

// clock hard-error mode is catchable
#let noclock = qr.try-query(multi, "SELECT date('now')")
#assert.eq(noclock.ok, false)
#assert.eq(noclock.error.code, "clock-required")

// probe + try-sqlite
#assert.eq(qr.probe(read("/tests/fixtures/generated/basic.sqlite", encoding: none)).ok, true)
#let bad-open = qr.try-sqlite(bytes("not a database"))
#assert.eq(bad-open.ok, false)

// cache key + normalize
#assert(qr.cache-key("wh", "SELECT 1").starts-with("b3-"))
#assert.eq(qr.normalize-sql("SELECT  1 -- x"), "SELECT 1")

Core OK
