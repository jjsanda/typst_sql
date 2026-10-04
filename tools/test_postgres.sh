#!/usr/bin/env bash
# Phase-3 integration against a REAL PostgreSQL server (portable, unprivileged
# zonky binaries): parameter rewriting, type mapping, read-only enforcement,
# credentials-by-reference, and the flagship criterion — the report rebuilds
# offline from the lockfile with the database GONE.
#
# PG_ROOT must point at an extracted embedded-postgres-binaries tree
# (bin/initdb etc.); tools/fetch_postgres.sh downloads one.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${QUARRY_PREFIX:-$HOME/.local/quarry}"
TYPST="${TYPST_BIN:-$PREFIX/bin/typst}"
QUARRY="$REPO/target/native-release/quarry"
PG_ROOT="${PG_ROOT:?set PG_ROOT to a portable postgres tree (see tools/fetch_postgres.sh)}"
PORT="${PG_PORT:-54329}"
WORK="$(mktemp -d)"
fail() { echo "POSTGRES TEST FAILED: $*" >&2; exit 1; }
cleanup() {
  "$PG_ROOT/bin/pg_ctl" -D "$WORK/pgdata" stop -m immediate >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

# ── server ───────────────────────────────────────────────────────────────────
"$PG_ROOT/bin/initdb" -D "$WORK/pgdata" -U quarry -A trust --no-sync >/dev/null
# no psql in the portable distro: seed via single-user mode (one statement per line)
"$PG_ROOT/bin/postgres" --single -D "$WORK/pgdata" postgres >/dev/null <<'SQL'
CREATE TABLE metrics (month date NOT NULL, cohort text NOT NULL, churn_rate double precision NOT NULL, customers bigint NOT NULL, note text)
INSERT INTO metrics VALUES ('2026-01-01', '2026-enterprise', 0.021, 9223372036854775807, 'boundary check'), ('2026-02-01', '2026-enterprise', 0.018, 480, NULL), ('2026-03-01', '2026-enterprise', 0.016, 495, 'O''Brien effect'), ('2026-01-01', 'starter', 0.070, 12000, NULL)
SQL
"$PG_ROOT/bin/pg_ctl" -D "$WORK/pgdata" -l "$WORK/pg.log" \
  -o "-p $PORT -c listen_addresses=127.0.0.1 -k $WORK" start >/dev/null
export WAREHOUSE_URL="postgres://quarry@127.0.0.1:$PORT/postgres"

# ── project ──────────────────────────────────────────────────────────────────
mkdir -p "$WORK/proj/packages"
cp -r "$REPO/packages/quarry" "$WORK/proj/packages/quarry"
cd "$WORK/proj"
cat > quarry.toml <<EOF
[quarry]
version = 1
mode = "strict"
typst = "$TYPST"

[sources.warehouse]
kind = "postgres"
url = "env:WAREHOUSE_URL"
statement-timeout = "10s"
max-rows = 1000
EOF

cat > report.typ <<'EOF'
#import "/packages/quarry/lib.typ" as qr
#let wh = qr.remote("warehouse")
#let churn = qr.query(wh, ```sql
  SELECT month, churn_rate, customers, note
  FROM metrics WHERE cohort = :cohort ORDER BY month
```, cohort: "2026-enterprise")
#let blocked = qr.try-query(wh, "INSERT INTO metrics VALUES ('2026-04-01', 'x', 0, 1, NULL)")
#qr.declare(blocked)
#if not qr.discover-mode() [
  #assert.eq(churn.rows.len(), 3)
  // bigint boundary survives postgres → envelope → typst exactly
  #assert.eq(churn.rows.first().customers, 9223372036854775807)
  // date column coerced to datetime by logical type
  #assert.eq(type(churn.rows.first().month), datetime)
  #assert.eq(churn.rows.at(2).note, "O'Brien effect")
  // read-only enforcement produced a catchable cached error
  #assert.eq(blocked.ok, false)
  #assert(lower(blocked.error.message).contains("read-only"))
]
#qr.sql-table(churn, columns: (
  (key: "month", title: [Month], format: qr.date()),
  (key: "churn_rate", title: [Churn], format: qr.percent(digits: 1)),
  (key: "customers", title: [Customers], format: qr.number()),
))
EOF

"$QUARRY" sync report.typ > sync.log
grep -q "1 SQL error" sync.log || fail "read-only INSERT should cache exactly one SQL error ($(cat sync.log))"
export SOURCE_DATE_EPOCH=1768867200
"$TYPST" compile --root . --creation-timestamp "$SOURCE_DATE_EPOCH" report.typ a.pdf

# ── the flagship criterion: database gone, report still builds, byte-identical
"$PG_ROOT/bin/pg_ctl" -D "$WORK/pgdata" stop -m immediate >/dev/null 2>&1
unset WAREHOUSE_URL
"$TYPST" compile --root . --creation-timestamp "$SOURCE_DATE_EPOCH" report.typ b.pdf
cmp a.pdf b.pdf || fail "offline rebuild differs"
echo "postgres: report reproducible offline from the lockfile, without credentials: OK"

# offline sync passes from cache alone
"$QUARRY" sync --mode offline report.typ >/dev/null || fail "offline mode should pass with a full cache"
echo "──────────────────────────────────────"
echo "postgres integration: ALL OK"
