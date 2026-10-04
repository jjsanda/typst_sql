//! The request model shared between discovery and execution, and the
//! JSON→CBOR bridge with key verification.
//!
//! Discovery yields requests as JSON (via `typst eval`/`typst query`); the
//! cache key must nevertheless equal what the *document* computed through the
//! plugin's CBOR path. After conversion the CLI recomputes the key with the
//! same native code and refuses on mismatch — a conversion divergence must be
//! loud, silently caching under the wrong key would be a stale-data machine.

use anyhow::{bail, Result};
use ciborium::Value;
use quarry_engine::request::{Params, Request as EngineRequest, RequestOptions};

#[derive(Debug, Clone)]
pub struct DiscoveredRequest {
    pub source: String,
    pub sql: String,
    pub params: Value, // Null | Map | Array (CBOR form, @dt/@dur/@json shapes)
    /// The key the document computed (present when discovered from a doc).
    pub declared_key: Option<String>,
}

pub fn json_to_cbor(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else {
                Value::from(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        serde_json::Value::String(s) => Value::Text(s.clone()),
        serde_json::Value::Array(items) => Value::Array(items.iter().map(json_to_cbor).collect()),
        serde_json::Value::Object(map) => Value::Map(
            map.iter()
                .map(|(k, v)| (Value::Text(k.clone()), json_to_cbor(v)))
                .collect(),
        ),
    }
}

pub fn toml_to_cbor(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::Text(s.clone()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => Value::from(*f),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::Map(vec![(
            Value::Text("@dt".into()),
            Value::Text(d.to_string()),
        )]),
        toml::Value::Array(items) => Value::Array(items.iter().map(toml_to_cbor).collect()),
        toml::Value::Table(map) => Value::Map(
            map.iter()
                .map(|(k, v)| (Value::Text(k.clone()), toml_to_cbor(v)))
                .collect(),
        ),
    }
}

impl DiscoveredRequest {
    pub fn to_engine_request(&self) -> Result<EngineRequest> {
        let params = match &self.params {
            Value::Null => Params::None,
            Value::Map(pairs) => {
                let mut named = Vec::with_capacity(pairs.len());
                for (k, v) in pairs {
                    let Some(name) = k.as_text() else {
                        bail!(
                            "request against {:?}: parameter names must be strings",
                            self.source
                        )
                    };
                    named.push((name.to_string(), v.clone()));
                }
                Params::Named(named)
            }
            Value::Array(items) => Params::Positional(items.clone()),
            other => bail!(
                "request against {:?}: unsupported params shape {other:?}",
                self.source
            ),
        };
        Ok(EngineRequest {
            sql: self.sql.clone(),
            params,
            options: RequestOptions::default(),
            source: Some(self.source.clone()),
        })
    }

    /// Compute the cache key with the exact code the plugin exports, and
    /// verify it against the document's key when one was carried.
    pub fn key(&self) -> Result<String> {
        let engine_request = self.to_engine_request()?;
        let key = quarry_engine::hash::cache_key(
            quarry_engine::envelope::ENVELOPE_VERSION,
            &self.source,
            &engine_request,
        );
        if let Some(declared) = &self.declared_key {
            if declared != &key {
                bail!(
                    "cache-key mismatch for a query against {:?}:\n  document computed {declared}\n  CLI computed      {key}\n\
                     This means the discovery JSON did not round-trip the request exactly \
                     (a float/int or byte-value parameter?). Declare the query in quarry.toml instead.",
                    self.source
                );
            }
        }
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_matches_document_side_for_plain_requests() {
        // The document computes keys through the plugin's CBOR encoding of
        // (sql, params, source); the CLI must land on the identical key after
        // the JSON round-trip.
        let json: serde_json::Value =
            serde_json::from_str(r#"{"cohort": "2026-enterprise", "min": 42}"#).unwrap();
        let req = DiscoveredRequest {
            source: "warehouse".into(),
            sql: "SELECT * FROM metrics WHERE cohort = :cohort AND n > :min".into(),
            params: json_to_cbor(&json),
            declared_key: None,
        };
        let key = req.key().unwrap();
        assert!(key.starts_with("b3-"));
        // identical request but via a different construction path
        let req2 = DiscoveredRequest {
            params: Value::Map(vec![
                (Value::Text("min".into()), Value::from(42)),
                (
                    Value::Text("cohort".into()),
                    Value::Text("2026-enterprise".into()),
                ),
            ]),
            declared_key: Some(key.clone()),
            ..req.clone()
        };
        assert_eq!(
            req2.key().unwrap(),
            key,
            "param order and construction path must not matter"
        );
    }

    #[test]
    fn key_mismatch_is_fatal() {
        let req = DiscoveredRequest {
            source: "wh".into(),
            sql: "SELECT 1".into(),
            params: Value::Null,
            declared_key: Some("b3-definitely-wrong".into()),
        };
        assert!(req
            .key()
            .unwrap_err()
            .to_string()
            .contains("cache-key mismatch"));
    }
}
