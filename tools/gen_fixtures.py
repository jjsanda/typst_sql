#!/usr/bin/env python3
"""Deterministic test-fixture databases for the quarry test suites.

Regenerated on demand (tests invoke this when a fixture is missing); content is
fully deterministic so tests can assert exact values. Requires only the Python
standard library.

Usage: gen_fixtures.py [--out DIR] [--big MB]
"""
import argparse
import os
import sqlite3
import struct
import sys

EPOCH_2026 = "2026-01-15 12:00:00"


def build_basic(path):
    if os.path.exists(path):
        os.remove(path)
    db = sqlite3.connect(path)
    c = db.cursor()
    c.executescript(
        """
        PRAGMA page_size = 4096;
        CREATE TABLE regions (
            region_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            region_group TEXT
        );
        CREATE TABLE orders (
            id INTEGER PRIMARY KEY,
            region_id INTEGER REFERENCES regions(region_id),
            region TEXT,
            amount INTEGER NOT NULL,
            price REAL,
            created_at DATETIME,
            fiscal_year INTEGER,
            note TEXT,
            flag BOOLEAN,
            data BLOB
        );
        CREATE TABLE customers (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            email TEXT
        );
        CREATE TABLE types_test (
            id INTEGER PRIMARY KEY,
            i INTEGER,
            r REAL,
            t TEXT,
            b BLOB,
            d DATE,
            dt DATETIME,
            tm TIME,
            bool_col BOOLEAN,
            j JSON
        );
        CREATE INDEX idx_orders_region ON orders(region);
        CREATE VIEW order_totals AS
            SELECT region, SUM(amount) AS total, COUNT(*) AS n
            FROM orders GROUP BY region;
        """
    )
    regions = [
        (1, "EMEA", "International"),
        (2, "APAC", "International"),
        (3, "Americas", "Domestic"),
        (4, "Internal", None),
    ]
    c.executemany("INSERT INTO regions VALUES (?,?,?)", regions)

    orders = []
    region_names = {1: "EMEA", 2: "APAC", 3: "Americas", 4: "Internal"}
    for i in range(1, 201):
        rid = (i % 4) + 1
        orders.append(
            (
                i,
                rid,
                region_names[rid],
                (i * 137) % 9000 + 100,
                round(i * 12.5 + 0.25, 2),
                f"2026-01-{(i % 28) + 1:02d} {(i % 24):02d}:30:00",
                2025 if i <= 60 else 2026,
                f"order-{i}",
                i % 2,
                None if i % 5 else struct.pack("<I", i) * 4,
            )
        )
    c.executemany("INSERT INTO orders VALUES (?,?,?,?,?,?,?,?,?,?)", orders)

    customers = [
        (1, "O'Brien Ltd", "obrien@example.com"),
        (2, 'Quote"Corp', "quote@example.com"),
        (3, "Ünïcode GmbH", "unicode@example.com"),
        (4, "Comma, Inc", None),
        (5, "Semi;colon LLC", "semi@example.com"),
    ]
    c.executemany("INSERT INTO customers VALUES (?,?,?)", customers)

    # 1x1 opaque red PNG for the blob → image() regression (d06), built with
    # correct CRCs so Typst's image() accepts it.
    import zlib

    def png_chunk(kind, payload):
        return (
            len(payload).to_bytes(4, "big")
            + kind
            + payload
            + zlib.crc32(kind + payload).to_bytes(4, "big")
        )

    ihdr = struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)
    idat = zlib.compress(b"\x00\xff\x00\x00")  # filter 0 + RGB red
    png = (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", idat)
        + png_chunk(b"IEND", b"")
    )
    types_rows = [
        (1, 9223372036854775807, 1.5e10, "max int64", b"\x00\x01\x02", "2026-01-15",
         "2026-01-15 12:00:00", "12:00:00", 1, '{"k": [1, 2, 3]}'),
        (2, -9223372036854775808, 1.2e-7, "min int64", b"", "1999-12-31",
         "1999-12-31 23:59:59", "23:59:59", 0, '{"nested": {"a": true}}'),
        (3, 3000000000, 2.718281828459045, "snowflake-ish", png, "2026-07-30",
         "2026-07-30 00:00:00", "00:00:00", None, "[]"),
        (4, 1753900000000, -0.0, "epoch ms", None, None, None, None, 1, None),
        (5, None, float("inf"), None, b"\xff" * 16, "2000-02-29",
         "2000-02-29 06:00:00", "06:00:00", 0, '{"inf": null}'),
    ]
    c.executemany("INSERT INTO types_test VALUES (?,?,?,?,?,?,?,?,?,?)", types_rows)

    # FTS5 corpus table, if this Python build has FTS5.
    try:
        c.execute("CREATE VIRTUAL TABLE docs USING fts5(title, body)")
        c.executemany(
            "INSERT INTO docs VALUES (?,?)",
            [
                ("quarterly report", "revenue grew in EMEA and APAC regions"),
                ("incident summary", "database outage resolved within the SLA"),
                ("meeting notes", "discuss revenue targets for next quarter"),
            ],
        )
    except sqlite3.OperationalError:
        print("warning: python sqlite3 lacks FTS5; docs table omitted", file=sys.stderr)

    db.commit()
    db.close()


def build_refs(path):
    if os.path.exists(path):
        os.remove(path)
    db = sqlite3.connect(path)
    c = db.cursor()
    c.executescript(
        """
        CREATE TABLE region_meta (
            region_id INTEGER PRIMARY KEY,
            currency TEXT NOT NULL,
            manager TEXT
        );
        """
    )
    c.executemany(
        "INSERT INTO region_meta VALUES (?,?,?)",
        [
            (1, "EUR", "Alex"),
            (2, "SGD", "Sam"),
            (3, "USD", "Jordan"),
            (4, "CZK", None),
        ],
    )
    db.commit()
    db.close()


def build_wal(path):
    """A cleanly checkpointed database whose header is still marked WAL (C-7)."""
    if os.path.exists(path):
        os.remove(path)
    db = sqlite3.connect(path)
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
    db.execute("INSERT INTO t VALUES (1, 'wal database')")
    db.commit()
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    db.close()
    with open(path, "rb") as f:
        header = f.read(20)
    assert header[18] == 2 and header[19] == 2, "expected WAL-marked header"


def build_malformed(outdir):
    os.makedirs(outdir, exist_ok=True)
    basic = os.path.join(os.path.dirname(outdir), "basic.sqlite")
    with open(basic, "rb") as f:
        data = f.read()

    def w(name, content):
        with open(os.path.join(outdir, name), "wb") as f:
            f.write(content)

    w("empty.sqlite", b"")
    w("truncated-header.sqlite", data[:50])
    w("truncated-mid.sqlite", data[: len(data) // 2 + 37])
    w("bad-magic.sqlite", b"NotSQLite3 data\x00" + data[16:])
    bad_page = bytearray(data)
    bad_page[16:18] = (1000).to_bytes(2, "big")  # not a power of two
    w("bad-page-size.sqlite", bytes(bad_page))
    corrupt = bytearray(data)
    for i in range(4096, min(8192, len(corrupt))):
        corrupt[i] ^= 0xA5
    w("corrupt-page.sqlite", bytes(corrupt))
    zeros = bytearray(data)
    zeros[40:44] = b"\xff\xff\xff\xff"  # absurd schema cookie
    w("weird-cookie.sqlite", bytes(zeros))
    shrunk = bytearray(data)
    shrunk[28:32] = (2**31 - 1).to_bytes(4, "big")  # page count far beyond file
    w("liar-page-count.sqlite", bytes(shrunk))


def build_big(path, mb):
    if os.path.exists(path):
        os.remove(path)
    db = sqlite3.connect(path)
    c = db.cursor()
    c.execute("PRAGMA page_size = 4096")
    c.execute(
        "CREATE TABLE events (id INTEGER PRIMARY KEY, ts INTEGER, kind TEXT, payload TEXT)"
    )
    row_bytes = 120
    rows = (mb * 1024 * 1024) // row_bytes
    batch = []
    for i in range(rows):
        batch.append((i, 1_700_000_000 + i, f"kind-{i % 7}", f"payload-{i:012d}" + "x" * 80))
        if len(batch) == 10_000:
            c.executemany("INSERT INTO events VALUES (?,?,?,?)", batch)
            batch = []
    if batch:
        c.executemany("INSERT INTO events VALUES (?,?,?,?)", batch)
    db.commit()
    db.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=None)
    ap.add_argument("--big", type=int, default=0, help="also build big.sqlite of N MB")
    ap.add_argument("--force", action="store_true", help="rebuild fixtures that already exist")
    args = ap.parse_args()
    out = args.out or os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "tests",
        "fixtures",
        "generated",
    )
    os.makedirs(out, exist_ok=True)
    # Never clobber existing fixtures unless forced: test binaries invoke this
    # concurrently, and rewriting a fixture while another test reads it is a
    # race. Each build is also written to a temp name and atomically renamed.
    if args.force or not os.path.exists(os.path.join(out, "basic.sqlite")):
        build_basic(os.path.join(out, "basic.sqlite"))
        build_refs(os.path.join(out, "refs.sqlite"))
        build_wal(os.path.join(out, "wal.sqlite"))
        build_malformed(os.path.join(out, "malformed"))
    if args.big and (args.force or not os.path.exists(os.path.join(out, "big.sqlite"))):
        tmp = os.path.join(out, f"big.sqlite.tmp{os.getpid()}")
        build_big(tmp, args.big)
        os.replace(tmp, os.path.join(out, "big.sqlite"))
    print(f"fixtures written to {out}")


if __name__ == "__main__":
    main()
