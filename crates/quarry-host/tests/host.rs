//! Tests against the real quarry-sqlite.wasm artifact under Typst-identical
//! wasmi semantics: pooling with dirty reuse, snapshot/restore, transitions.
//!
//! This is where the purity invariant (C-4) is enforced: an envelope must be
//! a function of (open args, request) alone — never of which instance served
//! the call or what ran on it before.

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
            let status = Command::new("python3")
                .arg(repo_root().join("tools/gen_fixtures.py"))
                .status()
                .expect("python3 required");
            assert!(status.success());
        }
    });
}

fn fixture(name: &str) -> Vec<u8> {
    ensure_fixtures();
    std::fs::read(repo_root().join("tests/fixtures/generated").join(name)).unwrap()
}

fn plugin() -> Plugin {
    Plugin::load_shipped().expect("run tools/build_wasm.sh first")
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

fn meta(seed: i64) -> Vec<u8> {
    encode(&Value::Map(vec![
        (Value::Text("now".into()), Value::from(NOW_US)),
        (
            Value::Text("clock-origin".into()),
            Value::Text("explicit".into()),
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

fn open_basic(p: &Plugin) -> Plugin {
    let db = fixture("basic.sqlite");
    p.transition("open_1", &[&meta(42), &db]).expect("open")
}

#[test]
fn capabilities_under_wasmi() {
    let p = plugin();
    let caps = decode(&p.call("capabilities", &[]).unwrap());
    assert_eq!(field(&caps, "engine-version").as_text(), Some("3.53.4"));
    assert_eq!(
        field(&caps, "engine-version"),
        field(&caps, "pinned-engine-version"),
        "runtime engine differs from pin (D-24)"
    );
}

#[test]
fn transition_open_and_query() {
    let p = plugin();
    let db = open_basic(&p);
    let env = decode(&db.query_ok("SELECT count(*) AS n FROM orders"));
    assert_eq!(field(&env, "ok"), &Value::Bool(true));
    let rows = field(&env, "rows").as_array().unwrap();
    assert_eq!(field(&rows[0], "n").as_integer().map(i128::from), Some(200));
}

trait QueryExt {
    fn query_ok(&self, sql: &str) -> Vec<u8>;
}
impl QueryExt for Plugin {
    fn query_ok(&self, sql: &str) -> Vec<u8> {
        self.call("query", &[&request(sql)]).expect("query call")
    }
}

#[test]
fn open_failure_is_hard_error_with_message() {
    let p = plugin();
    let err = match p.transition("open_1", &[&meta(0), b"garbage bytes"]) {
        Ok(_) => panic!("garbage must not open"),
        Err(e) => e,
    };
    assert!(err.contains("quarry:"), "got: {err}");
    assert!(
        err.contains("truncated") || err.contains("not a SQLite"),
        "got: {err}"
    );
}

#[test]
fn sql_errors_are_envelopes_not_traps() {
    let p = plugin();
    let db = open_basic(&p);
    let env = decode(&db.query_ok("SELECT * FROM custmers"));
    assert_eq!(field(&env, "ok"), &Value::Bool(false));
    assert_eq!(
        field(field(&env, "error"), "hint").as_text(),
        Some("did you mean \"customers\"?")
    );
}

/// The C-4 invariant test: for a battery of stateful queries, the envelope
/// from a dirty pooled instance must equal the envelope from a fresh instance
/// restored from the snapshot.
#[test]
fn purity_fresh_vs_dirty() {
    let p = plugin();
    let db = open_basic(&p);

    let stateful = [
        "SELECT random() AS r, random() AS r2",
        "SELECT randomblob(8) AS b",
        "CREATE TEMP TABLE t1 AS SELECT * FROM orders WHERE amount > 500; SELECT count(*) AS n FROM t1",
        "SELECT last_insert_rowid() AS lid",
        "SELECT date('now') AS d, datetime('now', '+1 day') AS d2",
        "SELECT count(*) AS n FROM sqlite_temp_schema",
        "SELECT region, sum(amount) AS s FROM orders GROUP BY region ORDER BY random()",
        "PRAGMA database_list",
    ];

    // Pass 1: run everything twice on the same (dirtying) pool.
    let dirty: Vec<Vec<u8>> = stateful.iter().map(|sql| db.query_ok(sql)).collect();
    let dirty2: Vec<Vec<u8>> = stateful.iter().map(|sql| db.query_ok(sql)).collect();
    assert_eq!(
        dirty, dirty2,
        "same pool, repeated: envelopes must not drift"
    );

    // Pass 2: force a fresh restore from the snapshot before every query.
    for (i, sql) in stateful.iter().enumerate() {
        db.clear_pool();
        let fresh = db.query_ok(sql);
        assert_eq!(
            fresh, dirty[i],
            "fresh-from-snapshot vs dirty-pool envelope mismatch for: {sql}"
        );
    }
}

#[test]
fn interleaving_does_not_change_results() {
    let p = plugin();
    let db = open_basic(&p);
    let probe_sql = "SELECT random() AS r, (SELECT count(*) FROM orders) AS n";
    let baseline = db.query_ok(probe_sql);
    // interleave with queries that mutate engine-internal state
    db.query_ok("CREATE TEMP TABLE x AS SELECT 1 AS one; SELECT * FROM x");
    db.query_ok("SELECT randomblob(1000) AS blob");
    db.query_ok("SELECT * FROM orders ORDER BY random() LIMIT 5");
    let after = db.query_ok(probe_sql);
    assert_eq!(baseline, after);
}

#[test]
fn two_handles_are_independent() {
    let p = plugin();
    let db_a = p
        .transition("open_1", &[&meta(1), &fixture("basic.sqlite")])
        .unwrap();
    let db_b = p
        .transition("open_1", &[&meta(2), &fixture("basic.sqlite")])
        .unwrap();
    let ra = decode(&db_a.query_ok("SELECT random() AS r"));
    let rb = decode(&db_b.query_ok("SELECT random() AS r"));
    assert_ne!(
        field(&field(&ra, "rows").as_array().unwrap()[0], "r"),
        field(&field(&rb, "rows").as_array().unwrap()[0], "r"),
        "different seeds must give different streams"
    );
    // and each keeps working after the other ran
    let ra2 = decode(&db_a.query_ok("SELECT random() AS r"));
    assert_eq!(
        field(&field(&ra, "rows").as_array().unwrap()[0], "r"),
        field(&field(&ra2, "rows").as_array().unwrap()[0], "r"),
    );
}

#[test]
fn wasm_matches_native_engine_byte_for_byte() {
    // The mini-differential: the full corpus lives in quarry-difftest; this
    // catches gross wasm/native divergence early.
    let p = plugin();
    let db = open_basic(&p);
    let native = quarry_engine::Connection::open(&meta(42), &[&fixture("basic.sqlite")]).unwrap();
    for sql in [
        "SELECT count(*) AS n, sum(amount) AS s, avg(price) AS a FROM orders",
        "SELECT sqrt(2.0) AS s, pow(2.0, 10.0) AS p",
        "SELECT i FROM types_test ORDER BY id",
        "SELECT date('now', '+3 months') AS d",
        "SELECT region, group_concat(id) AS ids FROM orders GROUP BY region",
        "SELECT b FROM types_test WHERE id = 3",
    ] {
        let wasm_env = db.query_ok(sql);
        let native_env = native.query(&request(sql));
        assert_eq!(wasm_env, native_env, "wasm vs native divergence for: {sql}");
    }
}

#[test]
fn snapshot_size_is_bounded() {
    let p = plugin();
    let db_bytes = fixture("basic.sqlite");
    let db = p.transition("open_1", &[&meta(0), &db_bytes]).unwrap();
    let snap = db.snapshot_bytes().expect("transition must snapshot");
    // The snapshot holds the db copy + sqlite heap; it must stay within a
    // small multiple of the database size (plus a fixed engine overhead).
    let ceiling = db_bytes.len() * 3 + 8 * 1024 * 1024;
    assert!(
        snap < ceiling,
        "snapshot {snap} bytes exceeds expected ceiling {ceiling}"
    );
}

#[test]
fn base_module_helpers_work() {
    let p = plugin();
    // probe
    let probe = decode(&p.call("probe", &[&fixture("basic.sqlite")]).unwrap());
    assert_eq!(field(&probe, "ok"), &Value::Bool(true));
    assert_eq!(
        field(&probe, "page-size").as_integer().map(i128::from),
        Some(4096)
    );
    // cache_key: stable, filename-safe
    let req = request("SELECT 1");
    let k1 = decode(&p.call("cache_key", &[&req]).unwrap());
    let k2 = decode(&p.call("cache_key", &[&req]).unwrap());
    assert_eq!(k1, k2);
    assert!(k1.as_text().unwrap().starts_with("b3-"));
    // normalize_sql
    let n = decode(&p.call("normalize_sql", &[b"SELECT  1 -- c\n"]).unwrap());
    assert_eq!(n.as_text(), Some("SELECT 1"));
    // querying the base module without opening is a catchable envelope
    let env = decode(&p.call("query", &[&request("SELECT 1")]).unwrap());
    assert_eq!(field(&env, "ok"), &Value::Bool(false));
}
