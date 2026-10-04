//! Parameter binding (F-11, fixing D-14).
//!
//! Binding is the *only* supported way to get dynamic values into SQL.
//! Typst values arrive as CBOR; the special map shapes `{"@dt": iso}`,
//! `{"@dur": seconds}` and `{"@json": text}` carry datetime, duration and
//! collection values without CBOR tags (which Typst cannot decode).

use crate::error::{codes, QuarryError};
use crate::ffi::{self, *};
use crate::request::Params;
use ciborium::Value;
use core::ffi::c_int;
use std::collections::BTreeSet;

fn special_map(v: &Value) -> Option<(&str, &Value)> {
    let map = v.as_map()?;
    if map.len() != 1 {
        return None;
    }
    let (k, inner) = &map[0];
    let key = k.as_text()?;
    key.starts_with('@').then_some((key, inner))
}

unsafe fn bind_one(
    stmt: *mut ffi::sqlite3_stmt,
    index: c_int,
    value: &Value,
    describe: &str,
) -> Result<(), QuarryError> {
    let rc = match value {
        Value::Null => ffi::sqlite3_bind_null(stmt, index),
        Value::Bool(b) => ffi::sqlite3_bind_int64(stmt, index, i64::from(*b)),
        Value::Integer(i) => {
            let v = i64::try_from(i128::from(*i)).map_err(|_| {
                QuarryError::new(
                    codes::BINDING,
                    format!("parameter {describe} exceeds the 64-bit integer range"),
                )
            })?;
            ffi::sqlite3_bind_int64(stmt, index, v)
        }
        Value::Float(f) => ffi::sqlite3_bind_double(stmt, index, *f),
        Value::Text(s) => ffi::sqlite3_bind_text(
            stmt,
            index,
            s.as_ptr() as *const _,
            s.len() as c_int,
            SQLITE_TRANSIENT,
        ),
        Value::Bytes(b) => ffi::sqlite3_bind_blob(
            stmt,
            index,
            b.as_ptr() as *const _,
            b.len() as c_int,
            SQLITE_TRANSIENT,
        ),
        Value::Map(_) => match special_map(value) {
            Some(("@dt", Value::Text(iso))) => ffi::sqlite3_bind_text(
                stmt,
                index,
                iso.as_ptr() as *const _,
                iso.len() as c_int,
                SQLITE_TRANSIENT,
            ),
            Some(("@dur", inner)) => {
                let seconds = match inner {
                    Value::Integer(i) => i64::try_from(i128::from(*i)).unwrap_or(0) as f64,
                    Value::Float(f) => *f,
                    _ => {
                        return Err(QuarryError::new(
                            codes::BINDING,
                            format!("parameter {describe}: @dur must carry a number of seconds"),
                        ))
                    }
                };
                ffi::sqlite3_bind_double(stmt, index, seconds)
            }
            Some(("@json", Value::Text(json))) => ffi::sqlite3_bind_text(
                stmt,
                index,
                json.as_ptr() as *const _,
                json.len() as c_int,
                SQLITE_TRANSIENT,
            ),
            _ => return Err(QuarryError::new(
                codes::BINDING,
                format!(
                    "parameter {describe} has an unsupported type: bind int, float, str, bytes, \
                         bool, none, datetime or duration (arrays and dictionaries are bound as \
                         JSON text by the Typst layer)"
                ),
            )),
        },
        _ => {
            return Err(QuarryError::new(
                codes::BINDING,
                format!("parameter {describe} has an unsupported CBOR type"),
            ))
        }
    };
    if rc != SQLITE_OK {
        return Err(QuarryError::new(
            codes::BINDING,
            format!("could not bind parameter {describe} (sqlite code {rc})"),
        ));
    }
    Ok(())
}

/// Bind all parameters of one prepared statement. Records which named
/// parameters / positional slots were used so the caller can flag typos
/// (a supplied-but-unused parameter is an error, not a shrug).
pub struct BindOutcome {
    pub used_names: BTreeSet<String>,
    pub max_positional: usize,
}

/// # Safety
/// `stmt` must be a valid, prepared, un-finalized sqlite3_stmt.
pub unsafe fn bind_statement(
    stmt: *mut ffi::sqlite3_stmt,
    params: &Params,
) -> Result<BindOutcome, QuarryError> {
    let count = ffi::sqlite3_bind_parameter_count(stmt);
    let mut outcome = BindOutcome {
        used_names: BTreeSet::new(),
        max_positional: 0,
    };
    if count == 0 {
        return Ok(outcome);
    }
    for i in 1..=count {
        let name_ptr = ffi::sqlite3_bind_parameter_name(stmt, i);
        let name = crate::value::opt_cstr(name_ptr);
        match (&name, params) {
            (Some(full), Params::Named(named)) => {
                // ":year" / "@year" / "$year" → "year"
                let bare = &full[1..];
                match named.iter().find(|(k, _)| k == bare) {
                    Some((_, v)) => {
                        bind_one(stmt, i, v, &full.to_string())?;
                        outcome.used_names.insert(bare.to_string());
                    }
                    None => {
                        let expected: Vec<String> = collect_parameter_names(stmt);
                        let supplied: Vec<String> = named.iter().map(|(k, _)| k.clone()).collect();
                        let mut err = QuarryError::new(
                            codes::BINDING,
                            format!(
                                "missing value for parameter {full}: the SQL expects ({}), you supplied ({})",
                                expected.join(", "),
                                if supplied.is_empty() { "nothing".to_string() } else { supplied.join(", ") },
                            ),
                        );
                        err.hint = crate::error::spelling_hint(bare, &supplied);
                        return Err(err);
                    }
                }
            }
            (Some(full), Params::None) => {
                let expected: Vec<String> = collect_parameter_names(stmt);
                return Err(QuarryError::new(
                    codes::BINDING,
                    format!(
                        "the SQL expects parameters ({}) but none were supplied (first missing: {full})",
                        expected.join(", ")
                    ),
                ));
            }
            (Some(full), Params::Positional(_)) => {
                return Err(QuarryError::new(
                    codes::BINDING,
                    format!(
                        "the SQL uses named parameter {full} but positional params: (...) were supplied — \
                         pass them as named arguments instead"
                    ),
                ));
            }
            (None, Params::Positional(values)) => {
                let idx = i as usize;
                match values.get(idx - 1) {
                    Some(v) => {
                        bind_one(stmt, i, v, &format!("?{idx}"))?;
                        outcome.max_positional = outcome.max_positional.max(idx);
                    }
                    None => {
                        return Err(QuarryError::new(
                            codes::BINDING,
                            format!(
                                "the SQL expects at least {idx} positional parameters but only {} were supplied",
                                values.len()
                            ),
                        ))
                    }
                }
            }
            (None, Params::Named(_)) => {
                return Err(QuarryError::new(
                    codes::BINDING,
                    "the SQL uses positional '?' parameters but named parameters were supplied — \
                     use :name placeholders or pass params: (...)",
                ));
            }
            (None, Params::None) => {
                return Err(QuarryError::new(
                    codes::BINDING,
                    format!("the SQL expects {count} positional parameters but none were supplied"),
                ));
            }
        }
    }
    Ok(outcome)
}

unsafe fn collect_parameter_names(stmt: *mut ffi::sqlite3_stmt) -> Vec<String> {
    let count = ffi::sqlite3_bind_parameter_count(stmt);
    (1..=count)
        .filter_map(|i| crate::value::opt_cstr(ffi::sqlite3_bind_parameter_name(stmt, i)))
        .collect()
}
