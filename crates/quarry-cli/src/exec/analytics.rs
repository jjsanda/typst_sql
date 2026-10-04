//! Analytics over CSV/Parquet/JSON files (F-37), powered by Apache DataFusion
//! running natively in the sidecar.
//!
//! Why DataFusion and not an in-WASM engine: Phase-0 measured the wasmi
//! interpretation penalty at 20–60× and an optimized analytical WASM build
//! would blow the ~5 MB size budget — the spec's own R-01 kill criterion.
//! Sidecar-only analytics is the sanctioned fallback, and DataFusion is the
//! spec's recommended Rust engine for it (04-project-description.md §7.2).

use crate::config::Source;
use crate::exec::{apply_row_cap, dedupe_names, error_outcome, ExecOutcome, Generic};
use crate::requests::DiscoveredRequest;
use anyhow::{anyhow, bail, Context, Result};
use ciborium::Value;
use datafusion::arrow::array::{Array, AsArray};
use datafusion::arrow::datatypes::DataType;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::arrow::util::display::array_value_to_string;
use datafusion::common::ParamValues;
use datafusion::execution::options::JsonReadOptions;
use datafusion::prelude::*;
use datafusion::scalar::ScalarValue;
use quarry_engine::envelope::{ColumnMeta, Warning};
use quarry_engine::error::{codes, QuarryError};
use std::collections::BTreeMap;
use std::path::Path;

fn engine_desc() -> String {
    "datafusion 54".to_string()
}

/// Table name from a file path: stem with non-identifier chars mapped to `_`.
fn table_name_for(path: &Path) -> String {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let mut name: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if name.chars().next().is_none_or(|c| c.is_ascii_digit()) {
        name.insert(0, 't');
    }
    name.to_lowercase()
}

/// Minimal glob: `*` within the final path component (the common
/// `data/events-*.parquet` shape). Directories are not traversed recursively.
fn expand_glob(pattern: &str) -> Result<Vec<std::path::PathBuf>> {
    if !pattern.contains('*') {
        return Ok(vec![pattern.into()]);
    }
    let path = Path::new(pattern);
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_pattern = path
        .file_name()
        .ok_or_else(|| anyhow!("bad glob {pattern:?}"))?
        .to_string_lossy()
        .to_string();
    let parts: Vec<&str> = file_pattern.split('*').collect();
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("glob {pattern:?}"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        // sequential wildcard match
        let mut rest = name.as_str();
        let mut ok = true;
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            if i == 0 {
                if let Some(stripped) = rest.strip_prefix(part) {
                    rest = stripped;
                } else {
                    ok = false;
                    break;
                }
            } else if i == parts.len() - 1 {
                if !rest.ends_with(part) {
                    ok = false;
                }
                break;
            } else if let Some(pos) = rest.find(part) {
                rest = &rest[pos + part.len()..];
            } else {
                ok = false;
                break;
            }
        }
        if ok {
            out.push(entry.path());
        }
    }
    out.sort();
    if out.is_empty() {
        bail!("glob {pattern:?} matched no files");
    }
    Ok(out)
}

async fn register_all(ctx: &SessionContext, name: &str, source: &Source) -> Result<Vec<String>> {
    let mut tables = Vec::new();
    let register = |table: String, path: std::path::PathBuf| (table, path);
    let mut pairs: Vec<(String, std::path::PathBuf)> = Vec::new();
    for pattern in &source.files {
        for path in expand_glob(pattern)? {
            pairs.push(register(table_name_for(&path), path));
        }
    }
    for (table, path) in &source.tables {
        pairs.push((table.clone(), path.into()));
    }
    for (table, path) in pairs {
        let path_str = path.to_string_lossy().to_string();
        let ext = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        match ext.as_str() {
            "csv" => {
                ctx.register_csv(&table, &path_str, CsvReadOptions::new().has_header(true))
                    .await
                    .with_context(|| format!("source {name:?}: registering {path_str}"))?;
            }
            "parquet" => {
                ctx.register_parquet(&table, &path_str, ParquetReadOptions::default())
                    .await
                    .with_context(|| format!("source {name:?}: registering {path_str}"))?;
            }
            "json" | "ndjson" | "jsonl" => {
                ctx.register_json(&table, &path_str, JsonReadOptions::default())
                    .await
                    .with_context(|| format!("source {name:?}: registering {path_str}"))?;
            }
            other => bail!(
                "source {name:?}: unsupported analytics file extension {other:?} for {path_str} \
                 (csv, parquet, json/ndjson)"
            ),
        }
        tables.push(table);
    }
    Ok(tables)
}

fn cbor_to_scalar(v: &Value) -> Result<ScalarValue> {
    Ok(match v {
        Value::Null => ScalarValue::Null,
        Value::Bool(b) => ScalarValue::Boolean(Some(*b)),
        Value::Integer(i) => ScalarValue::Int64(Some(i64::try_from(i128::from(*i))?)),
        Value::Float(f) => ScalarValue::Float64(Some(*f)),
        Value::Text(s) => ScalarValue::Utf8(Some(s.clone())),
        Value::Bytes(b) => ScalarValue::Binary(Some(b.clone())),
        Value::Map(pairs) if pairs.len() == 1 => {
            let (k, inner) = &pairs[0];
            match (k.as_text(), inner) {
                (Some("@dt"), Value::Text(s)) => ScalarValue::Utf8(Some(s.clone())),
                (Some("@json"), Value::Text(s)) => ScalarValue::Utf8(Some(s.clone())),
                (Some("@dur"), Value::Float(f)) => ScalarValue::Float64(Some(*f)),
                (Some("@dur"), Value::Integer(i)) => {
                    ScalarValue::Float64(Some(i64::try_from(i128::from(*i))? as f64))
                }
                _ => bail!("unsupported parameter value {v:?}"),
            }
        }
        other => bail!("unsupported parameter value {other:?}"),
    })
}

fn logical_type(dt: &DataType) -> &'static str {
    match dt {
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => "integer",
        DataType::Float16 | DataType::Float32 | DataType::Float64 => "real",
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => "text",
        DataType::Boolean => "boolean",
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView => "blob",
        DataType::Date32 => "date",
        DataType::Date64 | DataType::Timestamp(_, _) => "datetime",
        DataType::Time32(_) | DataType::Time64(_) => "time",
        DataType::Decimal128(_, _) | DataType::Decimal256(_, _) => "numeric",
        _ => "any",
    }
}

fn cell_value(batch: &RecordBatch, col: usize, row: usize) -> Result<Value> {
    let array = batch.column(col);
    if array.is_null(row) {
        return Ok(Value::Null);
    }
    Ok(match array.data_type() {
        DataType::Boolean => Value::Bool(array.as_boolean().value(row)),
        DataType::Int8 => Value::from(
            array
                .as_primitive::<datafusion::arrow::datatypes::Int8Type>()
                .value(row) as i64,
        ),
        DataType::Int16 => Value::from(
            array
                .as_primitive::<datafusion::arrow::datatypes::Int16Type>()
                .value(row) as i64,
        ),
        DataType::Int32 => Value::from(
            array
                .as_primitive::<datafusion::arrow::datatypes::Int32Type>()
                .value(row) as i64,
        ),
        DataType::Int64 => Value::from(
            array
                .as_primitive::<datafusion::arrow::datatypes::Int64Type>()
                .value(row),
        ),
        DataType::UInt8 => Value::from(
            array
                .as_primitive::<datafusion::arrow::datatypes::UInt8Type>()
                .value(row) as i64,
        ),
        DataType::UInt16 => Value::from(
            array
                .as_primitive::<datafusion::arrow::datatypes::UInt16Type>()
                .value(row) as i64,
        ),
        DataType::UInt32 => Value::from(
            array
                .as_primitive::<datafusion::arrow::datatypes::UInt32Type>()
                .value(row) as i64,
        ),
        DataType::UInt64 => {
            let v = array
                .as_primitive::<datafusion::arrow::datatypes::UInt64Type>()
                .value(row);
            i64::try_from(v)
                .map(Value::from)
                .unwrap_or_else(|_| Value::Text(v.to_string()))
        }
        DataType::Float32 => Value::from(
            array
                .as_primitive::<datafusion::arrow::datatypes::Float32Type>()
                .value(row) as f64,
        ),
        DataType::Float64 => Value::from(
            array
                .as_primitive::<datafusion::arrow::datatypes::Float64Type>()
                .value(row),
        ),
        DataType::Utf8 => Value::Text(array.as_string::<i32>().value(row).to_string()),
        DataType::LargeUtf8 => Value::Text(array.as_string::<i64>().value(row).to_string()),
        DataType::Utf8View => Value::Text(array.as_string_view().value(row).to_string()),
        DataType::Binary => Value::Bytes(array.as_binary::<i32>().value(row).to_vec()),
        DataType::LargeBinary => Value::Bytes(array.as_binary::<i64>().value(row).to_vec()),
        // Dates, times, timestamps, decimals and everything exotic: arrow's
        // canonical text rendering (ISO for temporals) — typed coercion on the
        // Typst side picks temporals back up via the logical column type.
        DataType::Date32
        | DataType::Date64
        | DataType::Timestamp(_, _)
        | DataType::Time32(_)
        | DataType::Time64(_)
        | DataType::Decimal128(_, _)
        | DataType::Decimal256(_, _) => Value::Text(array_value_to_string(array, row)?),
        _ => Value::Text(array_value_to_string(array, row)?),
    })
}

fn ts_normalize(dt: &DataType, v: Value) -> Value {
    // arrow renders timestamps as "2026-01-15T12:30:45" (T separator); keep
    // as-is — the Typst ISO parser accepts both.
    let _ = dt;
    v
}

pub fn execute(
    name: &str,
    source: &Source,
    request: &DiscoveredRequest,
    fetched_iso: &str,
) -> Result<ExecOutcome> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(execute_async(name, source, request, fetched_iso))
}

async fn execute_async(
    name: &str,
    source: &Source,
    request: &DiscoveredRequest,
    fetched_iso: &str,
) -> Result<ExecOutcome> {
    let ctx = SessionContext::new();
    register_all(&ctx, name, source).await?;

    // Read-only is absolute (backend contract §Non-negotiables): DataFusion's
    // SQL dialect includes COPY TO and DDL, which would let a query write
    // files — refuse statements and DDL/DML outright.
    let options = datafusion::execution::context::SQLOptions::new()
        .with_allow_ddl(false)
        .with_allow_dml(false)
        .with_allow_statements(false);
    let df = match ctx.sql_with_options(&request.sql, options).await {
        Ok(df) => df,
        Err(e) => {
            // SQL errors are envelopes (cached, catchable) — not sync failures.
            let mut err = QuarryError::new(codes::SQLITE, e.to_string());
            err.code = "sql-error".into();
            err.sql = Some(request.sql.clone());
            err.source = Some(name.to_string());
            return Ok(error_outcome("analytics", &engine_desc(), err));
        }
    };

    // Bind parameters ($name or $1 placeholders in DataFusion SQL).
    let df = match &request.params {
        Value::Null => df,
        Value::Map(pairs) => {
            let mut map: BTreeMap<String, ScalarValue> = BTreeMap::new();
            for (k, v) in pairs {
                map.insert(
                    k.as_text().unwrap_or_default().to_string(),
                    cbor_to_scalar(v)?,
                );
            }
            match df.with_param_values(ParamValues::Map(
                map.into_iter().map(|(k, v)| (k, v.into())).collect(),
            )) {
                Ok(df) => df,
                Err(e) => {
                    let mut err = QuarryError::new(codes::BINDING, e.to_string());
                    err.sql = Some(request.sql.clone());
                    err.source = Some(name.to_string());
                    return Ok(error_outcome("analytics", &engine_desc(), err));
                }
            }
        }
        Value::Array(items) => {
            let scalars: Result<Vec<ScalarValue>> = items.iter().map(cbor_to_scalar).collect();
            let scalars: Vec<_> = scalars?.into_iter().map(|v| v.into()).collect();
            match df.with_param_values(ParamValues::List(scalars)) {
                Ok(df) => df,
                Err(e) => {
                    let mut err = QuarryError::new(codes::BINDING, e.to_string());
                    err.sql = Some(request.sql.clone());
                    return Ok(error_outcome("analytics", &engine_desc(), err));
                }
            }
        }
        other => bail!("unsupported params shape {other:?}"),
    };

    let batches = match df.collect().await {
        Ok(batches) => batches,
        Err(e) => {
            let mut err = QuarryError::new(codes::SQLITE, e.to_string());
            err.code = "sql-error".into();
            err.sql = Some(request.sql.clone());
            err.source = Some(name.to_string());
            return Ok(error_outcome("analytics", &engine_desc(), err));
        }
    };

    let mut warnings: Vec<Warning> = Vec::new();
    let (columns, mut rows) = if batches.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let schema = batches[0].schema();
        let names = dedupe_names(schema.fields().iter().map(|f| f.name().clone()).collect());
        let columns: Vec<ColumnMeta> = schema
            .fields()
            .iter()
            .zip(names.iter())
            .map(|(field, name)| ColumnMeta {
                name: name.clone(),
                logical_type: logical_type(field.data_type()),
                decltype: Some(field.data_type().to_string()),
                nullable: field.is_nullable(),
                table: None,
                origin: None,
            })
            .collect();
        let mut rows = Vec::new();
        for batch in &batches {
            for row in 0..batch.num_rows() {
                let mut out = Vec::with_capacity(batch.num_columns());
                for col in 0..batch.num_columns() {
                    let value = cell_value(batch, col, row)?;
                    out.push(ts_normalize(batch.column(col).data_type(), value));
                }
                rows.push(out);
            }
        }
        (columns, rows)
    };
    let truncated = apply_row_cap(&mut rows, source.max_rows, &mut warnings);

    Ok(Generic {
        columns,
        rows,
        truncated,
        warnings,
    }
    .into_outcome(name, "analytics", &engine_desc(), fetched_iso))
}
