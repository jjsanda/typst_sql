//! Backend capability declaration (the backend contract's `capabilities()`).
//!
//! The document layer uses this to fail early with a clear message instead of
//! failing deep inside the engine, and tests assert every claim here against
//! observed behaviour (d19/d24).

use ciborium::Value;

pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Baked in by build.rs from vendor/sqlite/VERSION; d24 asserts it equals
/// sqlite3_libversion() at runtime and the version named in the docs.
pub const PINNED_SQLITE_VERSION: &str = env!("QUARRY_SQLITE_VERSION");

pub fn capabilities_value(engine_version: &str) -> Value {
    fn b(v: bool) -> Value {
        Value::Bool(v)
    }
    Value::Map(vec![
        (
            Value::Text("name".into()),
            Value::Text("quarry-sqlite".into()),
        ),
        (
            Value::Text("version".into()),
            Value::Text(CRATE_VERSION.into()),
        ),
        (
            Value::Text("engine-version".into()),
            Value::Text(engine_version.to_string()),
        ),
        (
            Value::Text("pinned-engine-version".into()),
            Value::Text(PINNED_SQLITE_VERSION.into()),
        ),
        (
            Value::Text("sql-dialect".into()),
            Value::Text("sqlite".into()),
        ),
        (
            Value::Text("supports".into()),
            Value::Map(vec![
                (Value::Text("window-functions".into()), b(true)),
                (Value::Text("ctes".into()), b(true)),
                (Value::Text("full-text".into()), b(true)),
                (Value::Text("json-functions".into()), b(true)),
                (Value::Text("math-functions".into()), b(true)),
                (Value::Text("rtree".into()), b(true)),
                (Value::Text("multi-source".into()), b(true)),
                (Value::Text("parameters".into()), b(true)),
                (Value::Text("blobs".into()), b(true)),
                (Value::Text("streaming".into()), b(true)),
                (Value::Text("load-extension".into()), b(false)),
                (Value::Text("localtime".into()), b(false)),
                (Value::Text("writes".into()), b(false)),
            ]),
        ),
        (
            Value::Text("limits".into()),
            Value::Map(vec![
                (Value::Text("max-source-bytes".into()), Value::Null),
                (Value::Text("max-result-bytes".into()), Value::Null),
                (Value::Text("max-params".into()), Value::from(32766u64)),
                (Value::Text("max-attached".into()), Value::from(32u64)),
            ]),
        ),
    ])
}
