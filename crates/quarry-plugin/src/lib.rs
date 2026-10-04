//! The Typst WASM plugin: a thin protocol shim over quarry-engine.
//!
//! Protocol (wasm-minimal-protocol, implemented by hand for full control):
//! - every export takes one i32 *byte length* per argument;
//! - the host writes all argument bytes, concatenated, into a buffer we
//!   provide via `wasm_minimal_protocol_write_args_to_buffer`;
//! - we send the result via `wasm_minimal_protocol_send_result_to_host`;
//! - return 0 ⇒ result is data, return 1 ⇒ result is an error message.
//!
//! Contract decisions that matter (see the plan):
//! - `query`/`describe`/`schema` ALWAYS return 0 with an ok/err envelope —
//!   SQL errors are data, which is what makes `try-query` possible (D-17).
//!   Return code 1 is reserved for protocol violations and `open_N` failures
//!   (where `plugin.transition` discards the result buffer anyway).
//! - `open_N` runs under `plugin.transition()`: the connection lives in this
//!   module's linear memory and is captured by Typst's snapshot (C-3).
//! - Typst never calls `_initialize` (C-5): every export runs the C
//!   constructors once via `ensure_ctors` before touching SQLite.

#![cfg(target_arch = "wasm32")]

use quarry_engine::{envelope, error::codes, error::QuarryError, Connection};

#[link(wasm_import_module = "typst_env")]
extern "C" {
    fn wasm_minimal_protocol_send_result_to_host(ptr: *const u8, len: usize);
    fn wasm_minimal_protocol_write_args_to_buffer(ptr: *mut u8);
}

extern "C" {
    fn __wasm_call_ctors();
}

// Plain statics live in linear memory, so plugin.transition()'s snapshot
// captures them and pooled-instance reuse sees a consistent state.
static mut CTORS_DONE: bool = false;
static mut CONNECTION: Option<Connection> = None;

fn ensure_ctors() {
    unsafe {
        if !CTORS_DONE {
            CTORS_DONE = true;
            __wasm_call_ctors();
        }
    }
}

fn read_args(total: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(total);
    unsafe {
        wasm_minimal_protocol_write_args_to_buffer(buf.as_mut_ptr());
        buf.set_len(total);
    }
    buf
}

fn send(bytes: &[u8]) {
    unsafe { wasm_minimal_protocol_send_result_to_host(bytes.as_ptr(), bytes.len()) }
}

fn send_ok(bytes: &[u8]) -> i32 {
    send(bytes);
    0
}

fn send_hard_error(message: &str) -> i32 {
    send(message.as_bytes());
    1
}

fn engine_desc() -> String {
    quarry_engine::engine_description()
}

unsafe fn with_connection(f: impl FnOnce(&Connection) -> Vec<u8>) -> i32 {
    match &*core::ptr::addr_of!(CONNECTION) {
        Some(conn) => send_ok(&f(conn)),
        None => {
            let e = QuarryError::new(
                codes::PROTOCOL,
                "no database is open on this module — call qr.sqlite(...) and use the handle it returns",
            );
            send_ok(&envelope::error_envelope(&e, &engine_desc()))
        }
    }
}

// -- module info --------------------------------------------------------------

#[no_mangle]
pub extern "C" fn abi_version() -> i32 {
    ensure_ctors();
    send_ok(&envelope::value_envelope(&ciborium::Value::from(1u64)))
}

#[no_mangle]
pub extern "C" fn capabilities() -> i32 {
    ensure_ctors();
    send_ok(&quarry_engine::capabilities_bytes())
}

// -- opening (called via plugin.transition) -----------------------------------

fn do_open(buf: Vec<u8>, lengths: &[usize]) -> i32 {
    unsafe {
        // A pooled instance may still hold a previous connection; a fresh
        // open must not leak it (and must reset engine globals through the
        // normal open path).
        *core::ptr::addr_of_mut!(CONNECTION) = None;
        match Connection::open_contiguous(buf, lengths) {
            Ok(conn) => {
                *core::ptr::addr_of_mut!(CONNECTION) = Some(conn);
                send_ok(b"")
            }
            Err(e) => {
                // plugin.transition() discards result buffers, so failure must
                // be a hard error; the message carries the structured info.
                let hint = e.hint.as_deref().unwrap_or("");
                send_hard_error(&format!(
                    "quarry: {}{}{}",
                    e.message,
                    if hint.is_empty() { "" } else { " — " },
                    hint,
                ))
            }
        }
    }
}

macro_rules! open_export {
    ($name:ident, $($len:ident),+) => {
        #[no_mangle]
        pub extern "C" fn $name($($len: usize),+) -> i32 {
            ensure_ctors();
            let lengths = [$($len),+];
            let total: usize = lengths.iter().sum();
            let buf = read_args(total);
            do_open(buf, &lengths)
        }
    };
}

open_export!(open_1, a0, a1);
open_export!(open_2, a0, a1, a2);
open_export!(open_3, a0, a1, a2, a3);
open_export!(open_4, a0, a1, a2, a3, a4);
open_export!(open_5, a0, a1, a2, a3, a4, a5);
open_export!(open_6, a0, a1, a2, a3, a4, a5, a6);
open_export!(open_7, a0, a1, a2, a3, a4, a5, a6, a7);
open_export!(open_8, a0, a1, a2, a3, a4, a5, a6, a7, a8);

#[no_mangle]
pub extern "C" fn close() -> i32 {
    ensure_ctors();
    unsafe {
        *core::ptr::addr_of_mut!(CONNECTION) = None;
    }
    send_ok(b"")
}

// -- queries (called on the transitioned module) ------------------------------

#[no_mangle]
pub extern "C" fn query(len: usize) -> i32 {
    ensure_ctors();
    let request = read_args(len);
    unsafe { with_connection(|conn| conn.query(&request)) }
}

#[no_mangle]
pub extern "C" fn describe(len: usize) -> i32 {
    ensure_ctors();
    let request = read_args(len);
    unsafe { with_connection(|conn| conn.describe(&request)) }
}

#[no_mangle]
pub extern "C" fn schema(len: usize) -> i32 {
    ensure_ctors();
    let arg = read_args(len);
    // Argument: CBOR bool — include row counts? (empty ⇒ true)
    let row_counts = if arg.is_empty() {
        true
    } else {
        ciborium::from_reader::<ciborium::Value, _>(arg.as_slice())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    };
    unsafe { with_connection(|conn| conn.schema(row_counts)) }
}

// -- stateless helpers (work on the base module, no open needed) --------------

#[no_mangle]
pub extern "C" fn probe(len: usize) -> i32 {
    ensure_ctors();
    let blob = read_args(len);
    send_ok(&quarry_engine::probe_bytes(&blob))
}

#[no_mangle]
pub extern "C" fn cache_key(len: usize) -> i32 {
    ensure_ctors();
    let request = read_args(len);
    match quarry_engine::cache_key_bytes(&request) {
        Ok(bytes) => send_ok(&bytes),
        Err(e) => send_ok(&envelope::error_envelope(&e, &engine_desc())),
    }
}

#[no_mangle]
pub extern "C" fn normalize_sql(len: usize) -> i32 {
    ensure_ctors();
    let sql = read_args(len);
    let text = String::from_utf8_lossy(&sql);
    send_ok(&quarry_engine::normalize_sql_bytes(&text))
}
