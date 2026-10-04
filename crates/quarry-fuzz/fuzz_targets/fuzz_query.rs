//! Arbitrary SQL text against a fixed database: errors welcome, crashes not.
#![no_main]
use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;

static DB: OnceLock<Vec<u8>> = OnceLock::new();

fuzz_target!(|sql: &str| {
    let db = DB.get_or_init(|| {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/generated/basic.sqlite"
        );
        std::fs::read(path).expect("run tools/gen_fixtures.py first")
    });
    let meta = {
        let v = ciborium::Value::Map(vec![
            (ciborium::Value::Text("now".into()), ciborium::Value::from(1_768_867_200_000_000i64)),
            (ciborium::Value::Text("seed".into()), ciborium::Value::from(1)),
        ]);
        let mut out = Vec::new();
        ciborium::into_writer(&v, &mut out).unwrap();
        out
    };
    let conn = quarry_engine::Connection::open(&meta, &[db]).unwrap();
    let req = {
        let v = ciborium::Value::Map(vec![(
            ciborium::Value::Text("sql".into()),
            ciborium::Value::Text(sql.into()),
        )]);
        let mut out = Vec::new();
        ciborium::into_writer(&v, &mut out).unwrap();
        out
    };
    let _ = conn.query(&req);
});
