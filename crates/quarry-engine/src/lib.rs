//! quarry-engine: the core SQLite engine shared by the WASM plugin and the
//! native sidecar/tests.
//!
//! Design constraints this crate answers to (see market_analysis/):
//! - **Correct** (F-01…F-05): real libc/libm via wasi-libc, injected clock and
//!   seed, 64-bit integers end to end, BLOBs as bytes, defined multi-statement
//!   semantics.
//! - **Pure**: identical inputs produce byte-identical CBOR envelopes,
//!   regardless of call order or instance reuse (Typst memoization contract).
//! - **Catchable**: every error is data in an envelope, never a trap (D-17).

// QuarryError is deliberately rich (sql text, offsets, hints); it crosses the
// boundary once per failed call, so its size is irrelevant to performance.
#![allow(clippy::result_large_err)]

pub mod bind;
pub mod capabilities;
pub mod conn;
pub mod envelope;
pub mod error;
pub mod exec;
pub mod ffi;
pub mod hash;
pub mod normalize;
pub mod probe;
pub mod request;
pub mod schema;
pub mod value;
pub mod vfs;

pub use conn::Connection;
pub use request::{OpenMeta, Params, Request};

use ciborium::Value;

/// The engine description used in envelopes when no connection exists yet.
pub fn engine_description() -> String {
    conn::engine_description()
}

/// `capabilities()` plugin export body.
pub fn capabilities_bytes() -> Vec<u8> {
    let v = capabilities::capabilities_value(&conn::sqlite_version());
    envelope::value_envelope(&v)
}

/// `probe(blob)` plugin export body: header inspection without opening.
pub fn probe_bytes(blob: &[u8]) -> Vec<u8> {
    envelope::value_envelope(&probe::probe_value(blob))
}

/// `cache_key(request)` plugin export body. The request must carry `source`
/// (the source id); the key deliberately excludes the source *kind* (see
/// hash::cache_key).
pub fn cache_key_bytes(request_bytes: &[u8]) -> Result<Vec<u8>, error::QuarryError> {
    let req = Request::parse(request_bytes)?;
    let source = req.source.clone().unwrap_or_else(|| "main".to_string());
    let key = hash::cache_key(envelope::ENVELOPE_VERSION, &source, &req);
    Ok(envelope::value_envelope(&Value::Text(key)))
}

/// `normalize_sql(sql)` plugin export body (canonical text, cache-key form).
pub fn normalize_sql_bytes(sql: &str) -> Vec<u8> {
    envelope::value_envelope(&Value::Text(normalize::canonicalize_sql(sql)))
}

impl Connection {
    /// Execute a query request, returning an envelope (never failing).
    pub fn query(&self, request_bytes: &[u8]) -> Vec<u8> {
        exec::run_query(self, request_bytes)
    }

    /// Describe the statements in a request without executing them.
    pub fn describe(&self, request_bytes: &[u8]) -> Vec<u8> {
        let result =
            Request::parse(request_bytes).and_then(|req| schema::describe_value(self, &req.sql));
        match result {
            Ok(v) => envelope::value_envelope(&v),
            Err(e) => envelope::error_envelope(&e, &self.engine_desc()),
        }
    }

    /// Full schema introspection.
    pub fn schema(&self, row_counts: bool) -> Vec<u8> {
        match schema::schema_value(self, row_counts) {
            Ok(v) => envelope::value_envelope(&v),
            Err(e) => envelope::error_envelope(&e, &self.engine_desc()),
        }
    }

    pub fn engine_desc(&self) -> String {
        conn::engine_description()
    }

    pub fn source_names(&self) -> &[String] {
        &self.sources
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ciborium::Value;

    pub fn encode(v: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        ciborium::into_writer(v, &mut out).unwrap();
        out
    }

    pub fn meta(now_us: Option<i64>, seed: i64, sources: &[&str]) -> Vec<u8> {
        let mut fields = vec![
            (
                Value::Text("now".into()),
                now_us.map(Value::from).unwrap_or(Value::Null),
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

    /// Build a tiny valid database natively for tests: engine can't create
    /// files, so tests synthesize one through a writable scratch connection…
    /// which quarry deliberately cannot do. Instead, fixtures are created by
    /// the fixture builder in tests/ (see quarry-difftest). Here we only test
    /// pure-Rust pieces plus error paths that need no real database.
    #[test]
    fn open_rejects_garbage_sources() {
        let m = meta(None, 0, &[]);
        let err = Connection::open(&m, &[b"not a database"]).unwrap_err();
        assert_eq!(err.code, error::codes::SOURCE_INVALID);
        assert!(err.message.contains("truncated") || err.message.contains("not a SQLite"));
    }

    #[test]
    fn open_requires_matching_source_names() {
        let m = meta(None, 0, &["a", "b"]);
        let err = Connection::open(&m, &[&[0u8; 100][..]]).unwrap_err();
        assert_eq!(err.code, error::codes::PROTOCOL);
    }

    #[test]
    fn capabilities_and_probe_are_valid_cbor() {
        envelope::assert_tag_free(&capabilities_bytes());
        envelope::assert_tag_free(&probe_bytes(b"garbage"));
    }
}
