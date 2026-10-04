# SQLite feature matrix (F-02)

Every compile-time flag of the vendored engine, with its reason. Nothing here
is incidental — the incumbent's matrix was an accident and its gaps produced
silently wrong answers (D-01, D-19). The build asserts the pinned version and
this document is checked against the runtime by regression d24.

**Vendored engine: SQLite 3.53.4** (amalgamation, SHA3-256-verified download,
`crates/quarry-engine/vendor/sqlite/`). Runtime `sqlite3_libversion()`, the
build-time pin and this file must all agree — the build fails otherwise.

| Define | Value | Why |
|---|---|---|
| `SQLITE_OS_OTHER` | 1 | quarry provides `sqlite3_os_init` and the base VFS: injected clock (D-04), seeded randomness (D-05), honest `xAccess` (D-21). All page I/O uses SQLite's own `memdb` via `sqlite3_deserialize`. |
| `SQLITE_THREADSAFE` | 0 | One instance per WASM module; native use is serialized by an engine-level lock (one live connection per process, documented). |
| `SQLITE_OMIT_LOAD_EXTENSION` | 1 | Extension loading is meaningless and dangerous in a compile-time sandbox — the one flag the incumbent got right (D-19). `capabilities()` reports it off and d19 proves `load_extension()` does not exist. |
| `SQLITE_TEMP_STORE` | 3 | Temp tables and sorter spills live in memory; the VFS has no file surface at all. |
| `SQLITE_ENABLE_MATH_FUNCTIONS` | 1 | `sqrt`/`pow`/`log`/… as SQL functions, backed by a **real libm** (wasi-libc) — the D-01 fix. Differentially tested against native, ≤2 ULP. |
| `SQLITE_ENABLE_FTS5` | 1 | Full-text search (D-19). Exercised by the differential corpus. |
| `SQLITE_ENABLE_RTREE` | 1 | Cheap, commonly requested; temp-schema R-tree creation allowed. |
| JSON functions | default-on | json_extract/each/tree… exercised differentially. |
| `SQLITE_ENABLE_COLUMN_METADATA` | 1 | `columns[].table` / `origin` in the envelope (F-15). |
| `SQLITE_OMIT_LOCALTIME` | 1 | There is no timezone database in the sandbox. `'localtime'` errors clearly instead of being silently wrong; timezone rendering is a formatting decision in the Typst layer. |
| `SQLITE_DQS` | 0 | Double-quoted strings off: a typo'd identifier is an error with a spelling hint, not a silently-created string literal. |
| `SQLITE_MAX_ATTACHED` | 32 | Multi-source headroom (the API exposes up to 8 named sources). |
| `SQLITE_DEFAULT_MEMSTATUS` | 0 | Perf; memory observability comes from the WASM page count. |
| `SQLITE_MAX_MMAP_SIZE` | 0 | No mmap in either environment; dead code removed. |
| `SQLITE_DEFAULT_JOURNAL_SIZE_LIMIT` | 0 | Read-only engine; journals never persist. |

## Runtime configuration (every connection)

| Setting | Value | Why |
|---|---|---|
| `sqlite3_deserialize` | `SQLITE_DESERIALIZE_READONLY`, no copy | Pager-level write refusal; zero-copy over the caller's bytes. |
| `SQLITE_DBCONFIG_DEFENSIVE` | on | Schema-corruption writes blocked even if a layer above fails. |
| `SQLITE_DBCONFIG_ENABLE_TRIGGER` | off | Triggers never execute in a read-only reporting engine. |
| `SQLITE_DBCONFIG_TRUSTED_SCHEMA` | off | Functions/vtabs in a hostile schema are not trusted (R-07). |
| Authorizer | deny-by-default | Writes, ATTACH/DETACH, transaction control and stateful PRAGMAs denied with mapped messages; temp-schema scratch objects allowed (rolled back per query by the purity guard). |
| Read-only PRAGMA allowlist | table_info, table_xinfo, table_list, index_list, index_info, index_xinfo, foreign_key_list, integrity_check, quick_check (arg ok); application_id, collation_list, compile_options, data_version, database_list, encoding, freelist_count, function_list, journal_mode, module_list, page_count, page_size, pragma_list, schema_version, user_version (no arg) | Introspection stays available; anything stateful is denied for purity. |

## Deliberate deviations from the original plan

- `SQLITE_ENABLE_NORMALIZE` is **not** set. `sqlite3_normalized_sql()`
  anonymizes literals, which would make *different* queries collide on one
  cache key — the opposite of content addressing. Cache keys use quarry's own
  canonicalizer (comments stripped, whitespace collapsed, literals preserved),
  the same code in the plugin and the CLI.
- `SQLITE_ENABLE_STMT_SCANSTATUS` is not set in 0.1 (F-39 profiling is
  post-1.0; the flag costs size on every user until then).
