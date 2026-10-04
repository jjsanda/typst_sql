//! Query execution: multi-statement loop, row pump, caps, purity guard.
//!
//! Semantics (F-05, fixing D-07): *every* statement executes. `"last"` returns
//! the final result set that produced columns; `"all"` returns each result
//! set; `"reject"` refuses multi-statement input. Nothing is ever silently
//! dropped, and `stats.statements` reports how many statements ran.
//!
//! The purity guard (C-4) makes every call order-independent: the seeded
//! random stream is rewound, SQLite's PRNG is reset, the whole call runs in a
//! rolled-back transaction (discarding temp objects), and stray statements are
//! finalized on exit. A query's envelope must be a function of (open args,
//! request) alone — the fresh-vs-dirty differential test enforces this.

use crate::bind::bind_statement;
use crate::conn::Connection;
use crate::envelope::{self, ClockInfo, ColumnMeta, ResultSet, Stats, Warning};
use crate::error::{codes, QuarryError};
use crate::ffi::{self, *};
use crate::request::{MultiStatement, Params, Request};
use crate::value;
use crate::vfs;
use ciborium::Value;
use core::ffi::c_char;
use std::collections::BTreeSet;
use std::ffi::CString;

/// Entry point: parse, execute and encode. Never fails at the protocol level —
/// every failure becomes an error envelope (that is what keeps errors
/// catchable in the document, D-17).
pub fn run_query(conn: &Connection, request_bytes: &[u8]) -> Vec<u8> {
    let request = match Request::parse(request_bytes) {
        Ok(r) => r,
        Err(e) => return envelope::error_envelope(&e, &conn.engine_desc),
    };

    let guard = PurityGuard::enter(conn);
    let outcome = execute(conn, &request);
    drop(guard);

    match outcome {
        Ok(exec) => {
            let stats = build_stats(conn, &exec);
            let mut warnings = conn.warnings.clone();
            warnings.extend(exec.warnings.clone());
            match request.options.multi_statement {
                MultiStatement::All => envelope::ok_all_envelope(
                    &exec.all_results,
                    &stats,
                    &warnings,
                    request.options.positional_rows,
                ),
                _ => {
                    let empty = ResultSet {
                        columns: Vec::new(),
                        rows: Vec::new(),
                        truncated: false,
                    };
                    let result = exec.last_result.as_ref().unwrap_or(&empty);
                    envelope::ok_envelope(
                        result,
                        &stats,
                        &warnings,
                        request.options.positional_rows,
                    )
                }
            }
        }
        Err(e) => envelope::error_envelope(&e, &conn.engine_desc),
    }
}

struct ExecOutcome {
    last_result: Option<ResultSet>,
    all_results: Vec<ResultSet>,
    statements: u64,
    warnings: Vec<Warning>,
}

fn build_stats(conn: &Connection, exec: &ExecOutcome) -> Stats {
    let row_count = match &exec.last_result {
        Some(r) => r.rows.len() as u64,
        None => exec.all_results.last().map_or(0, |r| r.rows.len() as u64),
    };
    let truncated = exec.last_result.as_ref().map_or_else(
        || exec.all_results.iter().any(|r| r.truncated),
        |r| r.truncated,
    );
    Stats {
        row_count,
        truncated,
        source: conn.sources.join(","),
        statements: exec.statements,
        clock: ClockInfo {
            value: conn.meta.clock_display.clone(),
            origin: conn.meta.clock_origin.clone(),
        },
        seed: Some(conn.meta.seed),
        engine: conn.engine_desc.clone(),
        placeholder: false,
    }
}

struct PurityGuard<'a> {
    conn: &'a Connection,
}

impl<'a> PurityGuard<'a> {
    fn enter(conn: &'a Connection) -> Self {
        // Rewind the seeded random stream, then force SQLite's internal PRNG
        // to reseed from it: random() results become independent of how many
        // queries ran before this one (the D-05 defect class in a new guise).
        vfs::rewind_rng();
        unsafe { ffi::sqlite3_randomness(0, core::ptr::null_mut()) };
        let _ = conn.internal_exec("BEGIN DEFERRED");
        PurityGuard { conn }
    }
}

impl Drop for PurityGuard<'_> {
    fn drop(&mut self) {
        // Discard temp objects and any other in-transaction state.
        // NOTE: no sqlite3_next_stmt finalize sweep here — statements found
        // that way can belong to extension modules (FTS5 caches statements on
        // its shadow tables) and finalizing them out from under their owner is
        // a use-after-free at close time. The statement loop owns its
        // statements and finalizes them on every path.
        let _ = self.conn.internal_exec("ROLLBACK");
        unsafe {
            ffi::sqlite3_set_last_insert_rowid(self.conn.db, 0);
            ffi::sqlite3_db_release_memory(self.conn.db);
        }
    }
}

fn execute(conn: &Connection, request: &Request) -> Result<ExecOutcome, QuarryError> {
    let sql = request.sql.as_str();
    if sql.as_bytes().contains(&0) {
        return Err(QuarryError::new(codes::PROTOCOL, "SQL contains a NUL byte"));
    }
    // CURRENT_TIMESTAMP / CURRENT_DATE / CURRENT_TIME compile to internal
    // clock reads that silently yield NULL when the VFS clock refuses — the
    // exact silent failure mode this project exists to prevent. Refuse loudly
    // up front when no clock is injected. (date('now') etc. are handled by
    // the overridden functions installed at open.)
    if vfs::clock().is_none() && mentions_current_time_keyword(sql) {
        return Err(
            QuarryError::new(codes::CLOCK_REQUIRED, crate::conn::clock_required_message())
                .with_sql(sql, 0),
        );
    }
    let sql_c = CString::new(sql).expect("checked for NUL above");
    let sql_start = sql_c.as_ptr();
    let sql_len = sql.len();

    let mut outcome = ExecOutcome {
        last_result: None,
        all_results: Vec::new(),
        statements: 0,
        warnings: Vec::new(),
    };
    let mut used_names: BTreeSet<String> = BTreeSet::new();
    let mut max_positional = 0usize;
    let mut cursor: *const c_char = sql_start;

    unsafe {
        loop {
            let consumed = cursor.offset_from(sql_start) as usize;
            if consumed >= sql_len {
                break;
            }
            let remaining = (sql_len - consumed) as i32;
            let mut stmt: *mut ffi::sqlite3_stmt = core::ptr::null_mut();
            let mut tail: *const c_char = core::ptr::null();
            let rc = ffi::sqlite3_prepare_v2(conn.db, cursor, remaining, &mut stmt, &mut tail);
            if rc != SQLITE_OK {
                return Err(map_sqlite_error(
                    conn,
                    rc,
                    sql,
                    outcome.statements as usize,
                    consumed,
                    true,
                ));
            }
            if stmt.is_null() {
                // Trailing whitespace or comments — no more statements.
                if tail > cursor {
                    cursor = tail;
                    continue;
                }
                break;
            }
            outcome.statements += 1;
            if outcome.statements > 1 && request.options.multi_statement == MultiStatement::Reject {
                ffi::sqlite3_finalize(stmt);
                return Err(QuarryError::new(
                    codes::MULTI_STATEMENT,
                    "the SQL contains more than one statement and multi-statement is set to \"reject\"",
                )
                .with_sql(sql, outcome.statements as usize - 1));
            }

            let bind_outcome = match bind_statement(stmt, &request.params) {
                Ok(o) => o,
                Err(mut e) => {
                    ffi::sqlite3_finalize(stmt);
                    e = e.with_sql(sql, outcome.statements as usize - 1);
                    return Err(e);
                }
            };
            used_names.extend(bind_outcome.used_names);
            max_positional = max_positional.max(bind_outcome.max_positional);

            let has_columns = ffi::sqlite3_column_count(stmt) > 0;
            let result = pump_rows(conn, stmt, request, has_columns).map_err(|mut e| {
                ffi::sqlite3_finalize(stmt);
                if e.sql.is_none() {
                    e = e.with_sql(sql, outcome.statements as usize - 1);
                }
                e
            })?;
            ffi::sqlite3_finalize(stmt);

            if has_columns {
                if let Some(rs) = result {
                    if request.options.multi_statement == MultiStatement::All {
                        outcome.all_results.push(rs);
                    } else {
                        outcome.last_result = Some(rs);
                    }
                }
            }
            cursor = tail;
        }
    }

    // A supplied-but-unused parameter is a typo waiting to print the wrong
    // number; fail loudly.
    if let Params::Named(named) = &request.params {
        let unused: Vec<&String> = named
            .iter()
            .map(|(k, _)| k)
            .filter(|k| !used_names.contains(*k))
            .collect();
        if !unused.is_empty() {
            let mut e = QuarryError::new(
                codes::BINDING,
                format!(
                    "parameter{} ({}) {} supplied but never used by the SQL",
                    if unused.len() > 1 { "s" } else { "" },
                    unused
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    if unused.len() > 1 { "were" } else { "was" },
                ),
            );
            if let Some(first) = unused.first() {
                e.hint = crate::error::spelling_hint(
                    first,
                    &used_names.iter().cloned().collect::<Vec<_>>(),
                );
            }
            return Err(e.with_sql(sql, 0));
        }
    }
    if let Params::Positional(values) = &request.params {
        if values.len() > max_positional {
            return Err(QuarryError::new(
                codes::BINDING,
                format!(
                    "{} positional parameters supplied but the SQL only uses {}",
                    values.len(),
                    max_positional
                ),
            )
            .with_sql(sql, 0));
        }
    }

    if outcome.statements == 0 {
        return Err(QuarryError::new(
            codes::SQLITE,
            "the SQL contains no statements",
        ));
    }
    Ok(outcome)
}

/// Step a statement to completion, capturing rows (with offset/limit/caps)
/// when it produces columns.
unsafe fn pump_rows(
    conn: &Connection,
    stmt: *mut ffi::sqlite3_stmt,
    request: &Request,
    has_columns: bool,
) -> Result<Option<ResultSet>, QuarryError> {
    let opts = &request.options;
    let ncol = ffi::sqlite3_column_count(stmt);

    // Column metadata up front; logical types refine as rows are observed.
    let mut columns: Vec<ColumnMeta> = Vec::with_capacity(ncol as usize);
    let mut decl_types: Vec<Option<&'static str>> = Vec::with_capacity(ncol as usize);
    let mut observed: Vec<Option<&'static str>> = vec![None; ncol as usize];
    let mut saw_null: Vec<bool> = vec![false; ncol as usize];
    let mut name_counts: std::collections::BTreeMap<String, usize> = Default::default();
    let mut duplicate_names = false;
    for i in 0..ncol {
        let raw_name = value::opt_cstr(ffi::sqlite3_column_name(stmt, i))
            .unwrap_or_else(|| format!("column{i}"));
        let count = name_counts.entry(raw_name.clone()).or_insert(0);
        *count += 1;
        let name = if *count > 1 {
            duplicate_names = true;
            format!("{raw_name}_{count}")
        } else {
            raw_name
        };
        let decltype = value::opt_cstr(ffi::sqlite3_column_decltype(stmt, i));
        let logical = decltype
            .as_deref()
            .and_then(value::logical_type_from_decltype);
        decl_types.push(logical);
        columns.push(ColumnMeta {
            name,
            logical_type: "null",
            decltype,
            nullable: false,
            table: value::opt_cstr(ffi::sqlite3_column_table_name(stmt, i)),
            origin: value::opt_cstr(ffi::sqlite3_column_origin_name(stmt, i)),
        });
    }

    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut truncated = false;
    let mut skipped = 0u64;
    let mut bytes_used = 0usize;
    let effective_limit = match (opts.max_rows, opts.row_limit) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };

    loop {
        let rc = ffi::sqlite3_step(stmt);
        if rc == SQLITE_DONE {
            break;
        }
        if rc != SQLITE_ROW {
            return Err(map_sqlite_error(conn, rc, &request.sql, 0, 0, false));
        }
        if !has_columns {
            continue;
        }
        if skipped < opts.row_offset {
            skipped += 1;
            continue;
        }
        if let Some(limit) = effective_limit {
            if rows.len() as u64 >= limit {
                truncated = true;
                break;
            }
        }
        let mut row = Vec::with_capacity(ncol as usize);
        let mut row_bytes = 0usize;
        for i in 0..ncol {
            let v = value::column_value(stmt, i);
            saw_null[i as usize] |= matches!(v, Value::Null);
            observed[i as usize] = value::merge_observed(observed[i as usize], &v);
            row_bytes += value::value_size(&v);
            row.push(v);
        }
        if let Some(max_bytes) = opts.max_bytes {
            if bytes_used + row_bytes > max_bytes as usize {
                truncated = true;
                break;
            }
        }
        bytes_used += row_bytes;
        rows.push(row);
    }

    if !has_columns {
        return Ok(None);
    }

    for (i, col) in columns.iter_mut().enumerate() {
        col.logical_type = decl_types[i].or(observed[i]).unwrap_or("null");
        col.nullable = saw_null[i];
    }
    let _ = duplicate_names; // disambiguated deterministically as name_2, name_3, …
    Ok(Some(ResultSet {
        columns,
        rows,
        truncated,
    }))
}

/// Map a SQLite failure to a structured error: message, extended code, byte
/// offset (with caret length), read-only mapping, spelling hints.
pub(crate) fn map_sqlite_error(
    conn: &Connection,
    rc: i32,
    sql: &str,
    statement_index: usize,
    offset_base: usize,
    offset_meaningful: bool,
) -> QuarryError {
    let message = conn.errmsg();
    // Authorizer denial: report our own message, not SQLite's generic one.
    if let Some(denied) = conn.take_denial() {
        let mut e = QuarryError::new(codes::READ_ONLY, denied);
        e.sqlite_code = Some(rc as i64);
        e = e.with_sql(sql, statement_index);
        return e;
    }
    // Clock guard: our overridden date functions raise a distinctive message.
    if message.contains("no clock was injected")
        || (vfs::clock().is_none() && mentions_current_time_keyword(sql))
    {
        let mut e = QuarryError::new(codes::CLOCK_REQUIRED, crate::conn::clock_required_message());
        e.sqlite_code = Some(rc as i64);
        return e.with_sql(sql, statement_index);
    }

    let mut e = QuarryError::new(codes::SQLITE, message.clone());
    e.sqlite_code = Some(rc as i64);
    unsafe {
        e.sqlite_extended = Some(ffi::sqlite3_extended_errcode(conn.db) as i64);
        if offset_meaningful {
            let off = ffi::sqlite3_error_offset(conn.db);
            if off >= 0 {
                e.offset = Some(off as i64 + offset_base as i64);
                e.length = Some(token_length(sql, (off as usize) + offset_base));
            }
        }
    }
    // "no such table: x" / "no such column: x" → spelling hint + fallback offset.
    for (prefix, known) in [
        ("no such table: ", conn.known_tables()),
        ("no such column: ", conn.known_columns()),
    ] {
        if let Some(missing) = message.strip_prefix(prefix) {
            let missing = missing.trim();
            // Qualified names ("refs.regions") — hint on the last component.
            let bare = missing.rsplit('.').next().unwrap_or(missing);
            e.hint = crate::error::spelling_hint(bare, &known);
            if e.offset.is_none() {
                if let Some((off, len)) = crate::error::find_identifier_offset(sql, bare) {
                    e.offset = Some(off);
                    e.length = Some(len);
                }
            }
        }
    }
    e.with_sql(sql, statement_index)
}

fn token_length(sql: &str, offset: usize) -> i64 {
    let bytes = sql.as_bytes();
    if offset >= bytes.len() {
        return 1;
    }
    let mut end = offset;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    ((end - offset).max(1)) as i64
}

/// Detect CURRENT_TIMESTAMP / CURRENT_DATE / CURRENT_TIME outside string
/// literals (they bypass the overridable date functions).
fn mentions_current_time_keyword(sql: &str) -> bool {
    for kw in ["current_timestamp", "current_date", "current_time"] {
        if crate::error::find_identifier_offset(sql, kw).is_some() {
            return true;
        }
    }
    false
}
