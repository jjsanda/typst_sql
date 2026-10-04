//! Schema introspection (F-30) and statement description.

use crate::conn::Connection;
use crate::error::{codes, QuarryError};
use crate::ffi::{self, *};
use crate::value;
use ciborium::Value;
use std::ffi::CString;

fn text(v: &Value) -> String {
    v.as_text().unwrap_or("").to_string()
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn sql_str(name: &str) -> String {
    name.replace('\'', "''")
}

/// Full introspection of every source: tables, columns, primary keys, foreign
/// keys, indexes and row counts. Returned as a plain CBOR value (not an
/// envelope) — this is metadata, not query data.
pub fn schema_value(conn: &Connection, row_counts: bool) -> Result<Value, QuarryError> {
    let mut sources = Vec::new();
    for src in &conn.sources {
        let tables = conn
            .internal_rows(&format!(
                "SELECT name, type FROM {}.sqlite_schema \
                 WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
                quote_ident(src)
            ))
            .map_err(|(code, msg)| {
                QuarryError::new(
                    codes::SQLITE,
                    format!("schema introspection failed: {msg} (code {code})"),
                )
            })?;

        let mut table_values = Vec::new();
        for row in tables {
            let name = text(&row[0]);
            let kind = text(&row[1]);
            let escaped = sql_str(&name);

            let columns = conn
                .internal_rows(&format!(
                    "SELECT name, type, \"notnull\", dflt_value, pk \
                     FROM pragma_table_xinfo('{escaped}', '{}') WHERE hidden = 0",
                    sql_str(src)
                ))
                .unwrap_or_default();
            let column_values: Vec<Value> = columns
                .iter()
                .map(|c| {
                    Value::Map(vec![
                        (Value::Text("name".into()), c[0].clone()),
                        (Value::Text("type".into()), c[1].clone()),
                        (
                            Value::Text("logical-type".into()),
                            Value::Text(
                                c[1].as_text()
                                    .and_then(value::logical_type_from_decltype)
                                    .unwrap_or("any")
                                    .into(),
                            ),
                        ),
                        (
                            Value::Text("not-null".into()),
                            Value::Bool(matches!(&c[2], Value::Integer(i) if i128::from(*i) != 0)),
                        ),
                        (Value::Text("default".into()), c[3].clone()),
                        (
                            Value::Text("primary-key".into()),
                            Value::Bool(matches!(&c[4], Value::Integer(i) if i128::from(*i) != 0)),
                        ),
                    ])
                })
                .collect();

            let fks = conn
                .internal_rows(&format!(
                    "SELECT \"table\", \"from\", \"to\" FROM pragma_foreign_key_list('{escaped}', '{}')",
                    sql_str(src)
                ))
                .unwrap_or_default();
            let fk_values: Vec<Value> = fks
                .iter()
                .map(|f| {
                    Value::Map(vec![
                        (Value::Text("table".into()), f[0].clone()),
                        (Value::Text("from".into()), f[1].clone()),
                        (Value::Text("to".into()), f[2].clone()),
                    ])
                })
                .collect();

            let indexes = conn
                .internal_rows(&format!(
                    "SELECT name, \"unique\", origin FROM pragma_index_list('{escaped}', '{}')",
                    sql_str(src)
                ))
                .unwrap_or_default();
            let index_values: Vec<Value> = indexes
                .iter()
                .map(|ix| {
                    let ix_name = text(&ix[0]);
                    let cols = conn
                        .internal_rows(&format!(
                            "SELECT name FROM pragma_index_info('{}', '{}')",
                            sql_str(&ix_name),
                            sql_str(src)
                        ))
                        .unwrap_or_default();
                    Value::Map(vec![
                        (Value::Text("name".into()), ix[0].clone()),
                        (
                            Value::Text("unique".into()),
                            Value::Bool(matches!(&ix[1], Value::Integer(i) if i128::from(*i) != 0)),
                        ),
                        (Value::Text("origin".into()), ix[2].clone()),
                        (
                            Value::Text("columns".into()),
                            Value::Array(cols.iter().map(|c| c[0].clone()).collect()),
                        ),
                    ])
                })
                .collect();

            let row_count = if row_counts && kind == "table" {
                conn.internal_rows(&format!(
                    "SELECT count(*) FROM {}.{}",
                    quote_ident(src),
                    quote_ident(&name)
                ))
                .ok()
                .and_then(|rows| rows.first().and_then(|r| r.first().cloned()))
                .unwrap_or(Value::Null)
            } else {
                Value::Null
            };

            table_values.push(Value::Map(vec![
                (Value::Text("name".into()), Value::Text(name)),
                (Value::Text("kind".into()), Value::Text(kind)),
                (Value::Text("columns".into()), Value::Array(column_values)),
                (Value::Text("foreign-keys".into()), Value::Array(fk_values)),
                (Value::Text("indexes".into()), Value::Array(index_values)),
                (Value::Text("row-count".into()), row_count),
            ]));
        }
        sources.push(Value::Map(vec![
            (Value::Text("name".into()), Value::Text(src.clone())),
            (Value::Text("tables".into()), Value::Array(table_values)),
        ]));
    }
    Ok(Value::Map(vec![
        (Value::Text("ok".into()), Value::Bool(true)),
        (Value::Text("sources".into()), Value::Array(sources)),
        (
            Value::Text("engine".into()),
            Value::Text(conn.engine_desc.clone()),
        ),
    ]))
}

/// Describe a statement without executing it: its columns and parameters.
pub fn describe_value(conn: &Connection, sql: &str) -> Result<Value, QuarryError> {
    if sql.as_bytes().contains(&0) {
        return Err(QuarryError::new(codes::PROTOCOL, "SQL contains a NUL byte"));
    }
    let sql_c = CString::new(sql).expect("checked");
    let sql_start = sql_c.as_ptr();
    let mut statements = Vec::new();
    let mut cursor = sql_start;
    let sql_len = sql.len();
    unsafe {
        loop {
            let consumed = cursor.offset_from(sql_start) as usize;
            if consumed >= sql_len {
                break;
            }
            let mut stmt: *mut ffi::sqlite3_stmt = core::ptr::null_mut();
            let mut tail: *const core::ffi::c_char = core::ptr::null();
            let rc = ffi::sqlite3_prepare_v2(
                conn.db,
                cursor,
                (sql_len - consumed) as i32,
                &mut stmt,
                &mut tail,
            );
            if rc != SQLITE_OK {
                return Err(crate::exec::map_sqlite_error(
                    conn,
                    rc,
                    sql,
                    statements.len(),
                    consumed,
                    true,
                ));
            }
            if stmt.is_null() {
                if tail > cursor {
                    cursor = tail;
                    continue;
                }
                break;
            }
            let ncol = ffi::sqlite3_column_count(stmt);
            let columns: Vec<Value> = (0..ncol)
                .map(|i| {
                    Value::Map(vec![
                        (
                            Value::Text("name".into()),
                            Value::Text(
                                value::opt_cstr(ffi::sqlite3_column_name(stmt, i))
                                    .unwrap_or_else(|| format!("column{i}")),
                            ),
                        ),
                        (
                            Value::Text("decltype".into()),
                            value::opt_cstr(ffi::sqlite3_column_decltype(stmt, i))
                                .map(Value::Text)
                                .unwrap_or(Value::Null),
                        ),
                    ])
                })
                .collect();
            let nparam = ffi::sqlite3_bind_parameter_count(stmt);
            let params: Vec<Value> = (1..=nparam)
                .map(|i| {
                    value::opt_cstr(ffi::sqlite3_bind_parameter_name(stmt, i))
                        .map(Value::Text)
                        .unwrap_or(Value::Null)
                })
                .collect();
            let readonly = ffi::sqlite3_stmt_readonly(stmt) != 0;
            ffi::sqlite3_finalize(stmt);
            statements.push(Value::Map(vec![
                (Value::Text("columns".into()), Value::Array(columns)),
                (Value::Text("parameters".into()), Value::Array(params)),
                (Value::Text("read-only".into()), Value::Bool(readonly)),
            ]));
            cursor = tail;
        }
    }
    Ok(Value::Map(vec![
        (Value::Text("ok".into()), Value::Bool(true)),
        (Value::Text("statements".into()), Value::Array(statements)),
    ]))
}
