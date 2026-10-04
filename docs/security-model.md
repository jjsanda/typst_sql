# Security model

A package that executes SQL at compile time — and, through its sidecar,
connects to production databases — owes its users an explicit security story
(04-project-description.md §8). This is it.

## 1. The WASM path has zero ambient authority

The plugin imports exactly four WASI stubs (`environ_get`,
`environ_sizes_get`, `fd_write`, `proc_exit` — panic plumbing, stubbed to
no-ops) and the two Typst protocol functions. **No clock, no randomness, no
file, no network imports exist in the module.** The import list is committed
(`tools/wasi-imports.allowlist`) and CI fails if it ever grows — a new
ambient-authority import cannot slip in silently. Database bytes enter only
because the document author passed them to `read()`, which Typst already
governs.

## 2. Read-only is enforced at every layer

1. `SQLITE_DESERIALIZE_READONLY` — the pager refuses writes outright;
2. a deny-by-default authorizer with human-readable denials (`INSERT`,
   `UPDATE`, `DELETE`, DDL, `ATTACH`, transaction control, stateful PRAGMAs);
   temp-schema scratch space is the only sanctioned mutation and is rolled
   back after every query;
3. `SQLITE_DBCONFIG_DEFENSIVE` + untrusted schema + no triggers + no
   extension loading;
4. the sidecar: Postgres sessions run `SET default_transaction_read_only=on`
   inside a rolled-back transaction; MySQL sessions run
   `SET SESSION TRANSACTION READ ONLY`; and the documentation recommends
   read-only database credentials on top.

Regression `writes_are_denied_with_clear_messages` attempts writes through
every statement class and verifies both the denial and that the data is
untouched.

## 3. Credentials never appear in documents

`quarry.toml` holds **references** (`env:VAR`, `file:path`) — a literal
connection string is rejected at config-parse time *and* flagged by
`quarry lint` (F-26/F-27). Compile-from-cache needs no credentials at all;
only the refresh job does. `quarry.lock` and the cache contain results, never
secrets.

## 4. Binding is the only interpolation path

Parameters are bound (`sqlite3_bind_*`, `$n`, native named params), never
spliced. String-concatenated SQL is documented as unsupported and
`quarry lint` flags it. Injection-shaped values are inert data (regression
d14).

## 5. Untrusted database files are a threat model

A malformed SQLite file is attacker-controlled input to a parser running in
the compiler (R-07; the incumbent's `qsort` overflow is this class, live).
Mitigations, outermost first:
- Typst's wasmi sandbox: an out-of-bounds access traps the interpreter — it
  cannot touch the host;
- memory-safe Rust for everything quarry adds (VFS, protocol, envelope);
- battle-tested SQLite `memdb` for all page I/O — no hand-rolled file layer;
- header validation + early schema verification before a source is accepted;
- continuous adversarial input: the curated malformed corpus and seeded
  mutation sweep run in every gate (`just test-fuzz-lite`, 800+ inputs), and
  nightly CI runs libFuzzer with ASAN natively (F-28).

## 6. Resource limits

Row caps (100k default) and byte caps (64 MiB default) truncate with an
honest `truncated: true`, never a hang or an OOM crash; the sidecar adds
statement timeouts and per-source row caps. A runaway query fails cleanly.

## 7. Reproducible, auditable binaries

Releases are built in CI from a tagged commit, **twice on independent
runners**, and published only when the hashes match; the hash goes in the
release notes (F-22). The incumbent shipped a 654 KB binary present in no
commit (D-22); quarry treats that as a disqualifying defect class.

## Reporting

See SECURITY.md for the disclosure process.
