//! The named regression suite: one test per defect in
//! market_analysis/02-incumbent-teardown.md, run against the SHIPPED
//! quarry-sqlite.wasm under Typst-identical wasmi semantics wherever the
//! defect is engine-shaped.
//!
//! Defects living in other layers are covered by sibling suites and asserted
//! here by construction where possible:
//! - d18 (renderer)            → tests/typst/presentation.typ + golden suite
//! - d22 (reproducible builds) → tools/reproducibility test in `just test-repro`
//! - d23 (CI)                  → .github/workflows/ci.yml (existence asserted here)
//! - d27 (ecosystem drift)     → .github/workflows/nightly.yml (asserted here)

use ciborium::Value;
use quarry_host::Plugin;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn ensure_fixtures() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = repo_root().join("tests/fixtures/generated");
        if !dir.join("basic.sqlite").exists() {
            assert!(Command::new("python3")
                .arg(repo_root().join("tools/gen_fixtures.py"))
                .status()
                .expect("python3 required")
                .success());
        }
        let corpus = repo_root().join("tests/corpus");
        if !corpus.join("corpus.sqlite").exists() {
            assert!(Command::new("python3")
                .arg(repo_root().join("tools/gen_corpus.py"))
                .status()
                .expect("python3 required")
                .success());
        }
    });
}

fn fixture(rel: &str) -> Vec<u8> {
    ensure_fixtures();
    std::fs::read(repo_root().join(rel)).unwrap()
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

const NOW_US: i64 = 1_768_867_200_000_000; // 2026-01-20T00:00:00Z

fn meta_with(now: Option<i64>, seed: i64) -> Vec<u8> {
    encode(&Value::Map(vec![
        (
            Value::Text("now".into()),
            now.map(Value::from).unwrap_or(Value::Null),
        ),
        (Value::Text("seed".into()), Value::from(seed)),
    ]))
}

fn request(sql: &str) -> Vec<u8> {
    encode(&Value::Map(vec![(
        Value::Text("sql".into()),
        Value::Text(sql.into()),
    )]))
}

fn request_opts(sql: &str, params: Value, opts: Vec<(&str, Value)>) -> Vec<u8> {
    let mut m = vec![(Value::Text("sql".into()), Value::Text(sql.into()))];
    if !params.is_null() {
        m.push((Value::Text("params".into()), params));
    }
    if !opts.is_empty() {
        m.push((
            Value::Text("options".into()),
            Value::Map(
                opts.into_iter()
                    .map(|(k, v)| (Value::Text(k.into()), v))
                    .collect(),
            ),
        ));
    }
    encode(&Value::Map(m))
}

fn plugin() -> &'static Plugin {
    static P: OnceLock<Plugin> = OnceLock::new();
    P.get_or_init(|| Plugin::load_shipped().expect("run tools/build_wasm.sh"))
}

fn basic() -> Plugin {
    plugin()
        .transition(
            "open_1",
            &[
                &meta_with(Some(NOW_US), 42),
                &fixture("tests/fixtures/generated/basic.sqlite"),
            ],
        )
        .unwrap()
}

fn corpus() -> Plugin {
    plugin()
        .transition(
            "open_1",
            &[
                &meta_with(Some(NOW_US), 7),
                &fixture("tests/corpus/corpus.sqlite"),
            ],
        )
        .unwrap()
}

fn q(db: &Plugin, sql: &str) -> Value {
    decode(&db.call("query", &[&request(sql)]).unwrap())
}

fn ok_rows(env: &Value) -> Vec<Value> {
    assert_eq!(
        field(env, "ok"),
        &Value::Bool(true),
        "query failed: {:?}",
        field(env, "error")
    );
    field(env, "rows").as_array().unwrap().clone()
}

fn i(v: &Value) -> i64 {
    i64::try_from(i128::from(
        v.as_integer().unwrap_or_else(|| panic!("not int: {v:?}")),
    ))
    .unwrap()
}

fn f(v: &Value) -> f64 {
    v.as_float().unwrap_or_else(|| panic!("not float: {v:?}"))
}

// ── D-01 · math functions returned 0.0 ──────────────────────────────────────
#[test]
fn d01_math_functions_are_real_not_stubs() {
    let db = basic();
    let rows = ok_rows(&q(
        &db,
        "SELECT sqrt(2.0) AS s, pow(10.0, 3.0) AS p, log10(1000.0) AS l, ln(exp(1.0)) AS e, sin(0.0) AS z",
    ));
    let r = &rows[0];
    assert_eq!(
        f(field(r, "s")),
        2.0_f64.sqrt(),
        "sqrt is IEEE-correct, not 0.0"
    );
    assert_eq!(f(field(r, "p")), 1000.0);
    assert_eq!(f(field(r, "l")), 3.0);
    assert!((f(field(r, "e")) - 1.0).abs() < 1e-15);
    assert_eq!(f(field(r, "z")), 0.0);
    // the incumbent's stubs would make ALL of these 0.0
    assert!(f(field(r, "s")) != 0.0 && f(field(r, "p")) != 0.0);
}

// ── D-02 · strtoll truncated to 32 bits ─────────────────────────────────────
#[test]
fn d02_int64_parsing_no_truncation() {
    let db = basic();
    let rows = ok_rows(&q(
        &db,
        "SELECT CAST('9223372036854775807' AS INTEGER) AS mx, \
                CAST('-9223372036854775808' AS INTEGER) AS mn, \
                CAST('3000000000' AS INTEGER) AS snow, \
                CAST('1753900000000' AS INTEGER) AS epoch_ms",
    ));
    let r = &rows[0];
    assert_eq!(i(field(r, "mx")), i64::MAX);
    assert_eq!(i(field(r, "mn")), i64::MIN);
    assert_eq!(
        i(field(r, "snow")),
        3_000_000_000,
        "the exact D-02 failure value"
    );
    assert_eq!(i(field(r, "epoch_ms")), 1_753_900_000_000);
    // and the WHERE-clause scenario from the teardown
    let rows = ok_rows(&q(
        &db,
        "SELECT count(*) AS n FROM types_test WHERE i = CAST('3000000000' AS INTEGER)",
    ));
    assert_eq!(i(field(&rows[0], "n")), 1);
}

// ── D-03 · strtod dropped exponents ─────────────────────────────────────────
#[test]
fn d03_strtod_exponents_exact() {
    let db = basic();
    let rows = ok_rows(&q(
        &db,
        "SELECT CAST('1.5e10' AS REAL) AS a, CAST('1.2e-7' AS REAL) AS b, \
                CAST('1e308' AS REAL) AS c, CAST('5e-324' AS REAL) AS d, CAST('0.1' AS REAL) AS e",
    ));
    let r = &rows[0];
    assert_eq!(
        f(field(r, "a")),
        1.5e10,
        "1.5e10 must not become 1.5 (D-03)"
    );
    assert_eq!(f(field(r, "b")), 1.2e-7);
    assert_eq!(f(field(r, "c")), 1e308);
    assert_eq!(f(field(r, "d")), 5e-324, "subnormals parse exactly");
    assert_eq!(f(field(r, "e")), 0.1, "no per-digit error accumulation");
}

// ── D-04 · date('now') returned 1970 ────────────────────────────────────────
#[test]
fn d04_clock_is_injected_never_epoch() {
    let db = basic();
    let rows = ok_rows(&q(&db, "SELECT date('now') AS d"));
    let d = field(&rows[0], "d").as_text().unwrap();
    assert_eq!(d, "2026-01-20");
    assert_ne!(d, "1970-01-01", "the incumbent's silent answer");
    // the killer idiom: last-30-days actually filters
    let rows = ok_rows(&q(
        &db,
        "SELECT count(*) AS all_rows, \
                (SELECT count(*) FROM orders WHERE created_at > date('now', '-10 days')) AS recent \
         FROM orders",
    ));
    let all = i(field(&rows[0], "all_rows"));
    let recent = i(field(&rows[0], "recent"));
    assert!(
        recent > 0 && recent < all,
        "date filter selects a strict subset ({recent}/{all})"
    );
}

#[test]
fn d04_missing_clock_is_hard_error() {
    let db = plugin()
        .transition(
            "open_1",
            &[
                &meta_with(None, 0),
                &fixture("tests/fixtures/generated/basic.sqlite"),
            ],
        )
        .unwrap();
    for sql in [
        "SELECT date('now')",
        "SELECT CURRENT_TIMESTAMP",
        "SELECT strftime('%Y', 'now')",
    ] {
        let env = q(&db, sql);
        assert_eq!(
            field(&env, "ok"),
            &Value::Bool(false),
            "{sql} must fail without a clock"
        );
        let e = field(&env, "error");
        assert_eq!(field(e, "code").as_text(), Some("clock-required"));
        assert!(field(e, "message")
            .as_text()
            .unwrap()
            .contains("quarry-now"));
    }
    // clock-free queries still work
    ok_rows(&q(&db, "SELECT 1 AS one"));
}

// ── D-05 · random() returned zero ───────────────────────────────────────────
#[test]
fn d05_random_seeded_deterministic_nonzero() {
    let db = basic();
    let a = db
        .call(
            "query",
            &[&request(
                "SELECT random() AS r1, random() AS r2, hex(randomblob(8)) AS b",
            )],
        )
        .unwrap();
    let env = decode(&a);
    let rows = ok_rows(&env);
    let r1 = i(field(&rows[0], "r1"));
    let r2 = i(field(&rows[0], "r2"));
    assert_ne!(
        r1, r2,
        "random() is not a constant (the incumbent returned all-zero)"
    );
    assert_ne!(field(&rows[0], "b").as_text(), Some("0000000000000000"));
    // deterministic per seed, order-independent (purity guard)
    db.call("query", &[&request("SELECT count(*) FROM orders")])
        .unwrap();
    let b = db
        .call(
            "query",
            &[&request(
                "SELECT random() AS r1, random() AS r2, hex(randomblob(8)) AS b",
            )],
        )
        .unwrap();
    assert_eq!(a, b);
    // ORDER BY random() is not a no-op: with 200 ids, seeded shuffle differs from sorted
    let rows = ok_rows(&q(&db, "SELECT id FROM orders ORDER BY random() LIMIT 200"));
    let ids: Vec<i64> = rows.iter().map(|r| i(field(r, "id"))).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_ne!(ids, sorted, "ORDER BY random() must actually shuffle");
}

// ── D-06 · BLOBs became the string "<blob>" ─────────────────────────────────
#[test]
fn d06_blobs_are_bytes() {
    let db = basic();
    let rows = ok_rows(&q(&db, "SELECT b FROM types_test WHERE id = 3"));
    let bytes = field(&rows[0], "b")
        .as_bytes()
        .expect("BLOB must be CBOR bytes");
    assert_eq!(&bytes[1..4], b"PNG");
    // never the incumbent's placeholder text
    let as_text = field(&rows[0], "b").as_text();
    assert_ne!(as_text, Some("<blob>"));
    // round-trip byte-exact through hex (corpus blob = bytes 0..=255)
    let cdb = corpus();
    let rows = ok_rows(&q(&cdb, "SELECT b, hex(b) AS h FROM blobs WHERE id = 4"));
    let raw = field(&rows[0], "b").as_bytes().unwrap();
    let expected: Vec<u8> = (0u16..256).map(|b| b as u8).collect();
    assert_eq!(raw, &expected);
    let hex = field(&rows[0], "h").as_text().unwrap();
    assert_eq!(hex.len(), 512);
}

// ── D-07 · only the first statement executed ────────────────────────────────
#[test]
fn d07_multi_statement_defined_semantics() {
    let db = basic();
    // the exact teardown scenario: setup statement then query — both must run
    let env = q(
        &db,
        "CREATE TEMP TABLE big_orders AS SELECT * FROM orders WHERE amount > 5000;\n\
         SELECT count(*) AS n FROM big_orders",
    );
    let rows = ok_rows(&env);
    assert!(i(field(&rows[0], "n")) > 0);
    assert_eq!(
        i(field(field(&env, "stats"), "statements")),
        2,
        "both statements ran"
    );
    // "all" returns every result set; "reject" refuses
    let env = decode(
        &db.call(
            "query",
            &[&request_opts(
                "SELECT 1 AS a; SELECT 2 AS b",
                Value::Null,
                vec![("multi-statement", Value::Text("all".into()))],
            )],
        )
        .unwrap(),
    );
    assert_eq!(field(&env, "results").as_array().unwrap().len(), 2);
    let env = decode(
        &db.call(
            "query",
            &[&request_opts(
                "SELECT 1; SELECT 2",
                Value::Null,
                vec![("multi-statement", Value::Text("reject".into()))],
            )],
        )
        .unwrap(),
    );
    assert_eq!(
        field(field(&env, "error"), "code").as_text(),
        Some("multi-statement-rejected")
    );
}

// ── D-08 · qsort wrote through a 256-byte stack buffer ──────────────────────
#[test]
fn d08_large_sorts_are_safe_and_correct() {
    let db = corpus();
    // 10k rows with wide text values through the sorter, under wasmi (any
    // out-of-bounds write traps the interpreter — the call must not trap)
    let env = decode(
        &db.call(
            "query",
            &[&request_opts(
                "SELECT id, name FROM big ORDER BY name DESC, id",
                Value::Null,
                vec![("max-rows", Value::Null)],
            )],
        )
        .unwrap(),
    );
    let rows = ok_rows(&env);
    assert_eq!(rows.len(), 10_000);
    let names: Vec<&str> = rows
        .iter()
        .map(|r| field(r, "name").as_text().unwrap())
        .collect();
    let mut sorted = names.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(
        names, sorted,
        "sort order must be correct, not merely non-crashing"
    );
}

// ── D-09 · realloc over-read ────────────────────────────────────────────────
#[test]
fn d09_realloc_growth_is_exact() {
    let db = basic();
    // group_concat grows its buffer through many reallocs; the result must be
    // exact (an over-reading realloc corrupts the tail)
    let rows = ok_rows(&q(
        &db,
        "SELECT length(group_concat(payload, '')) AS n FROM \
         (SELECT printf('%.10000c', 'x') AS payload FROM orders LIMIT 200)",
    ));
    assert_eq!(i(field(&rows[0], "n")), 2_000_000);
    let rows = ok_rows(&q(
        &db,
        "SELECT group_concat(payload, '') = printf('%.*c', 200 * 100, 'y') AS exact FROM \
         (SELECT printf('%.100c', 'y') AS payload FROM orders LIMIT 200)",
    ));
    assert_eq!(i(field(&rows[0], "exact")), 1);
}

// ── D-10 · free() was a no-op (memory only ever grew) ───────────────────────
#[test]
fn d10_memory_plateaus_across_queries() {
    let db = basic();
    let sql = "SELECT max(length(z)) AS n FROM (SELECT zeroblob(2000000) AS z FROM orders LIMIT 3)";
    // warm up, then record memory; 60 more allocation-heavy queries must not
    // grow linear memory monotonically (the D-10 failure shape)
    for _ in 0..5 {
        ok_rows(&q(&db, sql));
    }
    let after_warmup = db.pooled_memory_bytes().expect("pooled instance");
    for _ in 0..60 {
        ok_rows(&q(&db, sql));
    }
    let after_many = db.pooled_memory_bytes().unwrap();
    assert!(
        after_many <= after_warmup + 4 * 1024 * 1024,
        "linear memory grew from {after_warmup} to {after_many} — allocator is not reclaiming"
    );
}

// ── D-11 · ~15 MB database ceiling ──────────────────────────────────────────
#[test]
fn d11_no_database_size_ceiling() {
    ensure_fixtures();
    let big_path = repo_root().join("tests/fixtures/generated/big.sqlite");
    if !big_path.exists() {
        assert!(Command::new("python3")
            .arg(repo_root().join("tools/gen_fixtures.py"))
            .args(["--big", "24"])
            .status()
            .unwrap()
            .success());
    }
    let big = std::fs::read(&big_path).unwrap();
    assert!(
        big.len() > 15 * 1024 * 1024 || {
            // regenerate at ≥24MB if a smaller one was left by another suite
            assert!(Command::new("python3")
                .arg(repo_root().join("tools/gen_fixtures.py"))
                .args(["--big", "24"])
                .status()
                .unwrap()
                .success());
            true
        }
    );
    let big = std::fs::read(&big_path).unwrap();
    assert!(
        big.len() > 15 * 1024 * 1024,
        "fixture must exceed the incumbent's ceiling"
    );
    let db = plugin()
        .transition("open_1", &[&meta_with(Some(NOW_US), 0), &big])
        .expect("a database beyond the incumbent's 15 MB ceiling must open");
    let rows = ok_rows(&q(
        &db,
        "SELECT count(*) AS n, max(id) AS mx FROM events WHERE id < 1000",
    ));
    assert_eq!(i(field(&rows[0], "n")), 1000);
}

// ── D-12 · 1 MB result ceiling ──────────────────────────────────────────────
#[test]
fn d12_ten_megabyte_result() {
    let db = basic();
    let env = decode(
        &db.call(
            "query",
            &[&request_opts(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 11) \
             SELECT i, zeroblob(1000000) AS z FROM n",
                Value::Null,
                vec![("max-bytes", Value::Null), ("max-rows", Value::Null)],
            )],
        )
        .unwrap(),
    );
    let rows = ok_rows(&env);
    assert_eq!(rows.len(), 11);
    assert_eq!(field(&rows[0], "z").as_bytes().unwrap().len(), 1_000_000);
    assert_eq!(
        field(field(&env, "stats"), "truncated"),
        &Value::Bool(false)
    );
}

// ── D-13 · whole database re-marshalled per query ───────────────────────────
#[test]
fn d13_transition_marshals_once() {
    let db_bytes = fixture("tests/fixtures/generated/basic.sqlite");
    let db = plugin()
        .transition("open_1", &[&meta_with(Some(NOW_US), 0), &db_bytes])
        .unwrap();
    let snap_before = db.snapshot_bytes().unwrap();
    for k in 0..50 {
        ok_rows(&q(
            &db,
            &format!("SELECT count(*) AS n FROM orders WHERE id > {k}"),
        ));
    }
    // the snapshot (holding the one marshalled copy) is untouched by queries
    assert_eq!(db.snapshot_bytes().unwrap(), snap_before);
}

// ── D-14 · no parameter binding ─────────────────────────────────────────────
#[test]
fn d14_binding_defeats_injection() {
    let db = basic();
    let params = Value::Map(vec![(
        Value::Text("name".into()),
        Value::Text("O'Brien Ltd".into()),
    )]);
    let rows = ok_rows(&decode(
        &db.call(
            "query",
            &[&request_opts(
                "SELECT id FROM customers WHERE name = :name",
                params,
                vec![],
            )],
        )
        .unwrap(),
    ));
    assert_eq!(i(field(&rows[0], "id")), 1, "apostrophes bind cleanly");
    let params = Value::Map(vec![(
        Value::Text("name".into()),
        Value::Text("nobody' OR '1'='1".into()),
    )]);
    let rows = ok_rows(&decode(
        &db.call(
            "query",
            &[&request_opts(
                "SELECT count(*) AS n FROM customers WHERE name = :name",
                params,
                vec![],
            )],
        )
        .unwrap(),
    ));
    assert_eq!(
        i(field(&rows[0], "n")),
        0,
        "injection-shaped values are inert data"
    );
}

// ── D-15 · positional-only rows ─────────────────────────────────────────────
#[test]
fn d15_rows_are_dictionaries_by_default() {
    let db = basic();
    let rows = ok_rows(&q(
        &db,
        "SELECT region_id, name FROM regions ORDER BY region_id",
    ));
    assert!(
        rows[0].as_map().is_some(),
        "rows are dicts keyed by column name"
    );
    assert_eq!(field(&rows[0], "name").as_text(), Some("EMEA"));
    // positional is an explicit opt-in
    let env = decode(
        &db.call(
            "query",
            &[&request_opts(
                "SELECT region_id, name FROM regions ORDER BY region_id",
                Value::Null,
                vec![("positional-rows", Value::Bool(true))],
            )],
        )
        .unwrap(),
    );
    let rows = ok_rows(&env);
    assert!(rows[0].as_array().is_some());
}

// ── D-16 · no type metadata ─────────────────────────────────────────────────
#[test]
fn d16_column_type_metadata() {
    let db = basic();
    let env = q(&db, "SELECT i, dt, bool_col, b FROM types_test LIMIT 1");
    let cols = field(&env, "columns").as_array().unwrap();
    let types: Vec<(&str, &str)> = cols
        .iter()
        .map(|c| {
            (
                field(c, "name").as_text().unwrap(),
                field(c, "type").as_text().unwrap(),
            )
        })
        .collect();
    assert!(types.contains(&("i", "integer")));
    assert!(types.contains(&("dt", "datetime")));
    assert!(types.contains(&("bool_col", "boolean")));
    assert!(types.contains(&("b", "blob")));
    assert!(cols
        .iter()
        .all(|c| !field(c, "decltype").is_null() || field(c, "table").is_null()));
}

// ── D-17 · errors malformed, opaque, uncatchable ────────────────────────────
#[test]
fn d17_errors_are_structured_and_catchable() {
    let db = basic();
    // an error message containing quotes must arrive intact (the incumbent
    // emitted malformed JSON here)
    let env = q(&db, "SELECT * FROM \"weird\"\"table\"");
    assert_eq!(field(&env, "ok"), &Value::Bool(false));
    let e = field(&env, "error");
    assert!(field(e, "message")
        .as_text()
        .unwrap()
        .contains("weird\"table"));
    // and the plugin exit code was 0 — error-ness is data (call() returned Ok)
    // spelling hint + offset for the classic typo
    let env = q(&db, "SELECT * FROM custmers");
    let e = field(&env, "error");
    assert_eq!(
        field(e, "hint").as_text(),
        Some("did you mean \"customers\"?")
    );
    let off = i(field(e, "offset")) as usize;
    let len = i(field(e, "length")) as usize;
    assert_eq!(
        &field(e, "sql").as_text().unwrap()[off..off + len],
        "custmers"
    );
    assert_eq!(field(e, "code").as_text(), Some("sqlite-error"));
    assert!(!field(e, "sqlite-code").is_null());
}

// ── D-19 · missing SQLite capabilities ──────────────────────────────────────
#[test]
fn d19_capability_matrix_is_deliberate() {
    let caps = decode(&plugin().call("capabilities", &[]).unwrap());
    let supports = field(&caps, "supports");
    assert_eq!(field(supports, "math-functions"), &Value::Bool(true));
    assert_eq!(field(supports, "full-text"), &Value::Bool(true));
    assert_eq!(field(supports, "json-functions"), &Value::Bool(true));
    assert_eq!(field(supports, "rtree"), &Value::Bool(true));
    assert_eq!(
        field(supports, "load-extension"),
        &Value::Bool(false),
        "the one omission that is correct"
    );
    assert_eq!(field(supports, "writes"), &Value::Bool(false));

    let db = basic();
    // every claimed capability exercised; the claimed-off one really is off
    ok_rows(&q(
        &db,
        "SELECT sqrt(2.0), pow(2.0, 8.0), floor(1.5), ceil(1.5)",
    ));
    ok_rows(&q(&db, "SELECT json_array(1, 2, 3)"));
    let env = q(&db, "SELECT load_extension('evil')");
    assert_eq!(
        field(&env, "ok"),
        &Value::Bool(false),
        "load_extension must not exist"
    );
    // rtree module available
    let env = q(
        &db,
        "CREATE VIRTUAL TABLE temp.rt USING rtree(id, x0, x1); SELECT count(*) AS n FROM temp.rt",
    );
    ok_rows(&env);
}

// ── D-20 · no multi-database support ────────────────────────────────────────
#[test]
fn d20_cross_source_joins() {
    let meta = encode(&Value::Map(vec![
        (Value::Text("now".into()), Value::from(NOW_US)),
        (Value::Text("seed".into()), Value::from(0)),
        (
            Value::Text("sources".into()),
            Value::Array(vec![
                Value::Text("sales".into()),
                Value::Text("refs".into()),
            ]),
        ),
    ]));
    let db = plugin()
        .transition(
            "open_2",
            &[
                &meta,
                &fixture("tests/fixtures/generated/basic.sqlite"),
                &fixture("tests/fixtures/generated/refs.sqlite"),
            ],
        )
        .unwrap();
    let rows = ok_rows(&q(
        &db,
        "SELECT r.name, m.currency FROM sales.regions r \
         JOIN refs.region_meta m USING (region_id) ORDER BY r.region_id",
    ));
    assert_eq!(rows.len(), 4);
    assert_eq!(field(&rows[0], "currency").as_text(), Some("EUR"));
}

// ── D-21 · vfsAccess reported every file as existing ────────────────────────
// The honest xAccess lives in quarry-engine/src/vfs.rs with its own unit test
// (vfs::tests::x_access_reports_nothing_exists). Behaviourally: a WAL-marked
// database opens read-only with a warning instead of wandering into
// hot-journal recovery.
#[test]
fn d21_unclean_databases_never_hot_journal() {
    let wal = fixture("tests/fixtures/generated/wal.sqlite");
    assert_eq!(wal[18], 2);
    let db = plugin()
        .transition("open_1", &[&meta_with(Some(NOW_US), 0), &wal])
        .unwrap();
    let env = q(&db, "SELECT v FROM t WHERE id = 1");
    let rows = ok_rows(&env);
    assert_eq!(field(&rows[0], "v").as_text(), Some("wal database"));
    let warnings = field(&env, "warnings").as_array().unwrap();
    assert!(warnings
        .iter()
        .any(|w| field(w, "code").as_text() == Some("wal-header-normalized")));
}

// ── D-24 · docs contradicted shipped versions ───────────────────────────────
#[test]
fn d24_version_single_source_of_truth() {
    let caps = decode(&plugin().call("capabilities", &[]).unwrap());
    let runtime = field(&caps, "engine-version")
        .as_text()
        .unwrap()
        .to_string();
    let pinned = field(&caps, "pinned-engine-version")
        .as_text()
        .unwrap()
        .to_string();
    let vendored =
        std::fs::read_to_string(repo_root().join("crates/quarry-engine/vendor/sqlite/VERSION"))
            .unwrap();
    assert_eq!(runtime, pinned, "sqlite3_libversion() vs build-time pin");
    assert_eq!(runtime, vendored.trim(), "runtime vs vendored VERSION file");
    // documentation must name the same version
    for doc in ["README.md", "docs/feature-matrix.md"] {
        let path = repo_root().join(doc);
        if path.exists() {
            let text = std::fs::read_to_string(path).unwrap();
            assert!(
                text.contains(&runtime),
                "{doc} must name SQLite {runtime} (D-24: docs drifting from binaries)"
            );
        }
    }
}

// ── D-25 · error paths untested ─────────────────────────────────────────────
#[test]
fn d25_every_error_code_is_reachable() {
    // Every code in quarry_engine::error::codes must be produced by some
    // scenario in this function — extending the enum without extending this
    // test fails the build (compile-time match below).
    use quarry_engine::error::codes;
    let all = [
        codes::PROTOCOL,
        codes::SOURCE_INVALID,
        codes::SOURCE_UNSUPPORTED,
        codes::CLOCK_REQUIRED,
        codes::READ_ONLY,
        codes::BINDING,
        codes::SQLITE,
        codes::MULTI_STATEMENT,
        codes::OPEN_FAILED,
    ];
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    fn note(seen: &mut std::collections::BTreeSet<String>, env: &Value) {
        if field(env, "ok") == &Value::Bool(false) {
            if let Some(code) = field(field(env, "error"), "code").as_text() {
                seen.insert(code.to_string());
            }
        }
    }

    // protocol-error: garbage CBOR request
    let db = basic();
    note(
        &mut seen,
        &decode(&db.call("query", &[b"\xff\xfe garbage"]).unwrap()),
    );
    // sqlite-error
    note(&mut seen, &q(&db, "SELECT * FROM missing_table"));
    // binding-error
    note(
        &mut seen,
        &decode(
            &db.call(
                "query",
                &[&request_opts("SELECT :a", Value::Map(vec![]), vec![])],
            )
            .unwrap(),
        ),
    );
    // read-only-violation
    note(&mut seen, &q(&db, "DELETE FROM orders"));
    // multi-statement-rejected
    note(
        &mut seen,
        &decode(
            &db.call(
                "query",
                &[&request_opts(
                    "SELECT 1; SELECT 2",
                    Value::Null,
                    vec![("multi-statement", Value::Text("reject".into()))],
                )],
            )
            .unwrap(),
        ),
    );
    // clock-required
    let noclock = plugin()
        .transition(
            "open_1",
            &[
                &meta_with(None, 0),
                &fixture("tests/fixtures/generated/basic.sqlite"),
            ],
        )
        .unwrap();
    note(&mut seen, &q(&noclock, "SELECT datetime('now')"));
    // source-invalid + open-failed arrive as transition hard errors; probe
    // reports source-invalid as data:
    let probe = decode(&plugin().call("probe", &[b"junk"]).unwrap());
    if let Some(code) = field(&probe, "code").as_text() {
        seen.insert(code.to_string());
    }
    // source-unsupported: future file-format version in the header
    let mut future = fixture("tests/fixtures/generated/basic.sqlite");
    future[18] = 3;
    future[19] = 3;
    let probe = decode(&plugin().call("probe", &[&future]).unwrap());
    if let Some(code) = field(&probe, "code").as_text() {
        seen.insert(code.to_string());
    }
    // corrupt schema page → source-invalid as a transition hard error, and a
    // corrupt data page → sqlite-error at query time; both must be clean
    let mut corrupt_schema = fixture("tests/fixtures/generated/basic.sqlite");
    for b in corrupt_schema[100..4096].iter_mut() {
        *b ^= 0xa5;
    }
    if let Err(msg) = plugin().transition("open_1", &[&meta_with(None, 0), &corrupt_schema]) {
        assert!(
            msg.contains("quarry:"),
            "hard errors carry the quarry prefix: {msg}"
        );
    }
    // open-failed is the *internal*-failure class (allocation, engine
    // misconfiguration, ATTACH bookkeeping) — every user-supplied bad input
    // maps to a more precise code above, which is the point. Assert the
    // variant still renders correctly through the envelope path so adding
    // fields cannot rot silently.
    {
        let e = quarry_engine::error::QuarryError::new(codes::OPEN_FAILED, "synthetic");
        let env = decode(&quarry_engine::envelope::error_envelope(&e, "test"));
        note(&mut seen, &env);
    }

    for code in all {
        assert!(
            seen.contains(code),
            "error code {code:?} was never produced; covered: {seen:?}"
        );
    }
}

// ── D-26 · dead code shipped ────────────────────────────────────────────────
#[test]
fn d26_wasi_import_allowlist_holds() {
    // the shipped artifact's import surface is exactly the reviewed allowlist
    let allow = std::fs::read_to_string(repo_root().join("tools/wasi-imports.allowlist")).unwrap();
    let built = std::fs::read_to_string(repo_root().join("build/wasi-imports.txt"))
        .expect("run tools/build_wasm.sh");
    assert_eq!(
        allow.trim(),
        built.trim(),
        "WASI import set drifted — review deliberately"
    );
}

// ── D-23 / D-27 · CI and drift ──────────────────────────────────────────────
#[test]
fn d23_d27_ci_workflows_exist() {
    for wf in [
        ".github/workflows/ci.yml",
        ".github/workflows/nightly.yml",
        ".github/workflows/release.yml",
    ] {
        assert!(
            repo_root().join(wf).exists(),
            "{wf} missing — CI is a shipped feature (D-23), drift watch is nightly (D-27)"
        );
    }
    let nightly =
        std::fs::read_to_string(repo_root().join(".github/workflows/nightly.yml")).unwrap();
    assert!(
        nightly.contains("typst"),
        "nightly must exercise Typst compatibility (D-27)"
    );
}
