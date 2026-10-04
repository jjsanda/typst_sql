//! Structured, catchable errors (F-16/F-17, fixing D-17).
//!
//! Every failure the engine can produce is represented here and serialized
//! into the error half of envelope v1. Nothing ever surfaces as a raw string
//! or an uncatchable trap; the Typst layer decides whether to panic (with a
//! rendered caret diagnostic) or hand the error to the document (`try-query`).

use ciborium::Value;

/// Stable machine-readable error codes. `tests/regression/d25` asserts every
/// one of these is reached by at least one test.
pub mod codes {
    pub const PROTOCOL: &str = "protocol-error";
    pub const SOURCE_INVALID: &str = "source-invalid";
    pub const SOURCE_UNSUPPORTED: &str = "source-unsupported";
    pub const CLOCK_REQUIRED: &str = "clock-required";
    pub const READ_ONLY: &str = "read-only-violation";
    pub const BINDING: &str = "binding-error";
    pub const SQLITE: &str = "sqlite-error";
    pub const MULTI_STATEMENT: &str = "multi-statement-rejected";
    pub const OPEN_FAILED: &str = "open-failed";
}

#[derive(Debug, Clone, Default)]
pub struct QuarryError {
    pub code: String,
    pub message: String,
    pub sqlite_code: Option<i64>,
    pub sqlite_extended: Option<i64>,
    pub sql: Option<String>,
    pub statement_index: Option<u64>,
    /// Byte offset of the offending token within `sql`, when known.
    pub offset: Option<i64>,
    /// Length of the offending token, when known.
    pub length: Option<i64>,
    /// "did you mean …?" — produced by a bounded edit-distance search over the
    /// schema's table and column names.
    pub hint: Option<String>,
    pub source: Option<String>,
}

impl QuarryError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        QuarryError {
            code: code.to_string(),
            message: message.into(),
            ..Default::default()
        }
    }

    pub fn with_sql(mut self, sql: &str, statement_index: usize) -> Self {
        self.sql = Some(sql.to_string());
        self.statement_index = Some(statement_index as u64);
        self
    }

    pub fn to_value(&self, engine: &str) -> Value {
        fn opt_i64(v: Option<i64>) -> Value {
            v.map(Value::from).unwrap_or(Value::Null)
        }
        fn opt_text(v: &Option<String>) -> Value {
            v.as_ref()
                .map(|s| Value::Text(s.clone()))
                .unwrap_or(Value::Null)
        }
        Value::Map(vec![
            (Value::Text("code".into()), Value::Text(self.code.clone())),
            (
                Value::Text("message".into()),
                Value::Text(self.message.clone()),
            ),
            (Value::Text("sqlite-code".into()), opt_i64(self.sqlite_code)),
            (
                Value::Text("sqlite-extended".into()),
                opt_i64(self.sqlite_extended),
            ),
            (Value::Text("sql".into()), opt_text(&self.sql)),
            (
                Value::Text("statement-index".into()),
                self.statement_index.map(Value::from).unwrap_or(Value::Null),
            ),
            (Value::Text("offset".into()), opt_i64(self.offset)),
            (Value::Text("length".into()), opt_i64(self.length)),
            (Value::Text("hint".into()), opt_text(&self.hint)),
            (Value::Text("source".into()), opt_text(&self.source)),
            (
                Value::Text("engine".into()),
                Value::Text(engine.to_string()),
            ),
        ])
    }
}

/// Bounded Levenshtein distance (early exit above `max`).
pub fn levenshtein_within(a: &str, b: &str, max: usize) -> Option<usize> {
    let a: Vec<char> = a.to_lowercase().chars().collect();
    let b: Vec<char> = b.to_lowercase().chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(cur[j]);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    (prev[b.len()] <= max).then_some(prev[b.len()])
}

/// Given a missing identifier and the known names, produce a "did you mean"
/// hint when a close match (edit distance ≤ 2) exists.
pub fn spelling_hint(missing: &str, known: &[String]) -> Option<String> {
    let mut best: Option<(usize, &String)> = None;
    for name in known {
        if let Some(d) = levenshtein_within(missing, name, 2) {
            if d > 0 && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, name));
            }
        }
    }
    best.map(|(_, name)| format!("did you mean \"{name}\"?"))
}

/// Locate the byte offset of an identifier inside SQL text (case-insensitive,
/// outside string literals). Used when sqlite3_error_offset() reports -1 but
/// the message names the identifier.
pub fn find_identifier_offset(sql: &str, ident: &str) -> Option<(i64, i64)> {
    if ident.is_empty() {
        return None;
    }
    let hay = sql.to_lowercase();
    let needle = ident.to_lowercase();
    let bytes = hay.as_bytes();
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match in_str {
            Some(q) => {
                if c == q {
                    in_str = None;
                }
            }
            None => {
                if c == b'\'' || c == b'"' {
                    in_str = Some(c);
                } else if hay[i..].starts_with(&needle) {
                    let before_ok =
                        i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
                    let end = i + needle.len();
                    let after_ok = end >= bytes.len()
                        || !(bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
                    if before_ok && after_ok {
                        return Some((i as i64, needle.len() as i64));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein_within("custmers", "customers", 2), Some(1));
        assert_eq!(levenshtein_within("abc", "abc", 2), Some(0));
        assert_eq!(levenshtein_within("abc", "xyzabc", 2), None);
        assert_eq!(levenshtein_within("ab", "ba", 2), Some(2));
    }

    #[test]
    fn hint_prefers_closest() {
        let known = vec!["customers".to_string(), "customs".to_string()];
        assert_eq!(
            spelling_hint("custmers", &known),
            Some("did you mean \"customers\"?".to_string())
        );
        // identical name (distance 0) is not a useful hint
        assert_eq!(spelling_hint("customers", &known[..1]), None);
    }

    #[test]
    fn identifier_offset_skips_strings() {
        let sql = "SELECT 'custmers', region FROM custmers";
        let (off, len) = find_identifier_offset(sql, "custmers").unwrap();
        assert_eq!(&sql[off as usize..(off + len) as usize], "custmers");
        assert_eq!(off, 31);
    }
}
