//! Connection lifecycle: open sources, enforce read-only, keep calls pure.
//!
//! Open sequence (C-6): open a `:memory:` connection on the quarry VFS, then
//! `sqlite3_deserialize(…, SQLITE_DESERIALIZE_READONLY)` each source into its
//! own attached schema. SQLite borrows our buffer (zero-copy) and its
//! battle-tested `memdb` VFS does all page I/O; the base VFS supplies only the
//! injected clock, seeded randomness and honest `xAccess`.
//!
//! Read-only is enforced in layers (defence in depth):
//! 1. `SQLITE_DESERIALIZE_READONLY` — the pager refuses writes outright;
//! 2. an authorizer that denies every write action outside the `temp` schema,
//!    with a mapped, human-readable message;
//! 3. `SQLITE_DBCONFIG_DEFENSIVE` + `TRUSTED_SCHEMA=0`.

use crate::envelope::Warning;
use crate::error::{codes, QuarryError};
use crate::ffi::{self, *};
use crate::probe::{parse_header, wal_remedy};
use crate::request::OpenMeta;
use crate::{value, vfs};
use ciborium::Value;
use core::ffi::{c_char, c_int, c_void, CStr};
use std::cell::Cell;
use std::ffi::CString;
use std::sync::{Mutex, MutexGuard};

/// The engine's global state (VFS clock/RNG cells, SQLite's internal PRNG,
/// THREADSAFE=0 build) supports exactly one live connection per process.
/// Native callers (tests, the sidecar) are serialized by this lock; in wasm
/// there is one instance and the lock is uncontended.
static ENGINE_LOCK: Mutex<()> = Mutex::new(());

fn engine_lock() -> MutexGuard<'static, ()> {
    ENGINE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Authorizer scratch state, heap-pinned so the C callback can reach it.
pub(crate) struct AuthzState {
    /// True while quarry itself runs SQL (guard transactions, introspection).
    pub internal: Cell<bool>,
    /// Message describing the most recent denial, for error mapping.
    pub denied: Cell<Option<&'static str>>,
    pub denied_detail: std::cell::RefCell<Option<String>>,
}

pub struct Connection {
    pub(crate) db: *mut ffi::sqlite3,
    /// The contiguous buffer holding all source bytes. SQLite borrows slices
    /// of it until close; it must outlive `db` (Drop closes first).
    _buf: Vec<u8>,
    pub(crate) meta: OpenMeta,
    pub(crate) warnings: Vec<Warning>,
    pub(crate) authz: Box<AuthzState>,
    pub(crate) engine_desc: String,
    /// Names of the attached sources (["main"] for the single-source case).
    pub(crate) sources: Vec<String>,
    _guard: MutexGuard<'static, ()>,
}

pub fn sqlite_version() -> String {
    // Safe: sqlite3_libversion returns a static string.
    unsafe { CStr::from_ptr(ffi::sqlite3_libversion()) }
        .to_string_lossy()
        .into_owned()
}

pub fn engine_description() -> String {
    format!("sqlite {}", sqlite_version())
}

fn valid_source_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Connection {
    /// Open from one contiguous buffer laid out as
    /// `[meta CBOR][source blob 0][source blob 1]…` with `lengths` giving each
    /// segment's size. This is the zero-copy path used by the plugin's
    /// `open_N` exports; the buffer is moved in and owned for the connection's
    /// lifetime.
    pub fn open_contiguous(mut buf: Vec<u8>, lengths: &[usize]) -> Result<Connection, QuarryError> {
        if lengths.is_empty() || lengths.iter().sum::<usize>() != buf.len() {
            return Err(QuarryError::new(
                codes::PROTOCOL,
                "open: segment lengths do not match the argument buffer",
            ));
        }
        let meta = OpenMeta::parse(&buf[..lengths[0]])?;
        let n_sources = lengths.len() - 1;
        if n_sources == 0 {
            return Err(QuarryError::new(
                codes::SOURCE_INVALID,
                "open: no database bytes were supplied",
            ));
        }

        // Resolve source names.
        let sources: Vec<String> = if meta.sources.is_empty() {
            if n_sources != 1 {
                return Err(QuarryError::new(
                    codes::PROTOCOL,
                    "open: multiple blobs require meta.sources names",
                ));
            }
            vec!["main".to_string()]
        } else {
            if meta.sources.len() != n_sources {
                return Err(QuarryError::new(
                    codes::PROTOCOL,
                    format!(
                        "open: {} source names for {} blobs",
                        meta.sources.len(),
                        n_sources
                    ),
                ));
            }
            meta.sources.clone()
        };
        {
            let mut seen = std::collections::BTreeSet::new();
            for name in &sources {
                if !valid_source_name(name) && name != "main" {
                    return Err(QuarryError::new(
                        codes::SOURCE_INVALID,
                        format!(
                            "invalid source name {name:?}: use letters, digits and underscores, starting with a letter"
                        ),
                    ));
                }
                if name == "temp" {
                    return Err(QuarryError::new(
                        codes::SOURCE_INVALID,
                        "source name \"temp\" is reserved for temporary tables",
                    ));
                }
                if !seen.insert(name.clone()) {
                    return Err(QuarryError::new(
                        codes::SOURCE_INVALID,
                        format!("duplicate source name {name:?}"),
                    ));
                }
            }
        }

        // Compute segment offsets before any mutation.
        let mut offsets = Vec::with_capacity(lengths.len());
        {
            let mut off = 0usize;
            for len in lengths {
                offsets.push(off);
                off += len;
            }
        }

        let mut warnings = Vec::new();

        // Validate headers and normalize checkpointed-WAL markers (C-7)
        // *before* opening anything.
        for (i, name) in sources.iter().enumerate() {
            let (start, len) = (offsets[i + 1], lengths[i + 1]);
            let header = parse_header(&buf[start..start + len]).map_err(|mut e| {
                e.source = Some(name.clone());
                e.message = format!("source \"{name}\": {}", e.message);
                e
            })?;
            if header.write_version == 2 {
                // The database is marked WAL. memdb cannot serve WAL, but a
                // cleanly checkpointed WAL database differs from a rollback
                // one only by these two header bytes — normalize our copy.
                buf[start + 18] = 1;
                buf[start + 19] = 1;
                warnings.push(Warning {
                    code: "wal-header-normalized".into(),
                    message: format!(
                        "source \"{name}\" is marked WAL; quarry normalized its header for read-only \
                         use. If the database was not cleanly checkpointed this can fail later — {}",
                        wal_remedy(name)
                    ),
                });
            }
        }

        let guard = engine_lock();
        unsafe {
            let rc = ffi::sqlite3_initialize();
            if rc != SQLITE_OK {
                return Err(QuarryError::new(
                    codes::OPEN_FAILED,
                    format!("sqlite3_initialize failed with code {rc}"),
                ));
            }
        }
        vfs::set_clock(meta.now_us);
        vfs::reset_rng(meta.seed);

        let mut db: *mut ffi::sqlite3 = core::ptr::null_mut();
        let vfs_name = CString::new("quarry").unwrap();
        let memory_name = CString::new(":memory:").unwrap();
        let rc = unsafe {
            ffi::sqlite3_open_v2(
                memory_name.as_ptr(),
                &mut db,
                SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_NOMUTEX,
                vfs_name.as_ptr(),
            )
        };
        if rc != SQLITE_OK || db.is_null() {
            unsafe { ffi::sqlite3_close(db) };
            return Err(QuarryError::new(
                codes::OPEN_FAILED,
                format!("could not open in-memory connection (sqlite code {rc})"),
            ));
        }

        let authz = Box::new(AuthzState {
            internal: Cell::new(true),
            denied: Cell::new(None),
            denied_detail: std::cell::RefCell::new(None),
        });

        let mut conn = Connection {
            db,
            _buf: Vec::new(), // installed below, after deserialize
            meta,
            warnings,
            authz,
            engine_desc: engine_description(),
            sources: sources.clone(),
            _guard: guard,
        };

        // Attach empty in-memory schemas for named sources, then deserialize
        // each blob into its schema.
        for (i, name) in sources.iter().enumerate() {
            if name != "main" {
                let sql = format!("ATTACH ':memory:' AS \"{name}\"");
                conn.internal_exec(&sql).map_err(|(code, msg)| {
                    QuarryError::new(
                        codes::OPEN_FAILED,
                        format!("could not attach source \"{name}\": {msg} (sqlite code {code})"),
                    )
                })?;
            }
            let (start, len) = (offsets[i + 1], lengths[i + 1]);
            let schema_c = CString::new(name.as_str()).unwrap();
            let rc = unsafe {
                ffi::sqlite3_deserialize(
                    conn.db,
                    schema_c.as_ptr(),
                    buf.as_mut_ptr().add(start),
                    len as i64,
                    len as i64,
                    SQLITE_DESERIALIZE_READONLY,
                )
            };
            if rc != SQLITE_OK {
                return Err(QuarryError::new(
                    codes::OPEN_FAILED,
                    format!("could not load source \"{name}\" (sqlite code {rc})"),
                ));
            }
        }
        // SQLite now borrows slices of `buf`; keep it alive for the
        // connection's lifetime.
        conn._buf = buf;

        conn.configure()?;

        // Early validation: read each source's schema so corruption fails at
        // open time with the source named, not deep inside a later query.
        for name in &sources {
            let sql = format!("SELECT count(*) FROM \"{name}\".sqlite_schema");
            conn.internal_exec(&sql).map_err(|(code, msg)| {
                let mut e = QuarryError::new(
                    if code == SQLITE_NOTADB || code == SQLITE_CORRUPT {
                        codes::SOURCE_INVALID
                    } else {
                        codes::OPEN_FAILED
                    },
                    format!("source \"{name}\" failed validation: {msg} (sqlite code {code})"),
                );
                e.source = Some(name.clone());
                e.sqlite_code = Some(code as i64);
                e
            })?;
        }

        // Materialize the temp database now: SQLite creates it lazily on the
        // first CREATE TEMP TABLE and it survives the purity guard's rollback,
        // so a lazily-created one would make PRAGMA database_list (and
        // anything else observing attached schemas) depend on query history —
        // a purity leak. Creating it at open time makes it part of the
        // deterministic open state instead.
        conn.internal_exec("CREATE TEMP TABLE __quarry_init(x); DROP TABLE __quarry_init")
            .map_err(|(code, msg)| {
                QuarryError::new(
                    codes::OPEN_FAILED,
                    format!("could not initialize temp storage: {msg} (sqlite code {code})"),
                )
            })?;

        // From here on, SQL comes from the document.
        conn.authz.internal.set(false);
        Ok(conn)
    }

    /// Convenience opener for native callers: named blobs are concatenated
    /// into the contiguous layout used by the plugin path.
    pub fn open(meta_bytes: &[u8], blobs: &[&[u8]]) -> Result<Connection, QuarryError> {
        let mut buf =
            Vec::with_capacity(meta_bytes.len() + blobs.iter().map(|b| b.len()).sum::<usize>());
        let mut lengths = Vec::with_capacity(1 + blobs.len());
        buf.extend_from_slice(meta_bytes);
        lengths.push(meta_bytes.len());
        for b in blobs {
            buf.extend_from_slice(b);
            lengths.push(b.len());
        }
        Connection::open_contiguous(buf, &lengths)
    }

    fn configure(&mut self) -> Result<(), QuarryError> {
        unsafe {
            let mut ignored: c_int = 0;
            ffi::sqlite3_db_config(self.db, SQLITE_DBCONFIG_DEFENSIVE, 1 as c_int, &mut ignored);
            ffi::sqlite3_db_config(
                self.db,
                SQLITE_DBCONFIG_ENABLE_TRIGGER,
                0 as c_int,
                &mut ignored,
            );
            ffi::sqlite3_db_config(
                self.db,
                SQLITE_DBCONFIG_ENABLE_VIEW,
                1 as c_int,
                &mut ignored,
            );
            ffi::sqlite3_db_config(
                self.db,
                SQLITE_DBCONFIG_TRUSTED_SCHEMA,
                0 as c_int,
                &mut ignored,
            );
            ffi::sqlite3_set_last_insert_rowid(self.db, 0);

            let state_ptr = &*self.authz as *const AuthzState as *mut c_void;
            ffi::sqlite3_set_authorizer(self.db, Some(authorizer_cb), state_ptr);
        }
        // No injected clock: make every date/time function fail loudly with an
        // actionable message instead of silently misbehaving (F-03 / D-04).
        if self.meta.now_us.is_none() {
            for name in [
                "date",
                "time",
                "datetime",
                "julianday",
                "unixepoch",
                "strftime",
                "timediff",
            ] {
                let name_c = CString::new(name).unwrap();
                let rc = unsafe {
                    ffi::sqlite3_create_function_v2(
                        self.db,
                        name_c.as_ptr(),
                        -1,
                        SQLITE_UTF8 | SQLITE_DIRECTONLY,
                        core::ptr::null_mut(),
                        Some(clock_required_cb),
                        None,
                        None,
                        None,
                    )
                };
                if rc != SQLITE_OK {
                    return Err(QuarryError::new(
                        codes::OPEN_FAILED,
                        format!("could not install clock guard for {name} (sqlite code {rc})"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Run internal SQL with the authorizer disarmed. Never for document SQL.
    pub(crate) fn internal_exec(&self, sql: &str) -> Result<(), (c_int, String)> {
        let was_internal = self.authz.internal.replace(true);
        let sql_c = CString::new(sql).map_err(|_| (SQLITE_MISUSE, "NUL in SQL".to_string()))?;
        let rc = unsafe {
            ffi::sqlite3_exec(
                self.db,
                sql_c.as_ptr(),
                None,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            )
        };
        self.authz.internal.set(was_internal);
        if rc == SQLITE_OK {
            Ok(())
        } else {
            Err((rc, self.errmsg()))
        }
    }

    /// Run internal SQL and collect all rows as CBOR values.
    pub(crate) fn internal_rows(&self, sql: &str) -> Result<Vec<Vec<Value>>, (c_int, String)> {
        let was_internal = self.authz.internal.replace(true);
        let result = (|| {
            let sql_c = CString::new(sql).map_err(|_| (SQLITE_MISUSE, "NUL in SQL".to_string()))?;
            let mut stmt: *mut ffi::sqlite3_stmt = core::ptr::null_mut();
            let rc = unsafe {
                ffi::sqlite3_prepare_v2(
                    self.db,
                    sql_c.as_ptr(),
                    -1,
                    &mut stmt,
                    core::ptr::null_mut(),
                )
            };
            if rc != SQLITE_OK {
                return Err((rc, self.errmsg()));
            }
            let mut rows = Vec::new();
            unsafe {
                let ncol = ffi::sqlite3_column_count(stmt);
                loop {
                    let rc = ffi::sqlite3_step(stmt);
                    if rc == SQLITE_ROW {
                        let mut row = Vec::with_capacity(ncol as usize);
                        for i in 0..ncol {
                            row.push(value::column_value(stmt, i));
                        }
                        rows.push(row);
                    } else if rc == SQLITE_DONE {
                        break;
                    } else {
                        let err = (rc, self.errmsg());
                        ffi::sqlite3_finalize(stmt);
                        return Err(err);
                    }
                }
                ffi::sqlite3_finalize(stmt);
            }
            Ok(rows)
        })();
        self.authz.internal.set(was_internal);
        result
    }

    pub(crate) fn errmsg(&self) -> String {
        unsafe {
            CStr::from_ptr(ffi::sqlite3_errmsg(self.db))
                .to_string_lossy()
                .into_owned()
        }
    }

    /// All table and view names across every source (for spelling hints).
    pub(crate) fn known_tables(&self) -> Vec<String> {
        let mut names = Vec::new();
        for src in &self.sources {
            if let Ok(rows) = self.internal_rows(&format!(
                "SELECT name FROM \"{src}\".sqlite_schema WHERE type IN ('table','view')"
            )) {
                for row in rows {
                    if let Some(Value::Text(name)) = row.into_iter().next() {
                        names.push(name);
                    }
                }
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// All column names across every table of every source (for hints).
    pub(crate) fn known_columns(&self) -> Vec<String> {
        let mut names = Vec::new();
        for src in &self.sources {
            if let Ok(tables) = self.internal_rows(&format!(
                "SELECT name FROM \"{src}\".sqlite_schema WHERE type IN ('table','view')"
            )) {
                for row in tables {
                    if let Some(Value::Text(table)) = row.into_iter().next() {
                        let escaped = table.replace('\'', "''");
                        if let Ok(cols) = self.internal_rows(&format!(
                            "SELECT name FROM pragma_table_info('{escaped}', '{src}')"
                        )) {
                            for col in cols {
                                if let Some(Value::Text(name)) = col.into_iter().next() {
                                    names.push(name);
                                }
                            }
                        }
                    }
                }
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// Take (and clear) the last authorizer denial, if any.
    pub(crate) fn take_denial(&self) -> Option<String> {
        self.authz.denied.set(None);
        self.authz.denied_detail.borrow_mut().take()
    }
}

impl core::fmt::Debug for Connection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Connection")
            .field("sources", &self.sources)
            .field("engine", &self.engine_desc)
            .finish_non_exhaustive()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            // close_v2 defers if any statement is somehow still live; never
            // finalize statements found via sqlite3_next_stmt — they may be
            // owned by extension modules (e.g. FTS5), which finalize their own
            // on disconnect. The source buffer drops after close.
            ffi::sqlite3_close_v2(self.db);
        }
    }
}

// -- The read-only authorizer ------------------------------------------------

const READONLY_PRAGMAS_ARG_OK: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "integrity_check",
    "quick_check",
];

const READONLY_PRAGMAS_NO_ARG: &[&str] = &[
    "application_id",
    "collation_list",
    "compile_options",
    "data_version",
    "database_list",
    "encoding",
    "freelist_count",
    "function_list",
    "journal_mode",
    "module_list",
    "page_count",
    "page_size",
    "pragma_list",
    "schema_version",
    "user_version",
];

unsafe fn cstr_opt<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        None
    } else {
        CStr::from_ptr(p).to_str().ok()
    }
}

unsafe extern "C" fn authorizer_cb(
    user: *mut c_void,
    action: c_int,
    a1: *const c_char,
    a2: *const c_char,
    a3: *const c_char,
    _a4: *const c_char,
) -> c_int {
    let state = &*(user as *const AuthzState);
    if state.internal.get() {
        return SQLITE_OK;
    }

    let deny = |verb: String| -> c_int {
        state.denied.set(Some("denied"));
        *state.denied_detail.borrow_mut() = Some(verb);
        SQLITE_DENY
    };
    let db_name = cstr_opt(a3);
    let in_temp = db_name == Some("temp");

    match action {
        SQLITE_SELECT | SQLITE_READ | SQLITE_FUNCTION | SQLITE_RECURSIVE => SQLITE_OK,
        SQLITE_PRAGMA => {
            let name = cstr_opt(a1).unwrap_or("").to_ascii_lowercase();
            let has_arg = !a2.is_null();
            if READONLY_PRAGMAS_ARG_OK.contains(&name.as_str())
                || (!has_arg && READONLY_PRAGMAS_NO_ARG.contains(&name.as_str()))
            {
                SQLITE_OK
            } else if has_arg {
                deny(format!(
                    "quarry is read-only: PRAGMA {name} with an argument changes engine state and is not permitted"
                ))
            } else {
                deny(format!(
                    "quarry is read-only: PRAGMA {name} is not in the allowed read-only set"
                ))
            }
        }
        // Temporary objects are the sanctioned scratch space for
        // multi-statement scripts; everything lives in memory and is rolled
        // back by the purity guard.
        SQLITE_CREATE_TEMP_TABLE | SQLITE_CREATE_TEMP_VIEW | SQLITE_CREATE_TEMP_INDEX
        | SQLITE_DROP_TEMP_TABLE | SQLITE_DROP_TEMP_VIEW | SQLITE_DROP_TEMP_INDEX => SQLITE_OK,
        SQLITE_CREATE_TEMP_TRIGGER | SQLITE_DROP_TEMP_TRIGGER => {
            deny("quarry: triggers are disabled (trigger execution is off in this sandbox)".into())
        }
        SQLITE_CREATE_VTABLE | SQLITE_DROP_VTABLE => {
            if in_temp {
                SQLITE_OK
            } else {
                deny(format!(
                    "quarry is read-only: CREATE VIRTUAL TABLE is only permitted in the temp schema (got {})",
                    db_name.unwrap_or("?")
                ))
            }
        }
        SQLITE_INSERT | SQLITE_UPDATE | SQLITE_DELETE => {
            if in_temp {
                SQLITE_OK
            } else {
                let table = cstr_opt(a1).unwrap_or("?");
                if table.starts_with("sqlite_") {
                    // DDL statements surface here first as writes to the
                    // schema table; name the real operation, not the plumbing.
                    deny(
                        "quarry is read-only: modifying the schema (CREATE/DROP/ALTER outside temp) is not permitted"
                            .into(),
                    )
                } else {
                    let verb = match action {
                        SQLITE_INSERT => "INSERT",
                        SQLITE_UPDATE => "UPDATE",
                        _ => "DELETE",
                    };
                    deny(format!(
                        "quarry is read-only: {verb} on {}.{table} is not permitted (temp tables are allowed)",
                        db_name.unwrap_or("?")
                    ))
                }
            }
        }
        SQLITE_TRANSACTION | SQLITE_SAVEPOINT => deny(
            "quarry manages transactions itself: BEGIN/COMMIT/ROLLBACK/SAVEPOINT are not permitted in document SQL"
                .into(),
        ),
        SQLITE_ATTACH | SQLITE_DETACH => deny(
            "quarry: ATTACH/DETACH are not permitted — declare additional sources when opening the database"
                .into(),
        ),
        SQLITE_CREATE_TABLE | SQLITE_CREATE_VIEW | SQLITE_CREATE_INDEX | SQLITE_CREATE_TRIGGER
        | SQLITE_DROP_TABLE | SQLITE_DROP_VIEW | SQLITE_DROP_INDEX | SQLITE_DROP_TRIGGER
        | SQLITE_ALTER_TABLE | SQLITE_REINDEX | SQLITE_ANALYZE if !in_temp => {
            let what = match action {
                SQLITE_ALTER_TABLE => "ALTER TABLE",
                SQLITE_REINDEX => "REINDEX",
                SQLITE_ANALYZE => "ANALYZE",
                a if (SQLITE_CREATE_INDEX..=SQLITE_CREATE_VIEW).contains(&a) => "CREATE",
                _ => "DROP",
            };
            deny(format!(
                "quarry is read-only: {what} outside the temp schema is not permitted"
            ))
        }
        SQLITE_CREATE_TABLE | SQLITE_CREATE_VIEW | SQLITE_CREATE_INDEX
        | SQLITE_DROP_TABLE | SQLITE_DROP_VIEW | SQLITE_DROP_INDEX => SQLITE_OK, // in_temp
        SQLITE_CREATE_TRIGGER | SQLITE_DROP_TRIGGER | SQLITE_ALTER_TABLE | SQLITE_REINDEX
        | SQLITE_ANALYZE => deny(
            "quarry is read-only: triggers, ALTER, REINDEX and ANALYZE are not permitted even in temp".into(),
        ),
        _ => deny(format!(
            "quarry is read-only: operation (authorizer action {action}) is not permitted"
        )),
    }
}

const CLOCK_REQUIRED_MSG: &str = "quarry: this query uses date/time functions but no clock was injected. \
Pass now: datetime.today() (or an explicit datetime) to sqlite(), or set --input quarry-now=<ISO-8601>";

unsafe extern "C" fn clock_required_cb(
    ctx: *mut ffi::sqlite3_context,
    _argc: c_int,
    _argv: *mut *mut ffi::sqlite3_value,
) {
    ffi::sqlite3_result_error(
        ctx,
        CLOCK_REQUIRED_MSG.as_ptr() as *const c_char,
        CLOCK_REQUIRED_MSG.len() as c_int,
    );
}

pub(crate) fn clock_required_message() -> &'static str {
    CLOCK_REQUIRED_MSG
}
