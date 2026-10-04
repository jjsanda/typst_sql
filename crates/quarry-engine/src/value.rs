//! SQLite value ↔ CBOR value mapping and logical column typing (F-04, F-13, F-15).

use crate::ffi;
use ciborium::Value;
use core::ffi::CStr;

/// Read column `i` of the current row as a CBOR value.
///
/// - INTEGER → CBOR integer (full 64-bit, never widened — D-02)
/// - FLOAT   → CBOR f64
/// - TEXT    → CBOR text (lossy UTF-8: SQLite TEXT is not guaranteed valid)
/// - BLOB    → CBOR byte string (D-06)
/// - NULL    → CBOR null
///
/// # Safety
/// `stmt` must be a valid statement positioned on a row (after SQLITE_ROW),
/// and `i` a valid column index.
pub unsafe fn column_value(stmt: *mut ffi::sqlite3_stmt, i: i32) -> Value {
    match ffi::sqlite3_column_type(stmt, i) {
        ffi::SQLITE_INTEGER => Value::from(ffi::sqlite3_column_int64(stmt, i)),
        ffi::SQLITE_FLOAT => Value::from(ffi::sqlite3_column_double(stmt, i)),
        ffi::SQLITE_TEXT => {
            let ptr = ffi::sqlite3_column_text(stmt, i);
            let len = ffi::sqlite3_column_bytes(stmt, i) as usize;
            let bytes = core::slice::from_raw_parts(ptr, len);
            Value::Text(String::from_utf8_lossy(bytes).into_owned())
        }
        ffi::SQLITE_BLOB => {
            let len = ffi::sqlite3_column_bytes(stmt, i) as usize;
            if len == 0 {
                return Value::Bytes(Vec::new());
            }
            let ptr = ffi::sqlite3_column_blob(stmt, i) as *const u8;
            Value::Bytes(core::slice::from_raw_parts(ptr, len).to_vec())
        }
        _ => Value::Null,
    }
}

/// Rough serialized size of a value, used for the max-bytes budget.
pub fn value_size(v: &Value) -> usize {
    match v {
        Value::Text(s) => s.len() + 2,
        Value::Bytes(b) => b.len() + 2,
        _ => 9,
    }
}

/// Map a declared column type to the envelope's logical type vocabulary.
/// Mirrors SQLite's affinity rules, refined with the date/bool/json
/// conventions the Typst layer coerces on.
pub fn logical_type_from_decltype(decltype: &str) -> Option<&'static str> {
    let d = decltype.to_ascii_uppercase();
    // Most specific first: conventions, then affinity substring rules.
    if d.contains("BOOL") {
        Some("boolean")
    } else if d.contains("DATETIME") || d.contains("TIMESTAMP") {
        Some("datetime")
    } else if d.contains("DATE") {
        Some("date")
    } else if d.contains("TIME") {
        Some("time")
    } else if d.contains("JSON") {
        Some("json")
    } else if d.contains("INT") {
        Some("integer")
    } else if d.contains("CHAR") || d.contains("CLOB") || d.contains("TEXT") {
        Some("text")
    } else if d.contains("BLOB") {
        Some("blob")
    } else if d.contains("REAL") || d.contains("FLOA") || d.contains("DOUB") {
        Some("real")
    } else if d.contains("DEC") || d.contains("NUM") {
        Some("numeric")
    } else {
        None
    }
}

/// Merge the logical type observed so far with a new row's storage class.
/// Used for expression columns with no decltype.
pub fn merge_observed(current: Option<&'static str>, v: &Value) -> Option<&'static str> {
    let observed = match v {
        Value::Integer(_) => "integer",
        Value::Float(_) => "real",
        Value::Text(_) => "text",
        Value::Bytes(_) => "blob",
        Value::Null => return current, // NULL doesn't narrow the type
        _ => "any",
    };
    match current {
        None => Some(observed),
        Some(c) if c == observed => current,
        Some("integer") if observed == "real" => Some("numeric"),
        Some("real") if observed == "integer" => Some("numeric"),
        Some("numeric") if observed == "integer" || observed == "real" => Some("numeric"),
        _ => Some("any"),
    }
}

/// # Safety
/// `ptr` must be null or point to a NUL-terminated C string.
pub unsafe fn opt_cstr(ptr: *const core::ffi::c_char) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decltype_mapping() {
        assert_eq!(logical_type_from_decltype("INTEGER"), Some("integer"));
        assert_eq!(logical_type_from_decltype("BIGINT"), Some("integer"));
        assert_eq!(logical_type_from_decltype("VARCHAR(80)"), Some("text"));
        assert_eq!(logical_type_from_decltype("BOOLEAN"), Some("boolean"));
        assert_eq!(logical_type_from_decltype("DATETIME"), Some("datetime"));
        assert_eq!(logical_type_from_decltype("TIMESTAMP"), Some("datetime"));
        assert_eq!(logical_type_from_decltype("DATE"), Some("date"));
        assert_eq!(logical_type_from_decltype("TIME"), Some("time"));
        assert_eq!(logical_type_from_decltype("JSONB"), Some("json"));
        assert_eq!(logical_type_from_decltype("DOUBLE PRECISION"), Some("real"));
        assert_eq!(logical_type_from_decltype("DECIMAL(10,2)"), Some("numeric"));
        assert_eq!(logical_type_from_decltype("BLOB"), Some("blob"));
        assert_eq!(logical_type_from_decltype("whatever"), None);
    }

    #[test]
    fn observed_merging() {
        let int = Value::from(1i64);
        let real = Value::from(1.5f64);
        let text = Value::Text("x".into());
        let null = Value::Null;
        assert_eq!(merge_observed(None, &int), Some("integer"));
        assert_eq!(merge_observed(Some("integer"), &real), Some("numeric"));
        assert_eq!(merge_observed(Some("numeric"), &int), Some("numeric"));
        assert_eq!(merge_observed(Some("integer"), &null), Some("integer"));
        assert_eq!(merge_observed(Some("integer"), &text), Some("any"));
    }
}
