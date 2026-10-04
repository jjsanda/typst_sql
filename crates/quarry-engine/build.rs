//! Compiles the vendored SQLite amalgamation for the current target.
//!
//! Two targets are supported:
//! - `wasm32-wasip1`: compiled with wasi-sdk clang against wasi-libc (a *real*
//!   libc/libm — the incumbent's stubbed arithmetic is the defect class this
//!   project exists to eliminate).
//! - native (x86_64 etc.): compiled with the system compiler using the *same*
//!   flag matrix, so the differential test suite compares engines, not OS layers.
//!
//! The build refuses to proceed if the vendored source does not match the
//! pinned version (the D-24 defect class: docs drifting from the shipped binary).

use std::env;
use std::fs;
use std::path::PathBuf;

/// The single source of truth for the vendored SQLite version.
/// `vendor/sqlite/VERSION`, `sqlite3.h` and `sqlite3_libversion()` (asserted
/// at test time) must all agree with this.
const PINNED_SQLITE_VERSION: &str = "3.53.4";

/// The SQLite compile-time feature matrix (F-02). Every entry is deliberate;
/// the rationale for each lives in docs/feature-matrix.md.
const DEFINES: &[(&str, Option<&str>)] = &[
    // We provide sqlite3_os_init/sqlite3_os_end and a base VFS in Rust.
    ("SQLITE_OS_OTHER", Some("1")),
    // One connection per instance; wasm is single-threaded, native use is
    // serialized by an engine-level lock.
    ("SQLITE_THREADSAFE", Some("0")),
    // Security: extension loading is meaningless and dangerous in a
    // compile-time sandbox. (The one flag the incumbent got right — D-19.)
    ("SQLITE_OMIT_LOAD_EXTENSION", Some("1")),
    // Temp tables and sorter spills live in memory; the VFS has no files.
    ("SQLITE_TEMP_STORE", Some("3")),
    // D-01/D-19: sqrt/pow/log/… as SQL functions, backed by a real libm.
    ("SQLITE_ENABLE_MATH_FUNCTIONS", Some("1")),
    // D-19: full-text search.
    ("SQLITE_ENABLE_FTS5", Some("1")),
    ("SQLITE_ENABLE_RTREE", Some("1")),
    // F-15: sqlite3_column_table_name / _origin_name for envelope metadata.
    ("SQLITE_ENABLE_COLUMN_METADATA", Some("1")),
    // No timezone database exists in the sandbox; 'localtime' errors clearly
    // instead of being silently wrong. Documented in the feature matrix.
    ("SQLITE_OMIT_LOCALTIME", Some("1")),
    // Double-quoted string literals off: a typo'd identifier is an error,
    // not a silently-created string.
    ("SQLITE_DQS", Some("0")),
    // Multi-source headroom (default is 10).
    ("SQLITE_MAX_ATTACHED", Some("32")),
    // Small perf win; memory stats come from the wasm page count instead.
    ("SQLITE_DEFAULT_MEMSTATUS", Some("0")),
    // No mmap in either environment; cuts dead code paths.
    ("SQLITE_MAX_MMAP_SIZE", Some("0")),
    // Read-only engine: statement journals are never needed beyond memory.
    ("SQLITE_DEFAULT_JOURNAL_SIZE_LIMIT", Some("0")),
];

fn main() {
    let vendor = PathBuf::from("vendor/sqlite");
    println!("cargo:rerun-if-changed=vendor/sqlite/sqlite3.c");
    println!("cargo:rerun-if-changed=vendor/sqlite/VERSION");
    println!("cargo:rerun-if-env-changed=WASI_SDK");

    // -- D-24 guard: the vendored source must be exactly the pinned version. --
    let version_file = fs::read_to_string(vendor.join("VERSION"))
        .expect("vendor/sqlite/VERSION missing — run tools/bootstrap.sh");
    assert_eq!(
        version_file.trim(),
        PINNED_SQLITE_VERSION,
        "vendored SQLite VERSION file does not match the pinned version"
    );
    let header = fs::read_to_string(vendor.join("sqlite3.h")).expect("sqlite3.h missing");
    let header_version = header
        .lines()
        .find_map(|l| l.strip_prefix("#define SQLITE_VERSION "))
        .map(|v| v.trim().trim_matches('"').to_string())
        .expect("SQLITE_VERSION not found in sqlite3.h");
    assert_eq!(
        header_version, PINNED_SQLITE_VERSION,
        "sqlite3.h declares a different version than vendor/sqlite/VERSION — \
         the amalgamation and the pin have drifted (D-24)"
    );
    println!("cargo:rustc-env=QUARRY_SQLITE_VERSION={PINNED_SQLITE_VERSION}");

    let target = env::var("TARGET").unwrap();
    let mut build = cc::Build::new();
    build.file(vendor.join("sqlite3.c"));
    for (k, v) in DEFINES {
        build.define(k, *v);
    }

    if target.starts_with("wasm32") {
        let sdk = env::var("WASI_SDK").expect(
            "WASI_SDK must point at a wasi-sdk installation for wasm builds \
             (run tools/bootstrap.sh and export WASI_SDK=~/.local/quarry/opt/wasi-sdk)",
        );
        env::set_var("AR_wasm32_wasip1", format!("{sdk}/bin/llvm-ar"));
        build
            .compiler(format!("{sdk}/bin/clang"))
            .flag(format!("--sysroot={sdk}/share/wasi-sysroot"))
            .target("wasm32-wasip1")
            // No LTO on the C side: wasi-sdk's LLVM and rustc's LLVM may
            // disagree on bitcode; object-level linking is the reliable path.
            .flag("-fno-lto")
            .opt_level(2);
    } else {
        build.opt_level(2);
        // Native SQLite emits a handful of benign warnings under -Wall; the
        // amalgamation is vendored verbatim and not ours to patch.
        build.warnings(false);
    }
    build.compile("quarry_sqlite3");
}
