// quarry — compile-time SQL for Typst.
//
//   #import "@preview/quarry:0.1.0" as qr
//   #let db = qr.sqlite(read("sales.sqlite", encoding: none))
//   #qr.sql-table(qr.query(db, ```sql SELECT * FROM orders```))
//
// Correct (real libc, injected clock/seed, 64-bit ints, typed BLOBs),
// reproducible (same source + same data ⇒ same PDF), catchable errors.
// Full documentation: docs/ in the repository, README for the tour.

#import "src/core.typ": (
  sqlite, try-sqlite, probe, query, try-query, value, column, row, rows,
  describe, schema, capabilities, cache-key, normalize-sql,
)
#import "src/format.typ": number, currency, percent, date, unit, auto-format
#import "src/table.typ": sql-table
#import "src/charts.typ": xy, xticks, series, cetz-points
#import "src/extras.typ": schema-doc, pivot, sparkline, sql-lib
#import "src/sidecar.typ": remote, declare, sync-status, discover-mode
#import "src/util.typ": iso-display, parse-iso
