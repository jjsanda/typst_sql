//! Result envelope v1 (F-08/F-15). Normative schema in docs/envelope-v1.md.
//!
//! Encoding rules, non-negotiable:
//! - definite-length maps and arrays only;
//! - **no CBOR tags anywhere** (Typst's `cbor()` rejects them);
//! - integers as CBOR major 0/1, never widened to float (D-02);
//! - BLOBs as byte strings (D-06);
//! - deterministic field order, so identical results are byte-identical.

use crate::error::QuarryError;
use ciborium::Value;

pub const ENVELOPE_VERSION: u64 = 1;

#[derive(Debug, Clone)]
pub struct ColumnMeta {
    pub name: String,
    /// Logical type: integer | real | text | blob | null | numeric | boolean |
    /// date | datetime | time | json | any
    pub logical_type: &'static str,
    pub decltype: Option<String>,
    /// Observational: no NULL was seen in this result set.
    pub nullable: bool,
    pub table: Option<String>,
    pub origin: Option<String>,
}

impl ColumnMeta {
    fn to_value(&self) -> Value {
        fn opt_text(v: &Option<String>) -> Value {
            v.as_ref()
                .map(|s| Value::Text(s.clone()))
                .unwrap_or(Value::Null)
        }
        Value::Map(vec![
            (Value::Text("name".into()), Value::Text(self.name.clone())),
            (
                Value::Text("type".into()),
                Value::Text(self.logical_type.into()),
            ),
            (Value::Text("decltype".into()), opt_text(&self.decltype)),
            (Value::Text("nullable".into()), Value::Bool(self.nullable)),
            (Value::Text("table".into()), opt_text(&self.table)),
            (Value::Text("origin".into()), opt_text(&self.origin)),
        ])
    }
}

#[derive(Debug, Clone)]
pub struct Warning {
    pub code: String,
    pub message: String,
}

impl Warning {
    fn to_value(&self) -> Value {
        Value::Map(vec![
            (Value::Text("code".into()), Value::Text(self.code.clone())),
            (
                Value::Text("message".into()),
                Value::Text(self.message.clone()),
            ),
        ])
    }
}

#[derive(Debug, Clone)]
pub struct ClockInfo {
    /// ISO-8601 rendering of the injected clock, or none.
    pub value: Option<String>,
    /// Where the clock came from: explicit | input | today | none.
    pub origin: String,
}

#[derive(Debug, Clone)]
pub struct Stats {
    pub row_count: u64,
    pub truncated: bool,
    pub source: String,
    pub statements: u64,
    pub clock: ClockInfo,
    pub seed: Option<i64>,
    pub engine: String,
    /// True for sidecar-discovery placeholders, absent otherwise.
    pub placeholder: bool,
}

impl Stats {
    fn to_value(&self) -> Value {
        let mut fields = vec![
            (Value::Text("row-count".into()), Value::from(self.row_count)),
            (Value::Text("truncated".into()), Value::Bool(self.truncated)),
            (
                Value::Text("source".into()),
                Value::Text(self.source.clone()),
            ),
            (
                Value::Text("statements".into()),
                Value::from(self.statements),
            ),
            (
                Value::Text("clock".into()),
                Value::Map(vec![
                    (
                        Value::Text("value".into()),
                        self.clock
                            .value
                            .as_ref()
                            .map(|s| Value::Text(s.clone()))
                            .unwrap_or(Value::Null),
                    ),
                    (
                        Value::Text("origin".into()),
                        Value::Text(self.clock.origin.clone()),
                    ),
                ]),
            ),
            (
                Value::Text("seed".into()),
                self.seed.map(Value::from).unwrap_or(Value::Null),
            ),
            (
                Value::Text("engine".into()),
                Value::Text(self.engine.clone()),
            ),
        ];
        if self.placeholder {
            fields.push((Value::Text("placeholder".into()), Value::Bool(true)));
        }
        Value::Map(fields)
    }
}

/// A successful result set: columns + rows (+ per-request stats).
pub struct ResultSet {
    pub columns: Vec<ColumnMeta>,
    /// Row values in column order; wrapped as dicts or arrays at encode time.
    pub rows: Vec<Vec<Value>>,
    pub truncated: bool,
}

impl ResultSet {
    pub fn rows_value(&self, positional: bool) -> Value {
        if positional {
            Value::Array(self.rows.iter().cloned().map(Value::Array).collect())
        } else {
            Value::Array(
                self.rows
                    .iter()
                    .map(|row| {
                        Value::Map(
                            self.columns
                                .iter()
                                .zip(row.iter())
                                .map(|(c, v)| (Value::Text(c.name.clone()), v.clone()))
                                .collect(),
                        )
                    })
                    .collect(),
            )
        }
    }
}

fn serialize(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(value, &mut out).expect("CBOR serialization cannot fail on Vec");
    out
}

/// Encode a success envelope for a single result set.
pub fn ok_envelope(
    result: &ResultSet,
    stats: &Stats,
    warnings: &[Warning],
    positional: bool,
) -> Vec<u8> {
    let value = Value::Map(vec![
        (Value::Text("version".into()), Value::from(ENVELOPE_VERSION)),
        (Value::Text("ok".into()), Value::Bool(true)),
        (
            Value::Text("columns".into()),
            Value::Array(result.columns.iter().map(|c| c.to_value()).collect()),
        ),
        (Value::Text("rows".into()), result.rows_value(positional)),
        (Value::Text("stats".into()), stats.to_value()),
        (
            Value::Text("warnings".into()),
            Value::Array(warnings.iter().map(|w| w.to_value()).collect()),
        ),
    ]);
    serialize(&value)
}

/// Encode a success envelope carrying every statement's result set
/// (`multi-statement: "all"`).
pub fn ok_all_envelope(
    results: &[ResultSet],
    stats: &Stats,
    warnings: &[Warning],
    positional: bool,
) -> Vec<u8> {
    let sets: Vec<Value> = results
        .iter()
        .map(|r| {
            Value::Map(vec![
                (
                    Value::Text("columns".into()),
                    Value::Array(r.columns.iter().map(|c| c.to_value()).collect()),
                ),
                (Value::Text("rows".into()), r.rows_value(positional)),
                (
                    Value::Text("row-count".into()),
                    Value::from(r.rows.len() as u64),
                ),
                (Value::Text("truncated".into()), Value::Bool(r.truncated)),
            ])
        })
        .collect();
    let value = Value::Map(vec![
        (Value::Text("version".into()), Value::from(ENVELOPE_VERSION)),
        (Value::Text("ok".into()), Value::Bool(true)),
        (Value::Text("results".into()), Value::Array(sets)),
        (Value::Text("stats".into()), stats.to_value()),
        (
            Value::Text("warnings".into()),
            Value::Array(warnings.iter().map(|w| w.to_value()).collect()),
        ),
    ]);
    serialize(&value)
}

/// Encode an error envelope. Note: still exit code 0 at the plugin boundary —
/// error-ness is data, which is what makes it catchable (D-17/D-25).
pub fn error_envelope(error: &QuarryError, engine: &str) -> Vec<u8> {
    let value = Value::Map(vec![
        (Value::Text("version".into()), Value::from(ENVELOPE_VERSION)),
        (Value::Text("ok".into()), Value::Bool(false)),
        (Value::Text("error".into()), error.to_value(engine)),
    ]);
    serialize(&value)
}

/// Encode an arbitrary prebuilt value (capabilities, schema, probe).
pub fn value_envelope(value: &Value) -> Vec<u8> {
    serialize(value)
}

/// Walk a CBOR byte string and assert it contains no tags — a stray tag is an
/// uncatchable error in Typst's `cbor()`. Used by unit tests on every encoder.
pub fn assert_tag_free(bytes: &[u8]) {
    fn walk(v: &Value) {
        match v {
            Value::Tag(t, _) => panic!("CBOR tag {t} found in envelope — Typst rejects tags"),
            Value::Array(items) => items.iter().for_each(walk),
            Value::Map(pairs) => pairs.iter().for_each(|(k, val)| {
                walk(k);
                walk(val);
            }),
            _ => {}
        }
    }
    let v: Value = ciborium::from_reader(bytes).expect("envelope must be valid CBOR");
    walk(&v);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stats() -> Stats {
        Stats {
            row_count: 1,
            truncated: false,
            source: "main".into(),
            statements: 1,
            clock: ClockInfo {
                value: None,
                origin: "none".into(),
            },
            seed: Some(0),
            engine: "sqlite test".into(),
            placeholder: false,
        }
    }

    #[test]
    fn i64_boundaries_stay_integers() {
        let rs = ResultSet {
            columns: vec![ColumnMeta {
                name: "v".into(),
                logical_type: "integer",
                decltype: None,
                nullable: false,
                table: None,
                origin: None,
            }],
            rows: vec![vec![Value::from(i64::MAX)], vec![Value::from(i64::MIN)]],
            truncated: false,
        };
        let bytes = ok_envelope(&rs, &sample_stats(), &[], false);
        assert_tag_free(&bytes);
        let v: Value = ciborium::from_reader(bytes.as_slice()).unwrap();
        let rows = v
            .as_map()
            .unwrap()
            .iter()
            .find(|(k, _)| k.as_text() == Some("rows"))
            .unwrap()
            .1
            .as_array()
            .unwrap()
            .clone();
        let first = rows[0].as_map().unwrap()[0].1.as_integer().unwrap();
        let second = rows[1].as_map().unwrap()[0].1.as_integer().unwrap();
        assert_eq!(i128::from(first), i64::MAX as i128);
        assert_eq!(i128::from(second), i64::MIN as i128);
    }

    #[test]
    fn envelopes_are_tag_free_and_deterministic() {
        let rs = ResultSet {
            columns: vec![],
            rows: vec![],
            truncated: false,
        };
        let a = ok_envelope(&rs, &sample_stats(), &[], false);
        let b = ok_envelope(&rs, &sample_stats(), &[], false);
        assert_eq!(a, b);
        assert_tag_free(&a);
        let err = QuarryError::new(crate::error::codes::SQLITE, "no such table: custmers");
        let e = error_envelope(&err, "sqlite test");
        assert_tag_free(&e);
    }
}
