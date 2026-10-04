//! Hand-written FFI surface over the vendored SQLite amalgamation.
//!
//! Deliberately minimal: only the ~40 entry points the engine actually uses.
//! No abstraction is attempted here; safe wrappers live in the sibling modules.

#![allow(non_camel_case_types, non_snake_case, dead_code)]
#![allow(clippy::missing_safety_doc)] // raw SQLite ABI: safety contracts are SQLite's documented C API

use core::ffi::{c_char, c_int, c_uchar, c_void};

pub enum sqlite3 {}
pub enum sqlite3_stmt {}
pub enum sqlite3_context {}
pub enum sqlite3_value {}

pub type sqlite3_int64 = i64;
pub type sqlite3_uint64 = u64;

// -- Result codes -----------------------------------------------------------
pub const SQLITE_OK: c_int = 0;
pub const SQLITE_ERROR: c_int = 1;
pub const SQLITE_BUSY: c_int = 5;
pub const SQLITE_NOMEM: c_int = 7;
pub const SQLITE_READONLY: c_int = 8;
pub const SQLITE_IOERR: c_int = 10;
pub const SQLITE_CORRUPT: c_int = 11;
pub const SQLITE_NOTFOUND: c_int = 12;
pub const SQLITE_CANTOPEN: c_int = 14;
pub const SQLITE_MISUSE: c_int = 21;
pub const SQLITE_AUTH: c_int = 23;
pub const SQLITE_ROW: c_int = 100;
pub const SQLITE_DONE: c_int = 101;
pub const SQLITE_NOTADB: c_int = 26;

// -- Open flags -------------------------------------------------------------
pub const SQLITE_OPEN_READONLY: c_int = 0x0000_0001;
pub const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
pub const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;
pub const SQLITE_OPEN_MEMORY: c_int = 0x0000_0080;
pub const SQLITE_OPEN_NOMUTEX: c_int = 0x0000_8000;

// -- Column / value types ---------------------------------------------------
pub const SQLITE_INTEGER: c_int = 1;
pub const SQLITE_FLOAT: c_int = 2;
pub const SQLITE_TEXT: c_int = 3;
pub const SQLITE_BLOB: c_int = 4;
pub const SQLITE_NULL: c_int = 5;

// -- Deserialize flags ------------------------------------------------------
pub const SQLITE_DESERIALIZE_FREEONCLOSE: c_uint = 1;
pub const SQLITE_DESERIALIZE_RESIZEABLE: c_uint = 2;
pub const SQLITE_DESERIALIZE_READONLY: c_uint = 4;
pub type c_uint = core::ffi::c_uint;

// -- Authorizer action codes ------------------------------------------------
pub const SQLITE_CREATE_INDEX: c_int = 1;
pub const SQLITE_CREATE_TABLE: c_int = 2;
pub const SQLITE_CREATE_TEMP_INDEX: c_int = 3;
pub const SQLITE_CREATE_TEMP_TABLE: c_int = 4;
pub const SQLITE_CREATE_TEMP_TRIGGER: c_int = 5;
pub const SQLITE_CREATE_TEMP_VIEW: c_int = 6;
pub const SQLITE_CREATE_TRIGGER: c_int = 7;
pub const SQLITE_CREATE_VIEW: c_int = 8;
pub const SQLITE_DELETE: c_int = 9;
pub const SQLITE_DROP_INDEX: c_int = 10;
pub const SQLITE_DROP_TABLE: c_int = 11;
pub const SQLITE_DROP_TEMP_INDEX: c_int = 12;
pub const SQLITE_DROP_TEMP_TABLE: c_int = 13;
pub const SQLITE_DROP_TEMP_TRIGGER: c_int = 14;
pub const SQLITE_DROP_TEMP_VIEW: c_int = 15;
pub const SQLITE_DROP_TRIGGER: c_int = 16;
pub const SQLITE_DROP_VIEW: c_int = 17;
pub const SQLITE_INSERT: c_int = 18;
pub const SQLITE_PRAGMA: c_int = 19;
pub const SQLITE_READ: c_int = 20;
pub const SQLITE_SELECT: c_int = 21;
pub const SQLITE_TRANSACTION: c_int = 22;
pub const SQLITE_UPDATE: c_int = 23;
pub const SQLITE_ATTACH: c_int = 24;
pub const SQLITE_DETACH: c_int = 25;
pub const SQLITE_ALTER_TABLE: c_int = 26;
pub const SQLITE_REINDEX: c_int = 27;
pub const SQLITE_ANALYZE: c_int = 28;
pub const SQLITE_CREATE_VTABLE: c_int = 29;
pub const SQLITE_DROP_VTABLE: c_int = 30;
pub const SQLITE_FUNCTION: c_int = 31;
pub const SQLITE_SAVEPOINT: c_int = 32;
pub const SQLITE_RECURSIVE: c_int = 33;

pub const SQLITE_DENY: c_int = 1;
pub const SQLITE_IGNORE: c_int = 2;

// -- db_config ops ----------------------------------------------------------
pub const SQLITE_DBCONFIG_ENABLE_TRIGGER: c_int = 1003;
pub const SQLITE_DBCONFIG_ENABLE_VIEW: c_int = 1015;
pub const SQLITE_DBCONFIG_DEFENSIVE: c_int = 1010;
pub const SQLITE_DBCONFIG_TRUSTED_SCHEMA: c_int = 1017;

// -- Limits -----------------------------------------------------------------
pub const SQLITE_LIMIT_LENGTH: c_int = 0;
pub const SQLITE_LIMIT_SQL_LENGTH: c_int = 1;
pub const SQLITE_LIMIT_COLUMN: c_int = 2;
pub const SQLITE_LIMIT_EXPR_DEPTH: c_int = 3;
pub const SQLITE_LIMIT_VDBE_OP: c_int = 5;
pub const SQLITE_LIMIT_ATTACHED: c_int = 7;

// -- Text encodings / destructors -------------------------------------------
pub const SQLITE_UTF8: c_int = 1;
pub const SQLITE_DETERMINISTIC: c_int = 0x0000_0800;
pub const SQLITE_DIRECTONLY: c_int = 0x0008_0000;

/// SQLITE_TRANSIENT — tells SQLite to make its own copy of bound text/blobs.
pub const SQLITE_TRANSIENT: isize = -1;

// -- VFS structures ---------------------------------------------------------
// Layouts must match sqlite3.h exactly (iVersion 3 struct).
#[repr(C)]
pub struct sqlite3_vfs {
    pub iVersion: c_int,
    pub szOsFile: c_int,
    pub mxPathname: c_int,
    pub pNext: *mut sqlite3_vfs,
    pub zName: *const c_char,
    pub pAppData: *mut c_void,
    pub xOpen: Option<
        unsafe extern "C" fn(
            *mut sqlite3_vfs,
            *const c_char,
            *mut sqlite3_file,
            c_int,
            *mut c_int,
        ) -> c_int,
    >,
    pub xDelete: Option<unsafe extern "C" fn(*mut sqlite3_vfs, *const c_char, c_int) -> c_int>,
    pub xAccess:
        Option<unsafe extern "C" fn(*mut sqlite3_vfs, *const c_char, c_int, *mut c_int) -> c_int>,
    pub xFullPathname:
        Option<unsafe extern "C" fn(*mut sqlite3_vfs, *const c_char, c_int, *mut c_char) -> c_int>,
    pub xDlOpen: Option<unsafe extern "C" fn(*mut sqlite3_vfs, *const c_char) -> *mut c_void>,
    pub xDlError: Option<unsafe extern "C" fn(*mut sqlite3_vfs, c_int, *mut c_char)>,
    pub xDlSym: Option<
        unsafe extern "C" fn(
            *mut sqlite3_vfs,
            *mut c_void,
            *const c_char,
        ) -> Option<unsafe extern "C" fn()>,
    >,
    pub xDlClose: Option<unsafe extern "C" fn(*mut sqlite3_vfs, *mut c_void)>,
    pub xRandomness: Option<unsafe extern "C" fn(*mut sqlite3_vfs, c_int, *mut c_char) -> c_int>,
    pub xSleep: Option<unsafe extern "C" fn(*mut sqlite3_vfs, c_int) -> c_int>,
    pub xCurrentTime: Option<unsafe extern "C" fn(*mut sqlite3_vfs, *mut f64) -> c_int>,
    pub xGetLastError: Option<unsafe extern "C" fn(*mut sqlite3_vfs, c_int, *mut c_char) -> c_int>,
    // iVersion >= 2
    pub xCurrentTimeInt64:
        Option<unsafe extern "C" fn(*mut sqlite3_vfs, *mut sqlite3_int64) -> c_int>,
    // iVersion >= 3
    pub xSetSystemCall: Option<
        unsafe extern "C" fn(
            *mut sqlite3_vfs,
            *const c_char,
            Option<unsafe extern "C" fn()>,
        ) -> c_int,
    >,
    pub xGetSystemCall: Option<
        unsafe extern "C" fn(*mut sqlite3_vfs, *const c_char) -> Option<unsafe extern "C" fn()>,
    >,
    pub xNextSystemCall:
        Option<unsafe extern "C" fn(*mut sqlite3_vfs, *const c_char) -> *const c_char>,
}

#[repr(C)]
pub struct sqlite3_file {
    pub pMethods: *const c_void,
}

pub const SQLITE_ACCESS_EXISTS: c_int = 0;

extern "C" {
    pub fn sqlite3_initialize() -> c_int;
    pub fn sqlite3_libversion() -> *const c_char;

    pub fn sqlite3_open_v2(
        filename: *const c_char,
        db: *mut *mut sqlite3,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    pub fn sqlite3_close(db: *mut sqlite3) -> c_int;
    pub fn sqlite3_close_v2(db: *mut sqlite3) -> c_int;

    pub fn sqlite3_deserialize(
        db: *mut sqlite3,
        schema: *const c_char,
        data: *mut c_uchar,
        db_size: sqlite3_int64,
        buf_size: sqlite3_int64,
        flags: c_uint,
    ) -> c_int;

    pub fn sqlite3_exec(
        db: *mut sqlite3,
        sql: *const c_char,
        callback: Option<
            unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int,
        >,
        arg: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;

    pub fn sqlite3_prepare_v2(
        db: *mut sqlite3,
        sql: *const c_char,
        n_byte: c_int,
        stmt: *mut *mut sqlite3_stmt,
        tail: *mut *const c_char,
    ) -> c_int;
    pub fn sqlite3_step(stmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_finalize(stmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_reset(stmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_stmt_readonly(stmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_next_stmt(db: *mut sqlite3, stmt: *mut sqlite3_stmt) -> *mut sqlite3_stmt;

    pub fn sqlite3_column_count(stmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_column_name(stmt: *mut sqlite3_stmt, i: c_int) -> *const c_char;
    pub fn sqlite3_column_type(stmt: *mut sqlite3_stmt, i: c_int) -> c_int;
    pub fn sqlite3_column_int64(stmt: *mut sqlite3_stmt, i: c_int) -> sqlite3_int64;
    pub fn sqlite3_column_double(stmt: *mut sqlite3_stmt, i: c_int) -> f64;
    pub fn sqlite3_column_text(stmt: *mut sqlite3_stmt, i: c_int) -> *const c_uchar;
    pub fn sqlite3_column_blob(stmt: *mut sqlite3_stmt, i: c_int) -> *const c_void;
    pub fn sqlite3_column_bytes(stmt: *mut sqlite3_stmt, i: c_int) -> c_int;
    pub fn sqlite3_column_decltype(stmt: *mut sqlite3_stmt, i: c_int) -> *const c_char;
    pub fn sqlite3_column_table_name(stmt: *mut sqlite3_stmt, i: c_int) -> *const c_char;
    pub fn sqlite3_column_origin_name(stmt: *mut sqlite3_stmt, i: c_int) -> *const c_char;
    pub fn sqlite3_column_database_name(stmt: *mut sqlite3_stmt, i: c_int) -> *const c_char;

    pub fn sqlite3_bind_parameter_count(stmt: *mut sqlite3_stmt) -> c_int;
    pub fn sqlite3_bind_parameter_name(stmt: *mut sqlite3_stmt, i: c_int) -> *const c_char;
    pub fn sqlite3_bind_int64(stmt: *mut sqlite3_stmt, i: c_int, v: sqlite3_int64) -> c_int;
    pub fn sqlite3_bind_double(stmt: *mut sqlite3_stmt, i: c_int, v: f64) -> c_int;
    pub fn sqlite3_bind_text(
        stmt: *mut sqlite3_stmt,
        i: c_int,
        v: *const c_char,
        n: c_int,
        destructor: isize,
    ) -> c_int;
    pub fn sqlite3_bind_blob(
        stmt: *mut sqlite3_stmt,
        i: c_int,
        v: *const c_void,
        n: c_int,
        destructor: isize,
    ) -> c_int;
    pub fn sqlite3_bind_null(stmt: *mut sqlite3_stmt, i: c_int) -> c_int;

    pub fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;
    pub fn sqlite3_errcode(db: *mut sqlite3) -> c_int;
    pub fn sqlite3_extended_errcode(db: *mut sqlite3) -> c_int;
    pub fn sqlite3_error_offset(db: *mut sqlite3) -> c_int;

    pub fn sqlite3_set_authorizer(
        db: *mut sqlite3,
        callback: Option<
            unsafe extern "C" fn(
                *mut c_void,
                c_int,
                *const c_char,
                *const c_char,
                *const c_char,
                *const c_char,
            ) -> c_int,
        >,
        user_data: *mut c_void,
    ) -> c_int;

    pub fn sqlite3_db_config(db: *mut sqlite3, op: c_int, ...) -> c_int;
    pub fn sqlite3_limit(db: *mut sqlite3, id: c_int, new_val: c_int) -> c_int;

    pub fn sqlite3_create_function_v2(
        db: *mut sqlite3,
        name: *const c_char,
        n_arg: c_int,
        flags: c_int,
        user_data: *mut c_void,
        x_func: Option<unsafe extern "C" fn(*mut sqlite3_context, c_int, *mut *mut sqlite3_value)>,
        x_step: Option<unsafe extern "C" fn(*mut sqlite3_context, c_int, *mut *mut sqlite3_value)>,
        x_final: Option<unsafe extern "C" fn(*mut sqlite3_context)>,
        destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int;
    pub fn sqlite3_result_error(ctx: *mut sqlite3_context, msg: *const c_char, n: c_int);

    pub fn sqlite3_randomness(n: c_int, out: *mut c_void);
    pub fn sqlite3_set_last_insert_rowid(db: *mut sqlite3, rowid: sqlite3_int64);
    pub fn sqlite3_db_release_memory(db: *mut sqlite3) -> c_int;
    pub fn sqlite3_vfs_register(vfs: *mut sqlite3_vfs, make_default: c_int) -> c_int;
    pub fn sqlite3_free(ptr: *mut c_void);
}
