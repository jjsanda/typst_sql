//! Engine integration tests against real database fixtures.
//!
//! Fixtures are deterministic and built by tools/gen_fixtures.py (auto-invoked
//! when missing). These tests exercise the native build of the exact same
//! engine code the WASM plugin ships.

use ciborium::Value;
use quarry_engine::{envelope, Connection};
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

// -- fixture + CBOR helpers ---------------------------------------------------

fn fixture_dir() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    root.join("tests/fixtures/generated")
}

fn ensure_fixtures() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = fixture_dir();
        if !dir.join("basic.sqlite").exists() {
            let script = dir
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("tools/gen_fixtures.py");
            let status = Command::new("python3")
                .arg(script)
                .status()
                .expect("python3 must be available to build fixtures");
            assert!(status.success(), "fixture generation failed");
        }
    });
}

fn fixture(name: &str) -> Vec<u8> {
    ensure_fixtures();
    std::fs::read(fixture_dir().join(name)).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

fn encode(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(v, &mut out).unwrap();
    out
}

fn decode(bytes: &[u8]) -> Value {
    ciborium::from_reader(bytes).unwrap()
}

fn field<'a>(v: &'a Value, key: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    v.as_map()
        .and_then(|m| m.iter().find(|(k, _)| k.as_text() == Some(key)))
        .map(|(_, v)| v)
        .unwrap_or(&NULL)
}

fn meta(now_us: Option<i64>, seed: i64, sources: &[&str]) -> Vec<u8> {
    let mut fields = vec![
        (
            Value::Text("now".into()),
            now_us.map(Value::from).unwrap_or(Value::Null),
        ),
        (
            Value::Text("clock-origin".into()),
            Value::Text(if now_us.is_some() { "explicit" } else { "none" }.into()),
        ),
        (Value::Text("seed".into()), Value::from(seed)),
    ];
    if !sources.is_empty() {
        fields.push((
            Value::Text("sources".into()),
            Value::Array(sources.iter().map(|s| Value::Text(s.to_string())).collect()),
        ));
    }
    encode(&Value::Map(fields))
}

/// 2026-01-20T00:00:00Z in Unix microseconds.
const NOW_US: i64 = 1_768_867_200_000_000;

fn request(sql: &str) -> Vec<u8> {
    encode(&Value::Map(vec![(
        Value::Text("sql".into()),
        Value::Text(sql.into()),
    )]))
}

fn request_with(sql: &str, params: Value, options: Vec<(&str, Value)>) -> Vec<u8> {
    let mut m = vec![(Value::Text("sql".into()), Value::Text(sql.into()))];
    if !params.is_null() {
        m.push((Value::Text("params".into()), params));
    }
    if !options.is_empty() {
        m.push((
            Value::Text("options".into()),
            Value::Map(
                options
                    .into_iter()
                    .map(|(k, v)| (Value::Text(k.into()), v))
                    .collect(),
            ),
        ));
    }
    encode(&Value::Map(m))
}

fn open_basic() -> Connection {
    let db = fixture("basic.sqlite");
    Connection::open(&meta(Some(NOW_US), 42, &[]), &[&db]).expect("open basic.sqlite")
}

fn query_ok(conn: &Connection, req: &[u8]) -> Value {
    let bytes = conn.query(req);
    envelope::assert_tag_free(&bytes);
    let v = decode(&bytes);
    assert_eq!(
        field(&v, "ok"),
        &Value::Bool(true),
        "expected ok envelope, got: {:?}",
        field(&v, "error")
    );
    v
}

fn query_err(conn: &Connection, req: &[u8]) -> Value {
    let bytes = conn.query(req);
    envelope::assert_tag_free(&bytes);
    let v = decode(&bytes);
    assert_eq!(
        field(&v, "ok"),
        &Value::Bool(false),
        "expected error envelope but query succeeded"
    );
    field(&v, "error").clone()
}

fn rows(v: &Value) -> Vec<Value> {
    field(v, "rows").as_array().cloned().unwrap_or_default()
}

fn cell<'a>(row: &'a Value, key: &str) -> &'a Value {
    field(row, key)
}

fn as_i64(v: &Value) -> i64 {
    i64::try_from(i128::from(
        v.as_integer()
            .unwrap_or_else(|| panic!("not an integer: {v:?}")),
    ))
    .unwrap()
}

// -- basic execution ----------------------------------------------------------

#[test]
fn select_scalar() {
    let conn = open_basic();
    let v = query_ok(&conn, &request("SELECT 1 + 1 AS two"));
    assert_eq!(as_i64(cell(&rows(&v)[0], "two")), 2);
}

#[test]
fn rows_are_dictionaries_keyed_by_column() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request("SELECT region, total, n FROM order_totals ORDER BY total DESC"),
    );
    let all = rows(&v);
    assert_eq!(all.len(), 4);
    for row in &all {
        assert!(row.as_map().is_some(), "rows must be CBOR maps");
        assert!(cell(row, "region").as_text().is_some());
        assert!(cell(row, "total").as_integer().is_some());
    }
}

#[test]
fn positional_rows_are_opt_in() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request_with(
            "SELECT region_id, name FROM regions ORDER BY region_id",
            Value::Null,
            vec![("positional-rows", Value::Bool(true))],
        ),
    );
    let all = rows(&v);
    assert!(
        all[0].as_array().is_some(),
        "positional rows must be arrays"
    );
    assert_eq!(all[0].as_array().unwrap()[1], Value::Text("EMEA".into()));
}

#[test]
fn column_metadata_carries_types() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request("SELECT i, r, t, b, d, dt, bool_col, j FROM types_test"),
    );
    let cols = field(&v, "columns").as_array().unwrap().clone();
    let logical: Vec<(String, String)> = cols
        .iter()
        .map(|c| {
            (
                field(c, "name").as_text().unwrap().to_string(),
                field(c, "type").as_text().unwrap().to_string(),
            )
        })
        .collect();
    let get = |name: &str| logical.iter().find(|(n, _)| n == name).unwrap().1.clone();
    assert_eq!(get("i"), "integer");
    assert_eq!(get("r"), "real");
    assert_eq!(get("t"), "text");
    assert_eq!(get("b"), "blob");
    assert_eq!(get("d"), "date");
    assert_eq!(get("dt"), "datetime");
    assert_eq!(get("bool_col"), "boolean");
    assert_eq!(get("j"), "json");
}

// -- correctness core (the incumbent's S1 class) ------------------------------

#[test]
fn int64_boundaries_roundtrip_exactly() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request("SELECT i FROM types_test WHERE id IN (1, 2) ORDER BY id"),
    );
    let all = rows(&v);
    assert_eq!(as_i64(cell(&all[0], "i")), i64::MAX);
    assert_eq!(as_i64(cell(&all[1], "i")), i64::MIN);
}

#[test]
fn int64_text_parsing_no_truncation() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request("SELECT CAST('9223372036854775807' AS INTEGER) AS a, CAST('3000000000' AS INTEGER) AS b"),
    );
    let r = &rows(&v)[0];
    assert_eq!(as_i64(cell(r, "a")), i64::MAX);
    assert_eq!(as_i64(cell(r, "b")), 3_000_000_000);
}

#[test]
fn strtod_exponents_parse() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request("SELECT CAST('1.5e10' AS REAL) AS a, CAST('1.2e-7' AS REAL) AS b, 1.5e10 = CAST('1.5e10' AS REAL) AS same"),
    );
    let r = &rows(&v)[0];
    assert_eq!(cell(r, "a").as_float(), Some(1.5e10));
    assert_eq!(cell(r, "b").as_float(), Some(1.2e-7));
    assert_eq!(as_i64(cell(r, "same")), 1);
}

#[test]
fn math_functions_are_real() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request("SELECT sqrt(2.0) AS s, pow(10.0, 3.0) AS p, log10(1000.0) AS l, ln(1.0) AS z"),
    );
    let r = &rows(&v)[0];
    assert_eq!(r_f(cell(r, "s")), 2.0_f64.sqrt());
    assert_eq!(r_f(cell(r, "p")), 1000.0);
    assert_eq!(r_f(cell(r, "l")), 3.0);
    assert_eq!(r_f(cell(r, "z")), 0.0);
}

fn r_f(v: &Value) -> f64 {
    v.as_float().unwrap_or_else(|| panic!("not a float: {v:?}"))
}

#[test]
fn injected_clock_drives_date_now() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request("SELECT date('now') AS d, datetime('now') AS dt"),
    );
    let r = &rows(&v)[0];
    assert_eq!(cell(r, "d").as_text(), Some("2026-01-20"));
    assert_eq!(cell(r, "dt").as_text(), Some("2026-01-20 00:00:00"));
    // and the near-universal reporting idiom actually filters (D-04):
    let v = query_ok(
        &conn,
        &request("SELECT COUNT(*) AS n FROM orders WHERE created_at > date('now', '-10 days')"),
    );
    let n = as_i64(cell(&rows(&v)[0], "n"));
    assert!(
        n > 0 && n < 200,
        "date filter must select a strict subset, got {n}"
    );
}

#[test]
fn missing_clock_is_a_hard_error_not_epoch() {
    let db = fixture("basic.sqlite");
    let conn = Connection::open(&meta(None, 0, &[]), &[&db]).unwrap();
    let e = query_err(&conn, &request("SELECT date('now')"));
    assert_eq!(field(&e, "code").as_text(), Some("clock-required"));
    let msg = field(&e, "message").as_text().unwrap();
    assert!(msg.contains("quarry-now"), "error must name the fix: {msg}");
    // CURRENT_TIMESTAMP keyword path too
    let e = query_err(&conn, &request("SELECT CURRENT_TIMESTAMP"));
    assert_eq!(field(&e, "code").as_text(), Some("clock-required"));
    // …but a query that never touches time works fine without a clock
    query_ok(&conn, &request("SELECT 1"));
}

#[test]
fn random_is_seeded_and_order_independent() {
    let db = fixture("basic.sqlite");
    let conn_a = Connection::open(&meta(Some(NOW_US), 42, &[]), &[&db]).unwrap();
    let ra1 = conn_a.query(&request("SELECT random() AS r, random() AS r2"));
    // run an unrelated query, then repeat: same bytes (order independence)
    conn_a.query(&request("SELECT count(*) FROM orders"));
    let ra2 = conn_a.query(&request("SELECT random() AS r, random() AS r2"));
    assert_eq!(ra1, ra2, "random() must not depend on prior queries");
    drop(conn_a);

    let conn_b = Connection::open(&meta(Some(NOW_US), 42, &[]), &[&db]).unwrap();
    let rb = conn_b.query(&request("SELECT random() AS r, random() AS r2"));
    assert_eq!(ra1, rb, "same seed ⇒ same random stream");
    drop(conn_b);

    let conn_c = Connection::open(&meta(Some(NOW_US), 43, &[]), &[&db]).unwrap();
    let rc = conn_c.query(&request("SELECT random() AS r, random() AS r2"));
    assert_ne!(ra1, rc, "different seed ⇒ different stream");

    // and within one query the two values differ (not a constant source)
    let v = decode(&ra1);
    let r = &rows(&v)[0];
    assert_ne!(as_i64(cell(r, "r")), as_i64(cell(r, "r2")));
}

#[test]
fn blobs_roundtrip_as_bytes() {
    let conn = open_basic();
    let v = query_ok(&conn, &request("SELECT b FROM types_test WHERE id = 3"));
    let all = rows(&v);
    let bytes = cell(&all[0], "b")
        .as_bytes()
        .expect("BLOB must arrive as CBOR bytes");
    assert_eq!(&bytes[1..4], b"PNG", "PNG fixture blob survives byte-exact");
    // empty blob stays bytes (not null, not text)
    let v = query_ok(&conn, &request("SELECT b FROM types_test WHERE id = 2"));
    assert_eq!(cell(&rows(&v)[0], "b").as_bytes().map(|b| b.len()), Some(0));
}

// -- multi-statement semantics (F-05 / D-07) ----------------------------------

#[test]
fn multi_statement_default_runs_all_returns_last() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request(
            "CREATE TEMP TABLE scratch AS SELECT region, SUM(amount) AS total FROM orders GROUP BY region;\n\
             SELECT COUNT(*) AS n FROM scratch",
        ),
    );
    assert_eq!(as_i64(cell(&rows(&v)[0], "n")), 4);
    let stats = field(&v, "stats");
    assert_eq!(as_i64(field(stats, "statements")), 2);
}

#[test]
fn multi_statement_all_returns_every_result_set() {
    let conn = open_basic();
    let bytes = conn.query(&request_with(
        "SELECT 1 AS a; SELECT 2 AS b; SELECT 3 AS c",
        Value::Null,
        vec![("multi-statement", Value::Text("all".into()))],
    ));
    let v = decode(&bytes);
    assert_eq!(field(&v, "ok"), &Value::Bool(true));
    let results = field(&v, "results").as_array().unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn multi_statement_reject_mode() {
    let conn = open_basic();
    let bytes = conn.query(&request_with(
        "SELECT 1; SELECT 2",
        Value::Null,
        vec![("multi-statement", Value::Text("reject".into()))],
    ));
    let v = decode(&bytes);
    let e = field(&v, "error");
    assert_eq!(field(e, "code").as_text(), Some("multi-statement-rejected"));
}

// -- binding (F-11 / D-14) ----------------------------------------------------

#[test]
fn named_binding_handles_quotes_and_unicode() {
    let conn = open_basic();
    let params = Value::Map(vec![(
        Value::Text("name".into()),
        Value::Text("O'Brien Ltd".into()),
    )]);
    let v = query_ok(
        &conn,
        &request_with(
            "SELECT id FROM customers WHERE name = :name",
            params,
            vec![],
        ),
    );
    assert_eq!(as_i64(cell(&rows(&v)[0], "id")), 1);

    let params = Value::Map(vec![(
        Value::Text("name".into()),
        Value::Text("Ünïcode GmbH".into()),
    )]);
    let v = query_ok(
        &conn,
        &request_with(
            "SELECT id FROM customers WHERE name = :name",
            params,
            vec![],
        ),
    );
    assert_eq!(as_i64(cell(&rows(&v)[0], "id")), 3);
}

#[test]
fn injection_shaped_value_is_inert() {
    let conn = open_basic();
    let params = Value::Map(vec![(
        Value::Text("name".into()),
        Value::Text("' OR 1=1 --".into()),
    )]);
    let v = query_ok(
        &conn,
        &request_with(
            "SELECT COUNT(*) AS n FROM customers WHERE name = :name",
            params,
            vec![],
        ),
    );
    assert_eq!(
        as_i64(cell(&rows(&v)[0], "n")),
        0,
        "bound values must never alter the query"
    );
}

#[test]
fn missing_and_unused_params_error_with_hints() {
    let conn = open_basic();
    // missing
    let e = query_err(
        &conn,
        &request_with(
            "SELECT * FROM orders WHERE fiscal_year = :year",
            Value::Map(vec![]),
            vec![],
        ),
    );
    assert_eq!(field(&e, "code").as_text(), Some("binding-error"));
    assert!(field(&e, "message").as_text().unwrap().contains(":year"));
    // unused (typo) — with a spelling hint
    let e = query_err(
        &conn,
        &request_with(
            "SELECT * FROM orders WHERE fiscal_year = :year",
            Value::Map(vec![
                (Value::Text("year".into()), Value::from(2026)),
                (Value::Text("yeer".into()), Value::from(2025)),
            ]),
            vec![],
        ),
    );
    assert_eq!(field(&e, "code").as_text(), Some("binding-error"));
    assert!(field(&e, "message").as_text().unwrap().contains("yeer"));
}

#[test]
fn positional_binding_works() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request_with(
            "SELECT COUNT(*) AS n FROM orders WHERE fiscal_year = ? AND region = ?",
            Value::Array(vec![Value::from(2026), Value::Text("EMEA".into())]),
            vec![],
        ),
    );
    assert!(as_i64(cell(&rows(&v)[0], "n")) > 0);
}

#[test]
fn typed_params_datetime_bytes_bool_none() {
    let conn = open_basic();
    let params = Value::Map(vec![
        (
            Value::Text("dt".into()),
            Value::Map(vec![(
                Value::Text("@dt".into()),
                Value::Text("2026-01-15 12:00:00".into()),
            )]),
        ),
        (Value::Text("blob".into()), Value::Bytes(vec![0, 1, 2])),
        (Value::Text("flag".into()), Value::Bool(true)),
        (Value::Text("nothing".into()), Value::Null),
    ]);
    let v = query_ok(
        &conn,
        &request_with(
            "SELECT (:dt = '2026-01-15 12:00:00') AS dt_eq, length(:blob) AS bl, :flag AS f, (:nothing IS NULL) AS z",
            params,
            vec![],
        ),
    );
    let r = &rows(&v)[0];
    assert_eq!(as_i64(cell(r, "dt_eq")), 1);
    assert_eq!(as_i64(cell(r, "bl")), 3);
    assert_eq!(as_i64(cell(r, "f")), 1);
    assert_eq!(as_i64(cell(r, "z")), 1);
}

// -- errors (F-16/F-17 / D-17) ------------------------------------------------

#[test]
fn no_such_table_gets_hint_and_offset() {
    let conn = open_basic();
    let e = query_err(&conn, &request("SELECT * FROM custmers"));
    assert_eq!(field(&e, "code").as_text(), Some("sqlite-error"));
    assert_eq!(
        field(&e, "hint").as_text(),
        Some("did you mean \"customers\"?")
    );
    let off = as_i64(field(&e, "offset"));
    let sql = field(&e, "sql").as_text().unwrap();
    let len = as_i64(field(&e, "length")) as usize;
    assert_eq!(&sql[off as usize..off as usize + len], "custmers");
}

#[test]
fn no_such_column_gets_hint() {
    let conn = open_basic();
    let e = query_err(&conn, &request("SELECT regoin FROM orders"));
    assert_eq!(
        field(&e, "hint").as_text(),
        Some("did you mean \"region\"?")
    );
}

#[test]
fn syntax_error_carries_position() {
    let conn = open_basic();
    let e = query_err(&conn, &request("SELECT * FRM orders"));
    assert_eq!(field(&e, "code").as_text(), Some("sqlite-error"));
    assert!(field(&e, "message").as_text().unwrap().contains("syntax"));
}

// -- read-only enforcement ----------------------------------------------------

#[test]
fn writes_are_denied_with_clear_messages() {
    let conn = open_basic();
    for (sql, needle) in [
        ("INSERT INTO orders (id, amount) VALUES (999, 1)", "INSERT"),
        ("UPDATE orders SET amount = 0", "UPDATE"),
        ("DELETE FROM orders", "DELETE"),
        ("DROP TABLE orders", "DROP"),
        ("CREATE TABLE hacked (id INTEGER)", "CREATE"),
        ("ALTER TABLE orders ADD COLUMN x", "ALTER TABLE"),
        ("BEGIN", "transactions"),
        ("ATTACH ':memory:' AS other", "ATTACH"),
        ("PRAGMA journal_mode = DELETE", "PRAGMA"),
        ("ANALYZE", "ANALYZE"),
    ] {
        let e = query_err(&conn, &request(sql));
        assert_eq!(
            field(&e, "code").as_text(),
            Some("read-only-violation"),
            "for SQL: {sql} got {e:?}"
        );
        let msg = field(&e, "message").as_text().unwrap();
        assert!(
            msg.to_lowercase().contains(&needle.to_lowercase()),
            "message for {sql:?} should mention {needle}: {msg}"
        );
    }
    // …and the data is provably untouched
    let v = query_ok(&conn, &request("SELECT COUNT(*) AS n FROM orders"));
    assert_eq!(as_i64(cell(&rows(&v)[0], "n")), 200);
}

#[test]
fn readonly_pragmas_are_allowed() {
    let conn = open_basic();
    query_ok(&conn, &request("PRAGMA table_info(orders)"));
    query_ok(&conn, &request("SELECT * FROM pragma_table_info('orders')"));
    query_ok(&conn, &request("PRAGMA integrity_check"));
}

#[test]
fn temp_tables_are_allowed_and_rolled_back() {
    let conn = open_basic();
    query_ok(
        &conn,
        &request("CREATE TEMP TABLE s AS SELECT 1 AS x; SELECT x FROM s"),
    );
    // the purity guard rolled the temp table back — it must be gone now
    let e = query_err(&conn, &request("SELECT x FROM s"));
    assert!(field(&e, "message")
        .as_text()
        .unwrap()
        .contains("no such table"));
}

// -- multi-source (F-20 / D-20) -----------------------------------------------

#[test]
fn cross_source_join() {
    let basic = fixture("basic.sqlite");
    let refs = fixture("refs.sqlite");
    let conn =
        Connection::open(&meta(Some(NOW_US), 0, &["sales", "refs"]), &[&basic, &refs]).unwrap();
    let v = query_ok(
        &conn,
        &request(
            "SELECT r.name, m.currency FROM sales.regions r \
             JOIN refs.region_meta m USING (region_id) ORDER BY r.region_id",
        ),
    );
    let all = rows(&v);
    assert_eq!(all.len(), 4);
    assert_eq!(cell(&all[0], "name").as_text(), Some("EMEA"));
    assert_eq!(cell(&all[0], "currency").as_text(), Some("EUR"));
    // unqualified names resolve across sources when unique
    let v = query_ok(&conn, &request("SELECT COUNT(*) AS n FROM region_meta"));
    assert_eq!(as_i64(cell(&rows(&v)[0], "n")), 4);
}

#[test]
fn source_name_validation() {
    let basic = fixture("basic.sqlite");
    for bad in ["1abc", "a-b", "a b", "temp", ""] {
        let err = Connection::open(&meta(None, 0, &[bad]), &[&basic])
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.code, "source-invalid", "name {bad:?}");
    }
}

// -- WAL and malformed sources (C-7, F-21) ------------------------------------

#[test]
fn checkpointed_wal_database_opens_with_warning() {
    let wal = fixture("wal.sqlite");
    assert_eq!(wal[18], 2, "fixture must be WAL-marked");
    let conn = Connection::open(&meta(Some(NOW_US), 0, &[]), &[&wal]).unwrap();
    let v = query_ok(&conn, &request("SELECT v FROM t WHERE id = 1"));
    assert_eq!(cell(&rows(&v)[0], "v").as_text(), Some("wal database"));
    let warnings = field(&v, "warnings").as_array().unwrap();
    assert!(warnings
        .iter()
        .any(|w| field(w, "code").as_text() == Some("wal-header-normalized")));
}

#[test]
fn malformed_databases_error_cleanly_never_crash() {
    for name in [
        "empty.sqlite",
        "truncated-header.sqlite",
        "truncated-mid.sqlite",
        "bad-magic.sqlite",
        "bad-page-size.sqlite",
        "corrupt-page.sqlite",
        "liar-page-count.sqlite",
    ] {
        let bytes = fixture(&format!("malformed/{name}"));
        match Connection::open(&meta(None, 0, &[]), &[&bytes]) {
            Err(e) => {
                assert!(
                    e.code == "source-invalid" || e.code == "open-failed",
                    "{name}: unexpected code {}",
                    e.code
                );
                assert!(!e.message.is_empty());
            }
            Ok(conn) => {
                // Some corruption only surfaces at query time; it must be an
                // error envelope, not a crash.
                let bytes = conn.query(&request("SELECT * FROM sqlite_schema"));
                let _ = decode(&bytes);
            }
        }
    }
    // weird-cookie is structurally fine — must open
    let ok = fixture("malformed/weird-cookie.sqlite");
    Connection::open(&meta(None, 0, &[]), &[&ok]).unwrap();
}

// -- caps and chunking (F-06/F-09/D-11/D-12) ----------------------------------

#[test]
fn max_rows_truncates_with_flag() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request_with(
            "SELECT id FROM orders ORDER BY id",
            Value::Null,
            vec![("max-rows", Value::from(10))],
        ),
    );
    assert_eq!(rows(&v).len(), 10);
    assert_eq!(field(field(&v, "stats"), "truncated"), &Value::Bool(true));
}

#[test]
fn row_offset_and_limit_chunk_results() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request_with(
            "SELECT id FROM orders ORDER BY id",
            Value::Null,
            vec![
                ("row-offset", Value::from(50)),
                ("row-limit", Value::from(5)),
            ],
        ),
    );
    let ids: Vec<i64> = rows(&v).iter().map(|r| as_i64(cell(r, "id"))).collect();
    assert_eq!(ids, vec![51, 52, 53, 54, 55]);
}

#[test]
fn ten_megabyte_result_returns_fine() {
    let conn = open_basic();
    // 200 rows × ~64KB of text = ~12.8 MB
    let v = query_ok(
        &conn,
        &request_with(
            "WITH RECURSIVE big(n, payload) AS (\
               SELECT 1, printf('%.65536c', 'x') UNION ALL \
               SELECT n + 1, printf('%.65536c', 'x') FROM big WHERE n < 200) \
             SELECT n, payload FROM big",
            Value::Null,
            vec![("max-bytes", Value::Null), ("max-rows", Value::Null)],
        ),
    );
    let all = rows(&v);
    assert_eq!(all.len(), 200);
    assert_eq!(
        cell(&all[0], "payload").as_text().map(|t| t.len()),
        Some(65536)
    );
    assert_eq!(field(field(&v, "stats"), "truncated"), &Value::Bool(false));
}

// -- schema / describe / capabilities -----------------------------------------

#[test]
fn schema_introspection() {
    let conn = open_basic();
    let bytes = conn.schema(true);
    envelope::assert_tag_free(&bytes);
    let v = decode(&bytes);
    assert_eq!(field(&v, "ok"), &Value::Bool(true));
    let sources = field(&v, "sources").as_array().unwrap();
    let tables = field(&sources[0], "tables").as_array().unwrap();
    let orders = tables
        .iter()
        .find(|t| field(t, "name").as_text() == Some("orders"))
        .expect("orders table listed");
    assert_eq!(as_i64(field(orders, "row-count")), 200);
    let cols = field(orders, "columns").as_array().unwrap();
    assert!(cols
        .iter()
        .any(|c| field(c, "name").as_text() == Some("region")));
    let fks = field(orders, "foreign-keys").as_array().unwrap();
    assert_eq!(field(&fks[0], "table").as_text(), Some("regions"));
    let view = tables
        .iter()
        .find(|t| field(t, "name").as_text() == Some("order_totals"))
        .expect("view listed");
    assert_eq!(field(view, "kind").as_text(), Some("view"));
}

#[test]
fn describe_reports_columns_and_params() {
    let conn = open_basic();
    let bytes = conn.describe(&request(
        "SELECT region, amount FROM orders WHERE fiscal_year = :year",
    ));
    let v = decode(&bytes);
    let stmts = field(&v, "statements").as_array().unwrap();
    let cols = field(&stmts[0], "columns").as_array().unwrap();
    assert_eq!(cols.len(), 2);
    let params = field(&stmts[0], "parameters").as_array().unwrap();
    assert_eq!(params[0].as_text(), Some(":year"));
    assert_eq!(field(&stmts[0], "read-only"), &Value::Bool(true));
}

#[test]
fn capabilities_match_reality() {
    let caps = decode(&quarry_engine::capabilities_bytes());
    let engine = field(&caps, "engine-version").as_text().unwrap();
    let pinned = field(&caps, "pinned-engine-version").as_text().unwrap();
    assert_eq!(engine, pinned, "runtime engine differs from the pin (D-24)");
    let supports = field(&caps, "supports");
    assert_eq!(field(supports, "math-functions"), &Value::Bool(true));
    assert_eq!(field(supports, "load-extension"), &Value::Bool(false));

    let conn = open_basic();
    // claimed features must actually work
    query_ok(&conn, &request("SELECT sqrt(4.0)")); // math
    query_ok(&conn, &request("SELECT json_extract('{\"a\": 1}', '$.a')")); // json
    query_ok(
        &conn,
        &request("SELECT region, SUM(amount) OVER (PARTITION BY region) FROM orders LIMIT 1"),
    ); // window functions
    query_ok(
        &conn,
        &request("WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM c WHERE n < 5) SELECT max(n) FROM c"),
    ); // CTEs
       // FTS5 (present when the fixture builder had it)
    let bytes = conn.query(&request(
        "SELECT title FROM docs WHERE docs MATCH 'revenue' ORDER BY rank",
    ));
    let v = decode(&bytes);
    if field(&v, "ok") == &Value::Bool(true) {
        assert_eq!(rows(&v).len(), 2);
    }
}

// -- purity (native slice; the full fresh-vs-dirty test runs under wasmi) -----

#[test]
fn identical_requests_yield_identical_envelopes() {
    let conn = open_basic();
    let req = request(
        "SELECT region, SUM(amount) AS total FROM orders GROUP BY region ORDER BY total DESC",
    );
    let a = conn.query(&req);
    let _noise = conn.query(&request(
        "CREATE TEMP TABLE tmp1 AS SELECT * FROM orders; SELECT count(*) FROM tmp1",
    ));
    let b = conn.query(&req);
    assert_eq!(
        a, b,
        "envelopes must be byte-identical regardless of interleaved queries"
    );
}

#[test]
fn last_insert_rowid_does_not_leak_across_queries() {
    let conn = open_basic();
    let v = query_ok(
        &conn,
        &request(
            "CREATE TEMP TABLE t(x); INSERT INTO t VALUES (7); SELECT last_insert_rowid() AS lid",
        ),
    );
    assert_eq!(as_i64(cell(&rows(&v)[0], "lid")), 1);
    // after the guard, the next query must see a clean slate
    let v = query_ok(&conn, &request("SELECT last_insert_rowid() AS lid"));
    assert_eq!(as_i64(cell(&rows(&v)[0], "lid")), 0);
}
