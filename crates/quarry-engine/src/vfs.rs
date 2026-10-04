//! The quarry base VFS: injected clock, seeded randomness, honest `xAccess`.
//!
//! All page I/O goes through SQLite's built-in `memdb` VFS (loaded via
//! `sqlite3_deserialize`), so this VFS never reads or writes a byte of data.
//! Its entire job is to answer the three environment questions SQLite asks —
//! what time is it, give me randomness, does file X exist — *deterministically*:
//!
//! - **Time** (D-04): there is no wall clock in a pure plugin. The compile
//!   timestamp is injected by the caller; using a time-dependent SQL function
//!   without injecting one is a hard error, never a silent 1970.
//! - **Randomness** (D-05): a ChaCha8 stream keyed by the injected seed. Reset
//!   at the start of every query so results are independent of query order.
//! - **Existence** (D-21): nothing exists. Journals and WAL files are never
//!   reported present, so SQLite can never wander into hot-journal recovery.

use crate::ffi::*;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha8;
use core::ffi::{c_char, c_int};
use std::sync::Mutex;

/// Difference between the Unix epoch and the Julian-day epoch, in milliseconds.
/// (Julian day 0 is -4713-11-24T12:00Z; SQLite's xCurrentTimeInt64 wants
/// milliseconds since that epoch.)
const JULIAN_UNIX_OFFSET_MS: i64 = 210_866_760_000_000;

struct VfsState {
    /// Injected compile timestamp in Unix *microseconds*, if any.
    clock_us: Option<i64>,
    /// Seeded random stream. Reset per query for order-independence.
    rng: Option<ChaCha8>,
    seed: i64,
}

static STATE: Mutex<VfsState> = Mutex::new(VfsState {
    clock_us: None,
    rng: None,
    seed: 0,
});

/// Install the injected clock (Unix microseconds) for the current connection.
pub fn set_clock(clock_us: Option<i64>) {
    STATE.lock().unwrap().clock_us = clock_us;
}

pub fn clock() -> Option<i64> {
    STATE.lock().unwrap().clock_us
}

/// Set the seed and (re)start the random stream from its beginning.
pub fn reset_rng(seed: i64) {
    let key = blake3::hash(&seed.to_le_bytes());
    let cipher = ChaCha8::new(key.as_bytes().into(), &[0u8; 12].into());
    let mut st = STATE.lock().unwrap();
    st.rng = Some(cipher);
    st.seed = seed;
}

/// Restart the random stream from the beginning of the current seed's stream.
/// Called by the purity guard at the start of every query, so `random()`
/// results do not depend on how many queries ran before this one.
pub fn rewind_rng() {
    let seed = STATE.lock().unwrap().seed;
    reset_rng(seed);
}

fn fill_random(buf: &mut [u8]) {
    let mut st = STATE.lock().unwrap();
    buf.fill(0);
    if let Some(rng) = st.rng.as_mut() {
        rng.apply_keystream(buf);
    }
    // If no seed was ever installed the buffer stays zeroed — but Connection
    // always installs one (default 0), so this path is unreachable in practice.
}

// -- VFS callbacks -----------------------------------------------------------

unsafe extern "C" fn x_open(
    _vfs: *mut sqlite3_vfs,
    _name: *const c_char,
    _file: *mut sqlite3_file,
    _flags: c_int,
    _out_flags: *mut c_int,
) -> c_int {
    // Nothing should ever reach the base VFS's file layer: databases arrive
    // via sqlite3_deserialize (memdb) and temp storage is in-memory.
    SQLITE_CANTOPEN
}

unsafe extern "C" fn x_delete(
    _vfs: *mut sqlite3_vfs,
    _name: *const c_char,
    _sync_dir: c_int,
) -> c_int {
    SQLITE_OK
}

unsafe extern "C" fn x_access(
    _vfs: *mut sqlite3_vfs,
    _name: *const c_char,
    _flags: c_int,
    out: *mut c_int,
) -> c_int {
    // D-21: answer honestly. No file — journal, WAL, superjournal, anything —
    // exists in this environment.
    if !out.is_null() {
        *out = 0;
    }
    SQLITE_OK
}

unsafe extern "C" fn x_full_pathname(
    _vfs: *mut sqlite3_vfs,
    input: *const c_char,
    n_out: c_int,
    out: *mut c_char,
) -> c_int {
    // Verbatim copy, bounded by the output buffer.
    let mut i = 0isize;
    let max = n_out as isize - 1;
    while i < max {
        let c = *input.offset(i);
        *out.offset(i) = c;
        if c == 0 {
            return SQLITE_OK;
        }
        i += 1;
    }
    *out.offset(max.max(0)) = 0;
    SQLITE_OK
}

unsafe extern "C" fn x_randomness(_vfs: *mut sqlite3_vfs, n: c_int, out: *mut c_char) -> c_int {
    if n <= 0 || out.is_null() {
        return 0;
    }
    let buf = core::slice::from_raw_parts_mut(out as *mut u8, n as usize);
    fill_random(buf);
    n
}

unsafe extern "C" fn x_sleep(_vfs: *mut sqlite3_vfs, _micros: c_int) -> c_int {
    0
}

unsafe extern "C" fn x_current_time_int64(
    _vfs: *mut sqlite3_vfs,
    out: *mut sqlite3_int64,
) -> c_int {
    match clock() {
        Some(us) => {
            *out = us / 1000 + JULIAN_UNIX_OFFSET_MS;
            SQLITE_OK
        }
        // No injected clock: refuse. SQLite reports this as an error from the
        // date functions rather than fabricating an epoch (D-04).
        None => SQLITE_ERROR,
    }
}

unsafe extern "C" fn x_current_time(vfs: *mut sqlite3_vfs, out: *mut f64) -> c_int {
    let mut ms: sqlite3_int64 = 0;
    let rc = x_current_time_int64(vfs, &mut ms);
    if rc == SQLITE_OK {
        *out = ms as f64 / 86_400_000.0;
    }
    rc
}

unsafe extern "C" fn x_get_last_error(
    _vfs: *mut sqlite3_vfs,
    _n: c_int,
    _out: *mut c_char,
) -> c_int {
    0
}

// -- Registration ------------------------------------------------------------

static VFS_NAME: &[u8] = b"quarry\0";

static mut QUARRY_VFS: sqlite3_vfs = sqlite3_vfs {
    iVersion: 2,
    szOsFile: core::mem::size_of::<sqlite3_file>() as c_int,
    mxPathname: 512,
    pNext: core::ptr::null_mut(),
    zName: core::ptr::null(),
    pAppData: core::ptr::null_mut(),
    xOpen: Some(x_open),
    xDelete: Some(x_delete),
    xAccess: Some(x_access),
    xFullPathname: Some(x_full_pathname),
    xDlOpen: None,
    xDlError: None,
    xDlSym: None,
    xDlClose: None,
    xRandomness: Some(x_randomness),
    xSleep: Some(x_sleep),
    xCurrentTime: Some(x_current_time),
    xGetLastError: Some(x_get_last_error),
    xCurrentTimeInt64: Some(x_current_time_int64),
    xSetSystemCall: None,
    xGetSystemCall: None,
    xNextSystemCall: None,
};

/// Called by SQLite from `sqlite3_initialize()` because the amalgamation is
/// built with `SQLITE_OS_OTHER=1`. Registers the quarry VFS as the default,
/// which the built-in `memdb` VFS then adopts as its delegate for time and
/// randomness.
///
/// # Safety
/// Called exactly once by SQLite's initialization; must not be called directly.
#[no_mangle]
pub unsafe extern "C" fn sqlite3_os_init() -> c_int {
    QUARRY_VFS.zName = VFS_NAME.as_ptr() as *const c_char;
    sqlite3_vfs_register(core::ptr::addr_of_mut!(QUARRY_VFS), 1)
}

/// # Safety
/// Called by SQLite's shutdown path only.
#[no_mangle]
pub unsafe extern "C" fn sqlite3_os_end() -> c_int {
    SQLITE_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-21: the incumbent's xAccess answered "exists" for every path, which
    /// can push SQLite into hot-journal recovery against a file that is
    /// actually the database blob. Ours answers honestly: nothing exists.
    #[test]
    fn x_access_reports_nothing_exists() {
        for name in [
            b"main.db-journal\0".as_slice(),
            b"main.db-wal\0".as_slice(),
            b"anything\0".as_slice(),
        ] {
            let mut out: c_int = 99;
            let rc = unsafe {
                x_access(
                    core::ptr::null_mut(),
                    name.as_ptr() as *const c_char,
                    SQLITE_ACCESS_EXISTS,
                    &mut out,
                )
            };
            assert_eq!(rc, SQLITE_OK);
            assert_eq!(out, 0, "no file may ever be reported as existing");
        }
    }

    #[test]
    fn clock_math_is_exact() {
        // 2026-01-20T00:00:00Z in µs → Julian-day milliseconds
        set_clock(Some(1_768_867_200_000_000));
        let mut out: sqlite3_int64 = 0;
        let rc = unsafe { x_current_time_int64(core::ptr::null_mut(), &mut out) };
        assert_eq!(rc, SQLITE_OK);
        assert_eq!(out, 1_768_867_200_000 + JULIAN_UNIX_OFFSET_MS);
        set_clock(None);
        let rc = unsafe { x_current_time_int64(core::ptr::null_mut(), &mut out) };
        assert_eq!(
            rc, SQLITE_ERROR,
            "no injected clock ⇒ refuse, never fabricate"
        );
    }

    #[test]
    fn rng_streams_are_seeded_and_rewindable() {
        reset_rng(42);
        let mut a = [0u8; 16];
        fill_random(&mut a);
        rewind_rng();
        let mut b = [0u8; 16];
        fill_random(&mut b);
        assert_eq!(a, b, "rewind restarts the stream");
        reset_rng(43);
        let mut c = [0u8; 16];
        fill_random(&mut c);
        assert_ne!(a, c, "different seed ⇒ different stream");
        assert_ne!(a, [0u8; 16], "stream is not all-zero (D-05)");
    }
}
