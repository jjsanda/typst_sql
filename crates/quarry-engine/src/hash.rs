//! Content-addressed cache keys (F-31).
//!
//! `cache_key` is exported by the WASM plugin *and* compiled natively into the
//! sidecar CLI, so both sides of the cache derive identical keys from identical
//! requests — the symmetry the lockfile design depends on.

use crate::normalize::canonicalize_sql;
use crate::request::{Params, Request};
use ciborium::Value;

/// Derive the cache key for a request against a named source.
///
/// The key covers: envelope version, source id, canonical SQL and the bound
/// parameters (sorted by name for order-independence). Formatting changes to
/// the SQL do not change the key; literal or parameter changes do. The source
/// *kind* is deliberately excluded — the document cannot know it, and the
/// source name is the unique handle; the kind is recorded in quarry.lock for
/// review instead.
pub fn cache_key(envelope_version: u64, source_id: &str, req: &Request) -> String {
    let canonical_sql = canonicalize_sql(&req.sql);
    let params_value = match &req.params {
        Params::None => Value::Null,
        Params::Positional(vs) => Value::Array(vs.clone()),
        Params::Named(pairs) => {
            let mut sorted = pairs.clone();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Map(
                sorted
                    .into_iter()
                    .map(|(k, v)| (Value::Text(k), v))
                    .collect(),
            )
        }
    };
    let canonical = Value::Array(vec![
        Value::from(envelope_version),
        Value::Text(source_id.to_string()),
        Value::Text(canonical_sql),
        params_value,
    ]);
    let mut bytes = Vec::new();
    ciborium::into_writer(&canonical, &mut bytes).expect("CBOR encode cannot fail on Vec");
    format!("b3-{}", blake3::hash(&bytes).to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::RequestOptions;

    fn req(sql: &str, params: Params) -> Request {
        Request {
            sql: sql.to_string(),
            params,
            options: RequestOptions::default(),
            source: None,
        }
    }

    #[test]
    fn formatting_insensitive() {
        let a = cache_key(
            1,
            "wh",
            &req("SELECT *  FROM t -- x\nWHERE a=1", Params::None),
        );
        let b = cache_key(1, "wh", &req("SELECT * FROM t WHERE a=1", Params::None));
        assert_eq!(a, b);
    }

    #[test]
    fn distinguishes_source_params_and_literals() {
        let base = cache_key(1, "wh", &req("SELECT 1", Params::None));
        assert_ne!(base, cache_key(1, "other", &req("SELECT 1", Params::None)));
        assert_ne!(
            base,
            cache_key(1, "wh", &req("SELECT 2 -- kindless", Params::None))
        );
        assert_ne!(base, cache_key(1, "wh", &req("SELECT 2", Params::None)));
        assert_ne!(
            base,
            cache_key(
                1,
                "wh",
                &req("SELECT 1", Params::Positional(vec![Value::from(1)]))
            )
        );
        assert_ne!(base, cache_key(2, "wh", &req("SELECT 1", Params::None)));
    }

    #[test]
    fn named_param_order_is_irrelevant() {
        let p1 = Params::Named(vec![
            ("a".into(), Value::from(1)),
            ("b".into(), Value::from(2)),
        ]);
        let p2 = Params::Named(vec![
            ("b".into(), Value::from(2)),
            ("a".into(), Value::from(1)),
        ]);
        assert_eq!(
            cache_key(1, "s", &req("SELECT :a + :b", p1)),
            cache_key(1, "s", &req("SELECT :a + :b", p2))
        );
    }

    #[test]
    fn key_is_filename_safe() {
        let k = cache_key(1, "s", &req("SELECT 1", Params::None));
        assert!(k.starts_with("b3-"));
        assert!(k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert_eq!(k.len(), 3 + 64);
    }
}
