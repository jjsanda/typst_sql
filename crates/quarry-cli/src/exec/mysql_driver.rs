//! MySQL/MariaDB through the sidecar (Phase 3): read-only session, execution
//! timeout, row caps. The mysql crate natively understands `:name` and `?`
//! placeholders, so quarry's SQL passes through unmodified.

use crate::config::Source;
use crate::exec::{apply_row_cap, dedupe_names, error_outcome, ExecOutcome, Generic};
use crate::requests::DiscoveredRequest;
use anyhow::{bail, Context, Result};
use ciborium::Value;
use mysql::consts::{ColumnFlags, ColumnType};
use mysql::prelude::*;
use mysql::{Opts, Params, Value as SqlValue};
use quarry_engine::envelope::{ColumnMeta, Warning};
use quarry_engine::error::QuarryError;

fn cbor_to_mysql(v: &Value) -> Result<SqlValue> {
    Ok(match v {
        Value::Null => SqlValue::NULL,
        Value::Bool(b) => SqlValue::Int(i64::from(*b)),
        Value::Integer(i) => SqlValue::Int(i64::try_from(i128::from(*i))?),
        Value::Float(f) => SqlValue::Double(*f),
        Value::Text(s) => SqlValue::Bytes(s.as_bytes().to_vec()),
        Value::Bytes(b) => SqlValue::Bytes(b.clone()),
        Value::Map(pairs) if pairs.len() == 1 => {
            let (k, inner) = &pairs[0];
            match (k.as_text(), inner) {
                (Some("@dt"), Value::Text(s)) => SqlValue::Bytes(s.as_bytes().to_vec()),
                (Some("@json"), Value::Text(s)) => SqlValue::Bytes(s.as_bytes().to_vec()),
                (Some("@dur"), Value::Float(f)) => SqlValue::Double(*f),
                (Some("@dur"), Value::Integer(i)) => {
                    SqlValue::Double(i64::try_from(i128::from(*i))? as f64)
                }
                _ => bail!("unsupported parameter value {v:?}"),
            }
        }
        other => bail!("unsupported parameter value {other:?}"),
    })
}

fn logical_type(column: &mysql::Column) -> &'static str {
    use ColumnType::*;
    let binary = column.flags().contains(ColumnFlags::BINARY_FLAG);
    match column.column_type() {
        MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_LONG | MYSQL_TYPE_LONGLONG
        | MYSQL_TYPE_INT24 | MYSQL_TYPE_YEAR => "integer",
        MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => "real",
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "numeric",
        MYSQL_TYPE_DATE => "date",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_TIMESTAMP => "datetime",
        MYSQL_TYPE_TIME => "time",
        MYSQL_TYPE_JSON => "json",
        MYSQL_TYPE_BLOB | MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB
            if binary =>
        {
            "blob"
        }
        _ => "text",
    }
}

fn sql_value_to_cbor(v: SqlValue, logical: &'static str) -> Value {
    match v {
        SqlValue::NULL => Value::Null,
        SqlValue::Int(i) => Value::from(i),
        SqlValue::UInt(u) => i64::try_from(u)
            .map(Value::from)
            .unwrap_or_else(|_| Value::Text(u.to_string())),
        SqlValue::Float(f) => Value::from(f as f64),
        SqlValue::Double(f) => Value::from(f),
        SqlValue::Bytes(bytes) => {
            if logical == "blob" {
                Value::Bytes(bytes)
            } else {
                Value::Text(String::from_utf8_lossy(&bytes).into_owned())
            }
        }
        SqlValue::Date(y, mo, d, h, mi, s, _us) => {
            if logical == "date" && h == 0 && mi == 0 && s == 0 {
                Value::Text(format!("{y:04}-{mo:02}-{d:02}"))
            } else {
                Value::Text(format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}"))
            }
        }
        SqlValue::Time(neg, days, h, m, s, _us) => {
            let sign = if neg { "-" } else { "" };
            Value::Text(format!("{sign}{:02}:{m:02}:{s:02}", days * 24 + h as u32))
        }
    }
}

pub fn execute(
    name: &str,
    source: &Source,
    request: &DiscoveredRequest,
    fetched_iso: &str,
) -> Result<ExecOutcome> {
    let url = source.resolve_url(name)?;
    let opts = Opts::from_url(&url).with_context(|| format!("source {name:?}: parsing URL"))?;
    let mut conn =
        mysql::Conn::new(opts).with_context(|| format!("source {name:?}: connecting"))?;

    let timeout_ms = source.timeout().as_millis();
    conn.query_drop("SET SESSION TRANSACTION READ ONLY")
        .with_context(|| format!("source {name:?}: enforcing read-only"))?;
    // best-effort (MySQL ≥ 5.7.8; MariaDB uses max_statement_time in seconds)
    let _ = conn.query_drop(format!("SET SESSION max_execution_time = {timeout_ms}"));

    let params = match &request.params {
        Value::Null => Params::Empty,
        Value::Map(pairs) => {
            let mut named = Vec::new();
            for (k, v) in pairs {
                named.push((
                    k.as_text().unwrap_or_default().to_string(),
                    cbor_to_mysql(v)?,
                ));
            }
            Params::from(named)
        }
        Value::Array(items) => {
            let positional: Result<Vec<SqlValue>> = items.iter().map(cbor_to_mysql).collect();
            Params::Positional(positional?)
        }
        other => bail!("unsupported params shape {other:?}"),
    };

    // exec_iter handles the empty-params case too (binary protocol throughout)
    let outcome = match conn.exec_iter(&request.sql, params) {
        Err(e) => {
            let mut err = QuarryError::new("sql-error", e.to_string());
            err.sql = Some(request.sql.clone());
            err.source = Some(name.to_string());
            Ok(error_outcome("mysql", "mysql", err))
        }
        Ok(mut iter) => {
            let mut warnings: Vec<Warning> = Vec::new();
            let mysql_columns = iter.columns().as_ref().to_vec();
            let names = dedupe_names(
                mysql_columns
                    .iter()
                    .map(|c| c.name_str().to_string())
                    .collect(),
            );
            let logicals: Vec<&'static str> = mysql_columns.iter().map(logical_type).collect();
            let columns: Vec<ColumnMeta> = mysql_columns
                .iter()
                .zip(names.iter())
                .zip(logicals.iter())
                .map(|((col, name), logical)| ColumnMeta {
                    name: name.clone(),
                    logical_type: logical,
                    decltype: Some(format!("{:?}", col.column_type())),
                    nullable: !col.flags().contains(ColumnFlags::NOT_NULL_FLAG),
                    table: Some(col.table_str().to_string()).filter(|t| !t.is_empty()),
                    origin: Some(col.org_name_str().to_string()).filter(|o| !o.is_empty()),
                })
                .collect();
            let mut rows: Vec<Vec<Value>> = Vec::new();
            for row in iter.by_ref() {
                let row = row?;
                let values = row.unwrap();
                rows.push(
                    values
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| {
                            sql_value_to_cbor(v, logicals.get(i).copied().unwrap_or("text"))
                        })
                        .collect(),
                );
            }
            let truncated = apply_row_cap(&mut rows, source.max_rows, &mut warnings);
            Ok(Generic {
                columns,
                rows,
                truncated,
                warnings,
            }
            .into_outcome(name, "mysql", "mysql", fetched_iso))
        }
    };
    outcome
}
