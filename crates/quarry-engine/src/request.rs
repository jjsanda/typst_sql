//! Request and open-metadata parsing (the CBOR the Typst layer / CLI sends).

use crate::error::{codes, QuarryError};
use ciborium::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum MultiStatement {
    /// Execute all statements; return the last result set produced (default).
    Last,
    /// Execute all statements; return every result set.
    All,
    /// Error if more than one statement is present.
    Reject,
}

#[derive(Debug, Clone)]
pub struct RequestOptions {
    pub max_rows: Option<u64>,
    pub max_bytes: Option<u64>,
    pub multi_statement: MultiStatement,
    pub positional_rows: bool,
    pub row_offset: u64,
    pub row_limit: Option<u64>,
}

impl Default for RequestOptions {
    fn default() -> Self {
        RequestOptions {
            max_rows: Some(100_000),
            max_bytes: Some(64 * 1024 * 1024),
            multi_statement: MultiStatement::Last,
            positional_rows: false,
            row_offset: 0,
            row_limit: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Params {
    None,
    Named(Vec<(String, Value)>),
    Positional(Vec<Value>),
}

#[derive(Debug, Clone)]
pub struct Request {
    pub sql: String,
    pub params: Params,
    pub options: RequestOptions,
    /// Source (attached schema) name used for stats/diagnostics.
    pub source: Option<String>,
}

fn get<'a>(map: &'a [(Value, Value)], key: &str) -> Option<&'a Value> {
    map.iter()
        .find(|(k, _)| k.as_text() == Some(key))
        .map(|(_, v)| v)
}

fn as_u64(v: &Value) -> Option<u64> {
    v.as_integer()
        .and_then(|i| u64::try_from(i128::from(i)).ok())
}

fn as_i64(v: &Value) -> Option<i64> {
    v.as_integer()
        .and_then(|i| i64::try_from(i128::from(i)).ok())
}

fn protocol_err(msg: &str) -> QuarryError {
    QuarryError::new(codes::PROTOCOL, msg)
}

impl Request {
    pub fn parse(bytes: &[u8]) -> Result<Request, QuarryError> {
        let value: Value = ciborium::from_reader(bytes)
            .map_err(|e| protocol_err(&format!("request is not valid CBOR: {e}")))?;
        let map = value
            .as_map()
            .ok_or_else(|| protocol_err("request must be a CBOR map"))?;

        let sql = match get(map, "sql") {
            Some(Value::Text(s)) => s.clone(),
            Some(Value::Array(parts)) => {
                let mut pieces = Vec::with_capacity(parts.len());
                for p in parts {
                    match p {
                        Value::Text(s) => pieces.push(s.clone()),
                        _ => return Err(protocol_err("request.sql array must contain strings")),
                    }
                }
                pieces.join(";\n")
            }
            _ => {
                return Err(protocol_err(
                    "request.sql must be a string or array of strings",
                ))
            }
        };

        let params = match get(map, "params") {
            None | Some(Value::Null) => Params::None,
            Some(Value::Map(named)) => {
                let mut out = Vec::with_capacity(named.len());
                for (k, v) in named {
                    let name = k
                        .as_text()
                        .ok_or_else(|| protocol_err("parameter names must be strings"))?;
                    out.push((name.to_string(), v.clone()));
                }
                Params::Named(out)
            }
            Some(Value::Array(pos)) => Params::Positional(pos.clone()),
            Some(_) => return Err(protocol_err("request.params must be a map or array")),
        };

        let mut options = RequestOptions::default();
        if let Some(Value::Map(opts)) = get(map, "options") {
            if let Some(v) = get(opts, "max-rows") {
                options.max_rows = if v.is_null() {
                    None
                } else {
                    Some(as_u64(v).ok_or_else(|| {
                        protocol_err("options.max-rows must be a non-negative integer")
                    })?)
                };
            }
            if let Some(v) = get(opts, "max-bytes") {
                options.max_bytes = if v.is_null() {
                    None
                } else {
                    Some(as_u64(v).ok_or_else(|| {
                        protocol_err("options.max-bytes must be a non-negative integer")
                    })?)
                };
            }
            if let Some(v) = get(opts, "multi-statement") {
                options.multi_statement = match v.as_text() {
                    Some("last") => MultiStatement::Last,
                    Some("all") => MultiStatement::All,
                    Some("reject") => MultiStatement::Reject,
                    _ => {
                        return Err(protocol_err(
                            "options.multi-statement must be \"last\", \"all\" or \"reject\"",
                        ))
                    }
                };
            }
            if let Some(v) = get(opts, "positional-rows") {
                options.positional_rows = v
                    .as_bool()
                    .ok_or_else(|| protocol_err("options.positional-rows must be a bool"))?;
            }
            if let Some(v) = get(opts, "row-offset") {
                options.row_offset = as_u64(v).ok_or_else(|| {
                    protocol_err("options.row-offset must be a non-negative integer")
                })?;
            }
            if let Some(v) = get(opts, "row-limit") {
                options.row_limit = if v.is_null() {
                    None
                } else {
                    Some(as_u64(v).ok_or_else(|| {
                        protocol_err("options.row-limit must be a non-negative integer")
                    })?)
                };
            }
        }

        let source = get(map, "source")
            .and_then(|v| v.as_text())
            .map(String::from);

        Ok(Request {
            sql,
            params,
            options,
            source,
        })
    }
}

#[derive(Debug, Clone)]
pub struct OpenMeta {
    /// Injected clock, Unix microseconds.
    pub now_us: Option<i64>,
    /// Where the clock came from (explicit | input | today | none).
    pub clock_origin: String,
    /// ISO-8601 rendering of the clock, produced by the caller (kept opaque
    /// here so the engine never needs a date library).
    pub clock_display: Option<String>,
    pub seed: i64,
    /// Schema name per source blob, in blob order. Empty ⇒ single "main".
    pub sources: Vec<String>,
}

impl OpenMeta {
    pub fn parse(bytes: &[u8]) -> Result<OpenMeta, QuarryError> {
        let value: Value = ciborium::from_reader(bytes)
            .map_err(|e| protocol_err(&format!("open metadata is not valid CBOR: {e}")))?;
        let map = value
            .as_map()
            .ok_or_else(|| protocol_err("open metadata must be a CBOR map"))?;

        let now_us =
            match get(map, "now") {
                None | Some(Value::Null) => None,
                Some(v) => Some(as_i64(v).ok_or_else(|| {
                    protocol_err("meta.now must be Unix microseconds as an integer")
                })?),
            };
        let clock_origin = get(map, "clock-origin")
            .and_then(|v| v.as_text())
            .unwrap_or(if now_us.is_some() { "explicit" } else { "none" })
            .to_string();
        let clock_display = get(map, "clock-display")
            .and_then(|v| v.as_text())
            .map(String::from);
        let seed = match get(map, "seed") {
            None | Some(Value::Null) => 0,
            Some(v) => as_i64(v).ok_or_else(|| protocol_err("meta.seed must be an integer"))?,
        };
        let sources = match get(map, "sources") {
            None => Vec::new(),
            Some(Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    out.push(
                        it.as_text()
                            .ok_or_else(|| protocol_err("meta.sources must be strings"))?
                            .to_string(),
                    );
                }
                out
            }
            Some(_) => return Err(protocol_err("meta.sources must be an array")),
        };
        Ok(OpenMeta {
            now_us,
            clock_origin,
            clock_display,
            seed,
            sources,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(v: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        ciborium::into_writer(v, &mut out).unwrap();
        out
    }

    #[test]
    fn parses_minimal_request() {
        let v = Value::Map(vec![(
            Value::Text("sql".into()),
            Value::Text("SELECT 1".into()),
        )]);
        let r = Request::parse(&encode(&v)).unwrap();
        assert_eq!(r.sql, "SELECT 1");
        assert!(matches!(r.params, Params::None));
        assert_eq!(r.options.max_rows, Some(100_000));
        assert_eq!(r.options.multi_statement, MultiStatement::Last);
    }

    #[test]
    fn rejects_garbage() {
        let err = Request::parse(b"\xff\xff").unwrap_err();
        assert_eq!(err.code, codes::PROTOCOL);
        let v = Value::Text("just a string".into());
        assert_eq!(
            Request::parse(&encode(&v)).unwrap_err().code,
            codes::PROTOCOL
        );
    }

    #[test]
    fn parses_named_and_positional_params() {
        let named = Value::Map(vec![
            (Value::Text("sql".into()), Value::Text("SELECT :a".into())),
            (
                Value::Text("params".into()),
                Value::Map(vec![(Value::Text("a".into()), Value::from(1))]),
            ),
        ]);
        assert!(matches!(
            Request::parse(&encode(&named)).unwrap().params,
            Params::Named(_)
        ));
        let pos = Value::Map(vec![
            (Value::Text("sql".into()), Value::Text("SELECT ?".into())),
            (
                Value::Text("params".into()),
                Value::Array(vec![Value::from(1)]),
            ),
        ]);
        assert!(matches!(
            Request::parse(&encode(&pos)).unwrap().params,
            Params::Positional(_)
        ));
    }

    #[test]
    fn parses_open_meta() {
        let v = Value::Map(vec![
            (
                Value::Text("now".into()),
                Value::from(1_753_900_000_000_000i64),
            ),
            (
                Value::Text("clock-origin".into()),
                Value::Text("input".into()),
            ),
            (Value::Text("seed".into()), Value::from(42)),
            (
                Value::Text("sources".into()),
                Value::Array(vec![
                    Value::Text("sales".into()),
                    Value::Text("refs".into()),
                ]),
            ),
        ]);
        let m = OpenMeta::parse(&encode(&v)).unwrap();
        assert_eq!(m.now_us, Some(1_753_900_000_000_000));
        assert_eq!(m.seed, 42);
        assert_eq!(m.sources, vec!["sales", "refs"]);
    }
}
