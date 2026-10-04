#!/usr/bin/env python3
"""Differential-test corpus generator.

Deterministically (fixed seed, no wall clock) produces:
- tests/corpus/corpus.sqlite  — a database with adversarial data: integer
  boundaries, exponent-notation floats, unicode, quotes, 10k rows for sorts,
  every date storage convention, JSON documents, blobs;
- tests/corpus/queries.jsonl  — ≥500 queries as {"id", "category", "sql"}.

The differential runner executes every query against the native engine and the
WASM build under wasmi and requires byte-identical envelopes (with a documented
≤2-ULP float tolerance for the `math` category only — glibc and wasi-libc's
libm may legitimately differ in the last bits of transcendental functions).
"""
import json
import os
import random
import sqlite3
import sys

OUT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tests", "corpus")
RNG = random.Random(20260730)


def build_db(path):
    if os.path.exists(path):
        os.remove(path)
    db = sqlite3.connect(path)
    c = db.cursor()
    c.executescript(
        """
        PRAGMA page_size = 4096;
        CREATE TABLE nums (
            id INTEGER PRIMARY KEY,
            i INTEGER, r REAL, txt_num TEXT
        );
        CREATE TABLE strs (
            id INTEGER PRIMARY KEY,
            s TEXT, s_nocase TEXT COLLATE NOCASE, s_rtrim TEXT COLLATE RTRIM
        );
        CREATE TABLE dates (
            id INTEGER PRIMARY KEY,
            iso TEXT, julian REAL, epoch INTEGER, decl DATETIME
        );
        CREATE TABLE docs_json (id INTEGER PRIMARY KEY, j TEXT);
        CREATE TABLE blobs (id INTEGER PRIMARY KEY, b BLOB);
        CREATE TABLE big (
            id INTEGER PRIMARY KEY,
            grp TEXT, val INTEGER, fval REAL, name TEXT
        );
        CREATE INDEX idx_big_grp ON big(grp);
        """
    )

    ints = [
        0, 1, -1, 2, 42, 255, 256, 65535, 2**31 - 1, -(2**31), 2**31, 2**32,
        2**53, 2**53 + 1, 2**62, 2**63 - 1, -(2**63), 3000000000, 1753900000000,
        -999999999999999999,
    ]
    reals = [
        0.0, -0.0, 0.1, 0.5, 1.0 / 3.0, 2.718281828459045, 3.141592653589793,
        1.5e10, 1.2e-7, 1e308, 1e-308, 1.7976931348623157e308,
        2.2250738585072014e-308, 5e-324, -1.5e-100, 123456789.123456789,
    ]
    txt_nums = [
        "9223372036854775807", "-9223372036854775808", "3000000000", "1.5e10",
        "1.2e-7", "1e308", "1e-308", "0.1", "  42  ", "12abc", "abc", "",
        "0x1A", "1.", ".5", "-0.0", "9e999", "1.7976931348623157e308",
    ]
    for idx in range(60):
        c.execute(
            "INSERT INTO nums VALUES (?,?,?,?)",
            (
                idx + 1,
                ints[idx % len(ints)],
                reals[idx % len(reals)],
                txt_nums[idx % len(txt_nums)],
            ),
        )

    strings = [
        "hello", "HELLO", "Hello World", "O'Brien", 'quote"double', "tab\there",
        "new\nline", "Ünïcödé", "ěščřžýáíé", "日本語テキスト", "emoji 🎉 party",
        "trailing   ", "   leading", "", "a", "zzz", "100% match_",
        "under_score", "back\\slash", "semi;colon",
    ]
    for idx, s in enumerate(strings):
        c.execute("INSERT INTO strs VALUES (?,?,?,?)", (idx + 1, s, s, s))

    date_rows = [
        ("2026-01-15 12:30:45", 2461056.021354167, 1768480245, "2026-01-15 12:30:45"),
        ("2000-02-29 00:00:00", 2451603.5, 951782400, "2000-02-29 00:00:00"),
        ("1969-12-31 23:59:59", 2440587.499988426, -1, "1969-12-31 23:59:59"),
        ("2038-01-19 03:14:08", 2465442.634814815, 2147483648, "2038-01-19 03:14:08"),
        ("1900-01-01 00:00:00", 2415020.5, -2208988800, "1900-01-01 00:00:00"),
        ("2026-07-30 23:59:59", 2461252.499988426, 1785455999, "2026-07-30 23:59:59"),
    ]
    for idx, row in enumerate(date_rows):
        c.execute("INSERT INTO dates VALUES (?,?,?,?,?)", (idx + 1,) + row)

    json_docs = [
        '{"a": 1, "b": [1, 2, 3], "c": {"d": "text"}}',
        '{"nested": {"deep": {"deeper": [true, false, null]}}}',
        '[1, 2.5, "three", null, {"four": 4}]',
        '{"big": 9223372036854775807, "neg": -42, "float": 1.5e10}',
        '{"unicode": "Ünïcödé 日本語", "quote": "say \\"hi\\""}',
        '{}',
        '[]',
    ]
    for idx, j in enumerate(json_docs):
        c.execute("INSERT INTO docs_json VALUES (?,?)", (idx + 1, j))

    blob_vals = [
        b"", b"\x00", b"\xff" * 32, bytes(range(256)),
        b"\x00\x01\x02\x03" * 100, b"PNG-ish\x89\x50\x4e\x47",
    ]
    for idx, b in enumerate(blob_vals):
        c.execute("INSERT INTO blobs VALUES (?,?)", (idx + 1, b))

    groups = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta"]
    rows = []
    for i in range(10_000):
        rows.append(
            (
                i + 1,
                groups[RNG.randrange(len(groups))],
                RNG.randint(-(2**40), 2**40),
                RNG.random() * 1e6 - 5e5,
                f"name-{RNG.randint(0, 99999):05d}",
            )
        )
    c.executemany("INSERT INTO big VALUES (?,?,?,?,?)", rows)

    has_fts = True
    try:
        c.execute("CREATE VIRTUAL TABLE fts USING fts5(title, body)")
        fts_rows = [
            ("alpha report", "quarterly revenue grew across all regions"),
            ("beta memo", "the database migration finished ahead of schedule"),
            ("gamma notes", "revenue targets and database reliability discussed"),
            ("delta summary", "no incidents this quarter"),
        ]
        c.executemany("INSERT INTO fts VALUES (?,?)", fts_rows)
    except sqlite3.OperationalError:
        has_fts = False
        print("warning: no FTS5 in python sqlite3; fts category omitted", file=sys.stderr)

    db.commit()
    db.close()
    return has_fts


def gen_queries(has_fts):
    q = []

    def add(category, sql):
        q.append({"id": f"{category}-{sum(1 for x in q if x['category'] == category):03d}",
                  "category": category, "sql": sql})

    # -- math: every SQL math function over a value grid (D-01) --------------
    grid = ["-2.0", "-1.0", "-0.5", "0.0", "0.5", "1.0", "2.0", "10.0", "100.0", "1e10", "1e-10"]
    for fn in ["sqrt", "exp", "ln", "log10", "log2", "sin", "cos", "tan", "asin",
               "acos", "atan", "sinh", "cosh", "tanh", "ceil", "floor", "trunc",
               "degrees", "radians"]:
        for v in grid:
            add("math", f"SELECT {fn}({v}) AS v")
    for a, b in [("2.0", "10.0"), ("10.0", "-2.0"), ("0.0", "0.0"), ("-8.0", "0.333333333333333"),
                 ("1e5", "2.5"), ("2.0", "0.5")]:
        add("math", f"SELECT pow({a}, {b}) AS v")
        add("math", f"SELECT atan2({a}, {b}) AS v")
    for a, b in [("7.5", "2.0"), ("-7.5", "2.0"), ("1e10", "3.0")]:
        add("math", f"SELECT mod({a}, {b}) AS v")
    add("math", "SELECT pi() AS v")
    add("math", "SELECT sqrt(r) AS v FROM nums WHERE r > 0 ORDER BY id LIMIT 20")
    # Relative delta between pow(x,0.5) and sqrt(x): ~0 for a real libm
    # (within a few ULP), but -1.0 if pow is a return-0.0 stub (D-01). Kept
    # relative so a legitimate 1-ULP libm difference can't explode the value.
    add("math", "SELECT (pow(abs(r), 0.5) - sqrt(abs(r))) / max(sqrt(abs(r)), 1e-300) AS rel_delta FROM nums WHERE r = r ORDER BY id LIMIT 20")

    # -- int64 (D-02) --------------------------------------------------------
    for lit in ["9223372036854775807", "-9223372036854775808", "3000000000",
                "2147483647", "2147483648", "-2147483649", "4294967296",
                "9007199254740993", "0x7FFFFFFFFFFFFFFF", "0xFFFFFFFF"]:
        add("int64", f"SELECT {lit} AS v, typeof({lit}) AS t")
    for txt in ["'9223372036854775807'", "'-9223372036854775808'", "'3000000000'",
                "'  42  '", "'12abc'", "'abc'", "''", "'0x1A'"]:
        add("int64", f"SELECT CAST({txt} AS INTEGER) AS v, typeof(CAST({txt} AS INTEGER)) AS t")
    add("int64", "SELECT i, typeof(i) FROM nums ORDER BY id")
    add("int64", "SELECT CAST(txt_num AS INTEGER) AS v FROM nums ORDER BY id")
    add("int64", "SELECT max(i) AS mx, min(i) AS mn FROM nums")
    add("int64", "SELECT i + 0 AS v FROM nums WHERE i BETWEEN -9223372036854775808 AND 9223372036854775807 ORDER BY i LIMIT 25")
    add("int64", "SELECT 9223372036854775807 = CAST('9223372036854775807' AS INTEGER) AS eq")
    add("int64", "SELECT i * 1 AS v, -i AS neg FROM nums WHERE i != -9223372036854775808 ORDER BY id LIMIT 30")

    # -- strtod / float text parsing (D-03) ----------------------------------
    for txt in ["'1.5e10'", "'1.2e-7'", "'1e308'", "'1e-308'", "'0.1'",
                "'1.7976931348623157e308'", "'2.2250738585072014e-308'",
                "'5e-324'", "'1.'", "'.5'", "'-0.0'", "'9e999'", "'-9e999'",
                "'3.141592653589793'", "'1e'", "'e5'", "''"]:
        add("strtod", f"SELECT CAST({txt} AS REAL) AS v")
    add("strtod", "SELECT CAST(txt_num AS REAL) AS v FROM nums ORDER BY id")
    add("strtod", "SELECT txt_num + 0.0 AS v FROM nums ORDER BY id")
    add("strtod", "SELECT r = CAST(CAST(r AS TEXT) AS REAL) AS roundtrips, r FROM nums WHERE r = r ORDER BY id LIMIT 20")
    add("strtod", "SELECT printf('%.17g', r) AS s FROM nums ORDER BY id")
    add("strtod", "SELECT format('%.17g', 0.1) AS a, format('%!.15g', 100.0) AS b")

    # -- sorts and collations (D-08 class) -----------------------------------
    for col, order in [("val", "ASC"), ("val", "DESC"), ("fval", "ASC"),
                       ("name", "ASC"), ("name", "DESC"), ("grp", "ASC")]:
        add("sort", f"SELECT id FROM big ORDER BY {col} {order}, id LIMIT 100")
    add("sort", "SELECT id FROM big ORDER BY fval DESC, val ASC, id LIMIT 200")
    add("sort", "SELECT s FROM strs ORDER BY s")
    add("sort", "SELECT s FROM strs ORDER BY s COLLATE NOCASE, s")
    add("sort", "SELECT s_rtrim FROM strs ORDER BY s_rtrim, id")
    add("sort", "SELECT grp, count(*) AS n FROM big GROUP BY grp ORDER BY n DESC, grp")
    add("sort", "SELECT DISTINCT grp FROM big ORDER BY grp")
    add("sort", "SELECT val FROM big ORDER BY abs(val) LIMIT 50")
    add("sort", "SELECT name, min(fval) FROM big GROUP BY name ORDER BY name LIMIT 100")

    # -- strings -------------------------------------------------------------
    add("string", "SELECT s, length(s) AS chars, octet_length(s) AS bytes FROM strs ORDER BY id")
    add("string", "SELECT upper(s) AS u, lower(s) AS l FROM strs ORDER BY id")
    add("string", "SELECT trim(s) AS t, ltrim(s) AS lt, rtrim(s) AS rt FROM strs ORDER BY id")
    add("string", "SELECT replace(s, 'l', 'L') AS r FROM strs ORDER BY id")
    add("string", "SELECT substr(s, 2, 3) AS sub, instr(s, 'o') AS pos FROM strs ORDER BY id")
    add("string", "SELECT hex(s) AS h FROM strs ORDER BY id")
    add("string", "SELECT quote(s) AS q FROM strs ORDER BY id")
    add("string", "SELECT s FROM strs WHERE s LIKE '%o%' ORDER BY id")
    add("string", "SELECT s FROM strs WHERE s LIKE 'h_llo' ORDER BY id")
    add("string", "SELECT s FROM strs WHERE s GLOB '*o*' ORDER BY id")
    add("string", "SELECT s FROM strs WHERE s LIKE '100\\% match\\_' ESCAPE '\\' ORDER BY id")
    add("string", "SELECT char(65, 66, 67, 268, 269) AS c")
    add("string", "SELECT unicode('Ü') AS u, unicode('日') AS n")
    add("string", "SELECT printf('%d|%s|%f|%x', 42, 'str', 1.5, 255) AS out")
    add("string", "SELECT printf('%10.3f|%-10s|%05d', 3.14159, 'pad', 7) AS out")
    add("string", "SELECT group_concat(s, '; ') AS joined FROM (SELECT s FROM strs ORDER BY id LIMIT 8)")
    add("string", "SELECT concat(s, '-suffix') AS c FROM strs ORDER BY id LIMIT 10")
    add("string", "SELECT concat_ws('|', s, s_nocase) AS c FROM strs ORDER BY id LIMIT 10")
    add("string", "SELECT ltrim('xxhixx', 'x') AS a, rtrim('xxhixx', 'x') AS b")
    add("string", "SELECT s FROM strs WHERE s_nocase = 'hello' ORDER BY id")
    add("string", "SELECT unhex('DEADBEEF') AS b, hex(unhex('DEADBEEF')) AS h")
    add("string", "SELECT zeroblob(16) AS z, length(zeroblob(1000)) AS n")

    # -- dates (D-04) --------------------------------------------------------
    mods = ["'+1 day'", "'-30 days'", "'+3 months'", "'-1 year'", "'start of month'",
            "'start of year'", "'weekday 1'", "'+90 minutes'", "'-12 hours'"]
    for m in mods:
        add("date", f"SELECT date('now', {m}) AS d")
        add("date", f"SELECT datetime('now', {m}) AS dt")
    for fn in ["date", "time", "datetime", "julianday", "unixepoch"]:
        add("date", f"SELECT {fn}('now') AS v")
        add("date", f"SELECT {fn}(iso) AS v FROM dates ORDER BY id")
    add("date", "SELECT date(julian) AS d FROM dates ORDER BY id")
    add("date", "SELECT datetime(epoch, 'unixepoch') AS dt FROM dates ORDER BY id")
    add("date", "SELECT strftime('%Y-%m-%d %H:%M:%S', iso) AS f FROM dates ORDER BY id")
    add("date", "SELECT strftime('%j|%W|%w|%s', iso) AS f FROM dates ORDER BY id")
    add("date", "SELECT strftime('%Y-%m', 'now') AS ym")
    add("date", "SELECT timediff(iso, '2026-01-01') AS td FROM dates ORDER BY id")
    add("date", "SELECT iso < datetime('now') AS past FROM dates ORDER BY id")
    add("date", "SELECT julianday(iso) - julianday('2026-01-01') AS delta FROM dates ORDER BY id")

    # -- json ----------------------------------------------------------------
    add("json", "SELECT json_extract(j, '$.a') AS a FROM docs_json ORDER BY id")
    add("json", "SELECT j ->> '$.b[1]' AS b1, j -> '$.c' AS c FROM docs_json ORDER BY id")
    add("json", "SELECT json_type(j) AS t, json_valid(j) AS v FROM docs_json ORDER BY id")
    add("json", "SELECT json_array_length(j, '$.b') AS n FROM docs_json ORDER BY id")
    add("json", "SELECT json_array(1, 2.5, 'three', null) AS arr")
    add("json", "SELECT json_object('a', 1, 'b', json_array(1, 2)) AS obj")
    add("json", "SELECT json_insert(j, '$.z', 99) AS ins FROM docs_json WHERE id = 1")
    add("json", "SELECT json_set(j, '$.a', 'replaced') AS s FROM docs_json WHERE id = 1")
    add("json", "SELECT json_remove(j, '$.a') AS r FROM docs_json WHERE id = 1")
    add("json", "SELECT key, value, type FROM docs_json, json_each(docs_json.j) WHERE docs_json.id = 1 ORDER BY key")
    add("json", "SELECT fullkey, type FROM docs_json, json_tree(docs_json.j) WHERE docs_json.id = 2 ORDER BY fullkey")
    add("json", "SELECT json_quote('text with \"quotes\"') AS q")
    add("json", "SELECT json_extract(j, '$.big') AS big, typeof(json_extract(j, '$.big')) AS t FROM docs_json WHERE id = 4")
    add("json", "SELECT json_group_array(id) AS arr FROM docs_json")
    add("json", "SELECT json_group_object(CAST(id AS TEXT), json_type(j)) AS obj FROM docs_json")
    add("json", "SELECT json_patch('{\"a\":1}', '{\"b\":2}') AS p")
    add("json", "SELECT jsonb_extract(j, '$.a') AS a FROM docs_json WHERE id = 1")

    # -- window functions ----------------------------------------------------
    add("window", "SELECT id, row_number() OVER (ORDER BY val) AS rn FROM big ORDER BY id LIMIT 50")
    add("window", "SELECT grp, val, rank() OVER (PARTITION BY grp ORDER BY val) AS rk FROM big ORDER BY id LIMIT 50")
    add("window", "SELECT grp, val, dense_rank() OVER (PARTITION BY grp ORDER BY val) AS dr FROM big ORDER BY id LIMIT 50")
    add("window", "SELECT id, lag(val) OVER (ORDER BY id) AS prev, lead(val) OVER (ORDER BY id) AS next FROM big ORDER BY id LIMIT 50")
    add("window", "SELECT id, sum(val) OVER (ORDER BY id ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) AS running FROM big ORDER BY id LIMIT 50")
    add("window", "SELECT grp, avg(fval) OVER (PARTITION BY grp) AS ga FROM big ORDER BY id LIMIT 50")
    add("window", "SELECT id, ntile(4) OVER (ORDER BY val) AS quartile FROM big ORDER BY id LIMIT 50")
    add("window", "SELECT id, first_value(name) OVER (PARTITION BY grp ORDER BY id) AS fv FROM big ORDER BY id LIMIT 50")
    add("window", "SELECT id, cume_dist() OVER (ORDER BY val) AS cd FROM big ORDER BY id LIMIT 20")
    add("window", "SELECT id, percent_rank() OVER (ORDER BY val) AS pr FROM big ORDER BY id LIMIT 20")

    # -- CTEs ----------------------------------------------------------------
    add("cte", "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM c WHERE n < 100) SELECT sum(n) AS s, count(*) AS n FROM c")
    add("cte", "WITH RECURSIVE fib(a, b) AS (SELECT 0, 1 UNION ALL SELECT b, a+b FROM fib WHERE b < 1000000) SELECT a FROM fib ORDER BY a")
    add("cte", "WITH totals AS (SELECT grp, sum(val) AS t FROM big GROUP BY grp) SELECT grp, t FROM totals ORDER BY t DESC")
    add("cte", "WITH RECURSIVE dates_r(d) AS (SELECT '2026-01-01' UNION ALL SELECT date(d, '+1 day') FROM dates_r WHERE d < '2026-01-31') SELECT count(*) AS n, max(d) AS last FROM dates_r")
    add("cte", "WITH a AS (SELECT 1 AS x), b AS (SELECT x + 1 AS y FROM a) SELECT x, y FROM a, b")

    # -- aggregates ----------------------------------------------------------
    add("agg", "SELECT count(*) AS c, sum(val) AS s, avg(val) AS a, min(val) AS mn, max(val) AS mx FROM big")
    add("agg", "SELECT total(val) AS t, total(fval) AS tf FROM big")
    add("agg", "SELECT grp, sum(fval) AS s FROM big GROUP BY grp HAVING s > 0 ORDER BY grp")
    add("agg", "SELECT grp, count(DISTINCT name) AS dn FROM big GROUP BY grp ORDER BY grp")
    add("agg", "SELECT avg(r) AS a FROM nums WHERE r NOT IN (9e999, -9e999) AND r = r")
    add("agg", "SELECT sum(i) AS s FROM nums WHERE abs(i) < 4611686018427387904")
    add("agg", "SELECT group_concat(grp) AS g FROM (SELECT DISTINCT grp FROM big ORDER BY grp)")
    add("agg", "SELECT count(*) FILTER (WHERE val > 0) AS pos, count(*) FILTER (WHERE val < 0) AS neg FROM big")
    add("agg", "SELECT grp, min(fval) AS mn, max(fval) AS mx FROM big GROUP BY grp ORDER BY grp")
    add("agg", "SELECT string_agg(name, ',') AS sa FROM (SELECT name FROM big ORDER BY id LIMIT 10)")

    # -- affinity and NULL semantics -----------------------------------------
    add("null", "SELECT NULL IS NULL AS a, NULL IS NOT NULL AS b, NULL = NULL AS c")
    add("null", "SELECT coalesce(NULL, NULL, 3) AS c, ifnull(NULL, 'x') AS i, nullif(1, 1) AS n")
    add("null", "SELECT count(r) AS nonnull, count(*) AS total FROM (SELECT NULL AS r UNION ALL SELECT 1)")
    add("null", "SELECT 1 = '1' AS a, 1 = CAST('1' AS INTEGER) AS b, '1' = '1.0' AS c")
    add("null", "SELECT typeof(1), typeof(1.0), typeof('x'), typeof(x'00'), typeof(NULL)")
    add("null", "SELECT CASE WHEN val > 0 THEN 'pos' WHEN val < 0 THEN 'neg' ELSE 'zero' END AS sign, count(*) FROM big GROUP BY sign ORDER BY sign")
    add("null", "SELECT i IS DISTINCT FROM r AS d FROM nums ORDER BY id LIMIT 20")
    add("null", "SELECT max(NULL, 1) AS a, min(NULL, 1) AS b")
    add("null", "SELECT sum(CASE WHEN i IS NULL THEN 1 ELSE 0 END) AS nulls FROM nums")
    add("null", "SELECT iif(val > 0, 'yes', 'no') AS v, count(*) FROM big GROUP BY v ORDER BY v")

    # -- blobs (D-06) --------------------------------------------------------
    add("blob", "SELECT b, length(b) AS n FROM blobs ORDER BY id")
    add("blob", "SELECT hex(b) AS h FROM blobs ORDER BY id")
    add("blob", "SELECT substr(b, 1, 4) AS head FROM blobs WHERE length(b) >= 4 ORDER BY id")
    add("blob", "SELECT b = b AS eq, b < x'ff' AS lt FROM blobs ORDER BY id")
    add("blob", "SELECT x'deadbeef' AS lit, length(x'deadbeef') AS n")
    add("blob", "SELECT randomblob(16) AS rb, length(randomblob(1000)) AS n")
    add("blob", "SELECT hex(randomblob(8)) AS h1, hex(randomblob(8)) AS h2")
    add("blob", "SELECT CAST('text' AS BLOB) AS b, typeof(CAST('text' AS BLOB)) AS t")
    add("blob", "SELECT CAST(x'414243' AS TEXT) AS t")

    # -- subqueries ----------------------------------------------------------
    add("subq", "SELECT grp FROM big WHERE val = (SELECT max(val) FROM big)")
    add("subq", "SELECT count(*) AS n FROM big WHERE grp IN (SELECT grp FROM big GROUP BY grp HAVING count(*) > 1400)")
    add("subq", "SELECT EXISTS(SELECT 1 FROM big WHERE val > 0) AS has_pos")
    add("subq", "SELECT (SELECT count(*) FROM strs) AS strs, (SELECT count(*) FROM nums) AS nums")
    add("subq", "SELECT grp, (SELECT avg(fval) FROM big b2 WHERE b2.grp = b1.grp) AS ga FROM (SELECT DISTINCT grp FROM big) b1 ORDER BY grp")
    add("subq", "SELECT id FROM big b1 WHERE fval > (SELECT avg(fval) FROM big) ORDER BY id LIMIT 30")
    add("subq", "SELECT * FROM (SELECT grp, sum(val) AS s FROM big GROUP BY grp) WHERE s != 0 ORDER BY grp")

    # -- random (seeded determinism, D-05) -----------------------------------
    add("random", "SELECT random() AS a, random() AS b, random() AS c")
    add("random", "SELECT id FROM strs ORDER BY random()")
    add("random", "SELECT abs(random()) % 100 AS r1, abs(random()) % 100 AS r2")

    # -- fts5 ----------------------------------------------------------------
    if has_fts:
        add("fts", "SELECT title FROM fts WHERE fts MATCH 'revenue' ORDER BY rank")
        add("fts", "SELECT title, bm25(fts) AS score FROM fts WHERE fts MATCH 'database' ORDER BY score, title")
        add("fts", "SELECT highlight(fts, 1, '[', ']') AS h FROM fts WHERE fts MATCH 'quarter*' ORDER BY title")
        add("fts", "SELECT snippet(fts, 1, '<', '>', '…', 4) AS s FROM fts WHERE fts MATCH 'revenue OR database' ORDER BY title")
        add("fts", "SELECT count(*) AS n FROM fts WHERE fts MATCH 'title:alpha'")

    # -- generated per-value probes to give the differential surface breadth --
    for n in range(1, 21):
        add("probe", f"SELECT i, r, txt_num, typeof(i) AS ti, typeof(r) AS tr FROM nums WHERE id = {n}")
        add("probe", f"SELECT i + {n} AS plus, i - {n} AS minus, i / {max(n, 1)} AS div, i % {max(n, 1)} AS rem FROM nums WHERE id = {n}")
    for n in range(1, 21):
        add("probe", f"SELECT s, substr(s, 1, {n % 7 + 1}) AS pre, length(s) > {n % 5} AS longer FROM strs WHERE id = {n % 20 + 1}")
    for n in range(1, 16):
        add("probe", f"SELECT grp, count(*) AS n, sum(val) AS s, avg(fval) AS a FROM big WHERE id % {n + 1} = 0 GROUP BY grp ORDER BY grp")
    for off in range(0, 10):
        add("probe", f"SELECT id, name FROM big ORDER BY id LIMIT 10 OFFSET {off * 977}")
    for d in range(1, 7):
        add("probe", f"SELECT date(iso, '+{d} days') AS d1, datetime(iso, '-{d} hours') AS d2, strftime('%s', iso) AS secs FROM dates WHERE id = {d}")
    for p in ["$.a", "$.b", "$.b[0]", "$.b[2]", "$.c.d", "$.missing", "$[0]", "$"]:
        add("probe", f"SELECT id, json_extract(j, '{p}') AS v, json_type(j, '{p}') AS t FROM docs_json ORDER BY id")

    # -- multi-statement scripts (D-07) --------------------------------------
    add("script", "CREATE TEMP TABLE t AS SELECT grp, sum(val) AS s FROM big GROUP BY grp; SELECT * FROM t ORDER BY grp")
    add("script", "CREATE TEMP VIEW v AS SELECT count(*) AS n FROM strs; SELECT n * 2 AS n2 FROM v")
    add("script", "SELECT 1 AS first; SELECT 2 AS second; SELECT 3 AS third")

    return q


def main():
    os.makedirs(OUT, exist_ok=True)
    has_fts = build_db(os.path.join(OUT, "corpus.sqlite"))
    queries = gen_queries(has_fts)
    with open(os.path.join(OUT, "queries.jsonl"), "w") as f:
        for item in queries:
            f.write(json.dumps(item) + "\n")
    cats = {}
    for item in queries:
        cats[item["category"]] = cats.get(item["category"], 0) + 1
    total = len(queries)
    print(f"corpus: {total} queries → {OUT}")
    for k in sorted(cats):
        print(f"  {k:8s} {cats[k]}")
    assert total >= 500, f"corpus must hold ≥500 queries, got {total}"


if __name__ == "__main__":
    main()
