//! Postgres through the sidecar (Phase 3): native driver, enforced read-only
//! transaction, statement timeout, row caps, rustls TLS. Results land in the
//! same envelope v1 as every other backend.
//!
//! Dialect policy (Q-07): users write Postgres SQL. One convenience bridge:
//! quarry's `:name` / `?` placeholders are rewritten to `$n` so a document
//! query can move between sqlite and postgres unchanged when the SQL itself
//! is portable.

use crate::config::Source;
use crate::exec::{apply_row_cap, dedupe_names, error_outcome, ExecOutcome, Generic};
use crate::requests::DiscoveredRequest;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use ciborium::Value;
use postgres::types::{ToSql, Type};
use postgres::{Client, NoTls, Row};
use quarry_engine::envelope::{ColumnMeta, Warning};
use quarry_engine::error::{codes, QuarryError};

/// Rewrite :name and ? placeholders to $n (outside string literals and quoted
/// identifiers). Returns the rewritten SQL and the parameter order.
pub fn rewrite_placeholders(sql: &str) -> (String, Vec<PlaceholderRef>) {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut order: Vec<PlaceholderRef> = Vec::new();
    let mut named_index: std::collections::BTreeMap<String, usize> = Default::default();
    let mut i = 0;
    let mut in_quote: Option<u8> = None;
    let mut positional = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match in_quote {
            Some(q) => {
                out.push(c as char);
                if c == q {
                    in_quote = None;
                }
                i += 1;
            }
            None => match c {
                b'\'' | b'"' => {
                    in_quote = Some(c);
                    out.push(c as char);
                    i += 1;
                }
                // ':' begins a named placeholder unless '::' (a Postgres cast)
                b':' if i + 1 < bytes.len()
                    && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_')
                    && (i == 0 || bytes[i - 1] != b':')
                    && (i + 1 >= bytes.len() || bytes[i + 1] != b':') =>
                {
                    let start = i + 1;
                    let mut end = start;
                    while end < bytes.len()
                        && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                    {
                        end += 1;
                    }
                    let name = &sql[start..end];
                    let next = named_index.len() + positional + 1;
                    let n = *named_index.entry(name.to_string()).or_insert_with(|| {
                        order.push(PlaceholderRef::Named(name.to_string()));
                        next
                    });
                    out.push_str(&format!("${n}"));
                    i = end;
                }
                b'?' => {
                    positional += 1;
                    order.push(PlaceholderRef::Positional(positional - 1));
                    out.push_str(&format!("${}", named_index.len() + positional));
                    i += 1;
                }
                _ => {
                    out.push(c as char);
                    i += 1;
                }
            },
        }
    }
    (out, order)
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaceholderRef {
    Named(String),
    Positional(usize),
}

enum PgParam {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

impl PgParam {
    fn as_tosql(&self) -> &(dyn ToSql + Sync) {
        match self {
            PgParam::Null => &None::<String>,
            PgParam::Bool(v) => v,
            PgParam::Int(v) => v,
            PgParam::Float(v) => v,
            PgParam::Text(v) => v,
            PgParam::Bytes(v) => v,
        }
    }
}

fn cbor_to_pg(v: &Value) -> Result<PgParam> {
    Ok(match v {
        Value::Null => PgParam::Null,
        Value::Bool(b) => PgParam::Bool(*b),
        Value::Integer(i) => PgParam::Int(i64::try_from(i128::from(*i))?),
        Value::Float(f) => PgParam::Float(*f),
        Value::Text(s) => PgParam::Text(s.clone()),
        Value::Bytes(b) => PgParam::Bytes(b.clone()),
        Value::Map(pairs) if pairs.len() == 1 => {
            let (k, inner) = &pairs[0];
            match (k.as_text(), inner) {
                (Some("@dt"), Value::Text(s)) => PgParam::Text(s.clone()),
                (Some("@json"), Value::Text(s)) => PgParam::Text(s.clone()),
                (Some("@dur"), Value::Float(f)) => PgParam::Float(*f),
                (Some("@dur"), Value::Integer(i)) => {
                    PgParam::Float(i64::try_from(i128::from(*i))? as f64)
                }
                _ => bail!("unsupported parameter value {v:?}"),
            }
        }
        other => bail!("unsupported parameter value {other:?}"),
    })
}

fn logical_type(t: &Type) -> &'static str {
    match *t {
        Type::BOOL => "boolean",
        Type::INT2 | Type::INT4 | Type::INT8 | Type::OID => "integer",
        Type::FLOAT4 | Type::FLOAT8 => "real",
        Type::NUMERIC => "numeric",
        Type::BYTEA => "blob",
        Type::DATE => "date",
        Type::TIMESTAMP | Type::TIMESTAMPTZ => "datetime",
        Type::TIME => "time",
        Type::JSON | Type::JSONB => "json",
        _ => "text",
    }
}

fn cell(row: &Row, idx: usize, t: &Type) -> Result<Value> {
    Ok(match *t {
        Type::BOOL => row
            .try_get::<_, Option<bool>>(idx)?
            .map(Value::Bool)
            .unwrap_or(Value::Null),
        Type::INT2 => row
            .try_get::<_, Option<i16>>(idx)?
            .map(|v| Value::from(v as i64))
            .unwrap_or(Value::Null),
        Type::INT4 => row
            .try_get::<_, Option<i32>>(idx)?
            .map(|v| Value::from(v as i64))
            .unwrap_or(Value::Null),
        Type::INT8 => row
            .try_get::<_, Option<i64>>(idx)?
            .map(Value::from)
            .unwrap_or(Value::Null),
        Type::FLOAT4 => row
            .try_get::<_, Option<f32>>(idx)?
            .map(|v| Value::from(v as f64))
            .unwrap_or(Value::Null),
        Type::FLOAT8 => row
            .try_get::<_, Option<f64>>(idx)?
            .map(Value::from)
            .unwrap_or(Value::Null),
        Type::BYTEA => row
            .try_get::<_, Option<Vec<u8>>>(idx)?
            .map(Value::Bytes)
            .unwrap_or(Value::Null),
        Type::TIMESTAMP => row
            .try_get::<_, Option<NaiveDateTime>>(idx)?
            .map(|v| Value::Text(v.format("%Y-%m-%d %H:%M:%S").to_string()))
            .unwrap_or(Value::Null),
        Type::TIMESTAMPTZ => row
            .try_get::<_, Option<DateTime<Utc>>>(idx)?
            .map(|v| Value::Text(v.format("%Y-%m-%d %H:%M:%S").to_string()))
            .unwrap_or(Value::Null),
        Type::DATE => row
            .try_get::<_, Option<NaiveDate>>(idx)?
            .map(|v| Value::Text(v.format("%Y-%m-%d").to_string()))
            .unwrap_or(Value::Null),
        Type::TIME => row
            .try_get::<_, Option<NaiveTime>>(idx)?
            .map(|v| Value::Text(v.format("%H:%M:%S").to_string()))
            .unwrap_or(Value::Null),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => row
            .try_get::<_, Option<String>>(idx)?
            .map(Value::Text)
            .unwrap_or(Value::Null),
        ref other => {
            // No silent stringification of types we do not understand — the
            // user gets an actionable error instead of a maybe-wrong value.
            bail!(
                "column {idx} has Postgres type {other} which quarry does not map yet — \
                 CAST it in SQL (e.g. ::text, ::float8, ::bigint)"
            )
        }
    })
}

pub fn execute(
    name: &str,
    source: &Source,
    request: &DiscoveredRequest,
    fetched_iso: &str,
) -> Result<ExecOutcome> {
    let url = source.resolve_url(name)?;
    let mut client = connect(&url).with_context(|| format!("source {name:?}: connecting"))?;

    // Read-only and bounded, before anything else (Phase-3 exit criteria).
    let timeout_ms = source.timeout().as_millis();
    client
        .batch_execute(&format!(
            "SET default_transaction_read_only = on; SET statement_timeout = {timeout_ms};"
        ))
        .with_context(|| format!("source {name:?}: session setup"))?;

    let (sql, order) = rewrite_placeholders(&request.sql);
    let params: Vec<PgParam> = match (&request.params, order.is_empty()) {
        (Value::Null, true) => Vec::new(),
        (Value::Null, false) => {
            bail!("source {name:?}: the SQL has placeholders but no params were supplied")
        }
        (Value::Map(named), _) => {
            let mut out = Vec::new();
            for reference in &order {
                match reference {
                    PlaceholderRef::Named(param_name) => {
                        let found = named
                            .iter()
                            .find(|(k, _)| k.as_text() == Some(param_name.as_str()));
                        match found {
                            Some((_, v)) => out.push(cbor_to_pg(v)?),
                            None => bail!("source {name:?}: missing parameter :{param_name}"),
                        }
                    }
                    PlaceholderRef::Positional(_) => {
                        bail!("source {name:?}: mixing ? and :name placeholders is not supported")
                    }
                }
            }
            out
        }
        (Value::Array(items), _) => {
            let mut out = Vec::new();
            for reference in &order {
                match reference {
                    PlaceholderRef::Positional(i) => match items.get(*i) {
                        Some(v) => out.push(cbor_to_pg(v)?),
                        None => bail!("source {name:?}: missing positional parameter {}", i + 1),
                    },
                    PlaceholderRef::Named(n) => {
                        bail!("source {name:?}: named placeholder :{n} with positional params")
                    }
                }
            }
            out
        }
        (other, _) => bail!("unsupported params shape {other:?}"),
    };
    let param_refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p.as_tosql()).collect();

    // One read-only transaction per query.
    let mut tx = client.transaction()?;
    let rows_result = tx.query(&sql, &param_refs);
    let outcome = match rows_result {
        Err(e) => {
            // SQL errors are envelopes: cached, catchable, reproducible offline.
            // (postgres::Error's Display is a generic "db error"; the real
            // message lives in the DbError.)
            let message = e
                .as_db_error()
                .map(|db| {
                    let mut m = db.message().to_string();
                    if let Some(detail) = db.detail() {
                        m.push_str(" — ");
                        m.push_str(detail);
                    }
                    m
                })
                .unwrap_or_else(|| e.to_string());
            let mut err = QuarryError::new(codes::SQLITE, message);
            err.code = "sql-error".into();
            err.sql = Some(request.sql.clone());
            err.source = Some(name.to_string());
            error_outcome("postgres", "postgresql", err)
        }
        Ok(pg_rows) => {
            let mut warnings: Vec<Warning> = Vec::new();
            let (columns, mut rows) = if pg_rows.is_empty() {
                (Vec::new(), Vec::new())
            } else {
                let cols = pg_rows[0].columns();
                let names = dedupe_names(cols.iter().map(|c| c.name().to_string()).collect());
                let columns: Vec<ColumnMeta> = cols
                    .iter()
                    .zip(names.iter())
                    .map(|(col, name)| ColumnMeta {
                        name: name.clone(),
                        logical_type: logical_type(col.type_()),
                        decltype: Some(col.type_().to_string()),
                        nullable: true,
                        table: None,
                        origin: None,
                    })
                    .collect();
                let mut rows = Vec::new();
                for pg_row in &pg_rows {
                    let mut out = Vec::with_capacity(cols.len());
                    for (idx, col) in pg_row.columns().iter().enumerate() {
                        out.push(cell(pg_row, idx, col.type_())?);
                    }
                    rows.push(out);
                }
                (columns, rows)
            };
            let truncated = apply_row_cap(&mut rows, source.max_rows, &mut warnings);
            Generic {
                columns,
                rows,
                truncated,
                warnings,
            }
            .into_outcome(name, "postgres", "postgresql", fetched_iso)
        }
    };
    tx.rollback()?;
    Ok(outcome)
}

fn connect(url: &str) -> Result<Client> {
    let needs_tls = url.contains("sslmode=require")
        || url.contains("sslmode=verify-ca")
        || url.contains("sslmode=verify-full");
    if needs_tls {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let tls = tokio_postgres_rustls::MakeRustlsConnect::new(config);
        Ok(Client::connect(url, tls)?)
    } else {
        Ok(Client::connect(url, NoTls)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_named_placeholders() {
        let (sql, order) =
            rewrite_placeholders("SELECT * FROM t WHERE a = :year AND b = :name AND c = :year");
        assert_eq!(sql, "SELECT * FROM t WHERE a = $1 AND b = $2 AND c = $1");
        assert_eq!(
            order,
            vec![
                PlaceholderRef::Named("year".into()),
                PlaceholderRef::Named("name".into())
            ]
        );
    }

    #[test]
    fn rewrites_positional_and_respects_strings_and_casts() {
        let (sql, order) =
            rewrite_placeholders("SELECT ':not_a_param', x::int, ? FROM t WHERE y = ?");
        assert_eq!(sql, "SELECT ':not_a_param', x::int, $1 FROM t WHERE y = $2");
        assert_eq!(
            order,
            vec![PlaceholderRef::Positional(0), PlaceholderRef::Positional(1)]
        );
        // quoted identifiers too
        let (sql, _) = rewrite_placeholders("SELECT \":alias\" FROM t");
        assert_eq!(sql, "SELECT \":alias\" FROM t");
    }
}
