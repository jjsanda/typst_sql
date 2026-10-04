//! F-28 / R-07: arbitrary bytes as a database file. The parser must reject or
//! serve them — never crash. (The incumbent's qsort stack overflow is exactly
//! this bug class.)
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let meta = {
        let v = ciborium::Value::Map(vec![(
            ciborium::Value::Text("seed".into()),
            ciborium::Value::from(0),
        )]);
        let mut out = Vec::new();
        ciborium::into_writer(&v, &mut out).unwrap();
        out
    };
    if let Ok(conn) = quarry_engine::Connection::open(&meta, &[data]) {
        let req = {
            let v = ciborium::Value::Map(vec![(
                ciborium::Value::Text("sql".into()),
                ciborium::Value::Text("SELECT * FROM sqlite_schema".into()),
            )]);
            let mut out = Vec::new();
            ciborium::into_writer(&v, &mut out).unwrap();
            out
        };
        let _ = conn.query(&req);
    }
});
