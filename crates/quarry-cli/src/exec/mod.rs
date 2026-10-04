//! Query execution against declared sources. Every driver produces the same
//! envelope v1 bytes the WASM plugin produces (via quarry-engine's envelope
//! builders), so a document cannot tell where its data came from — that is
//! the backend contract.

use crate::config::Source;
use crate::requests::DiscoveredRequest;
use anyhow::{bail, Result};
use ciborium::Value;
use quarry_engine::envelope::{ClockInfo, ColumnMeta, ResultSet, Stats, Warning};
use std::time::Instant;

#[cfg(feature = "analytics")]
pub mod analytics;
#[cfg(feature = "mysql-driver")]
pub mod mysql_driver;
#[cfg(feature = "postgres-driver")]
pub mod postgres_driver;
pub mod sqlite;

pub struct ExecOutcome {
    pub envelope: Vec<u8>,
    pub rows: u64,
    pub ok: bool,
    pub elapsed_ms: u64,
    pub kind: String,
}

/// Execute one request against its source. SQL errors become error envelopes
/// (cached, so offline rebuilds reproduce them); infrastructure errors
/// (unreachable server, bad credentials) fail the sync.
pub fn execute(
    name: &str,
    source: &Source,
    request: &DiscoveredRequest,
    fetched_iso: &str,
) -> Result<ExecOutcome> {
    let start = Instant::now();
    let mut outcome = match source.kind.as_str() {
        "sqlite" => sqlite::execute(name, source, request, fetched_iso)?,
        #[cfg(feature = "analytics")]
        "analytics" => analytics::execute(name, source, request, fetched_iso)?,
        #[cfg(feature = "postgres-driver")]
        "postgres" => postgres_driver::execute(name, source, request, fetched_iso)?,
        #[cfg(feature = "mysql-driver")]
        "mysql" => mysql_driver::execute(name, source, request, fetched_iso)?,
        other => bail!(
            "source {name:?}: kind {other:?} is not supported by this build of the quarry CLI"
        ),
    };
    outcome.elapsed_ms = start.elapsed().as_millis() as u64;
    Ok(outcome)
}

/// Stats block for sidecar-produced envelopes. The clock VALUE is
/// deliberately absent: embedding the fetch time in the envelope would make
/// every resync change every digest, drowning real data drift in timestamp
/// noise (the lockfile records `fetched` separately). Origin "sidecar" tells
/// the document where the data came from.
pub fn sidecar_stats(
    source_name: &str,
    engine: &str,
    row_count: u64,
    truncated: bool,
    _fetched_iso: &str,
) -> Stats {
    Stats {
        row_count,
        truncated,
        source: source_name.to_string(),
        statements: 1,
        clock: ClockInfo {
            value: None,
            origin: "sidecar".into(),
        },
        seed: None,
        engine: engine.to_string(),
        placeholder: false,
    }
}

/// Assemble an ok envelope from generic columns/rows (used by the analytics,
/// postgres and mysql drivers; the sqlite driver reuses the engine directly).
pub struct Generic {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Value>>,
    pub truncated: bool,
    pub warnings: Vec<Warning>,
}

impl Generic {
    pub fn into_outcome(
        self,
        source_name: &str,
        kind: &str,
        engine: &str,
        fetched_iso: &str,
    ) -> ExecOutcome {
        let rows = self.rows.len() as u64;
        let stats = sidecar_stats(source_name, engine, rows, self.truncated, fetched_iso);
        let result = ResultSet {
            columns: self.columns,
            rows: self.rows,
            truncated: self.truncated,
        };
        ExecOutcome {
            envelope: quarry_engine::envelope::ok_envelope(&result, &stats, &self.warnings, false),
            rows,
            ok: true,
            elapsed_ms: 0,
            kind: kind.to_string(),
        }
    }
}

pub fn error_outcome(
    kind: &str,
    engine: &str,
    error: quarry_engine::error::QuarryError,
) -> ExecOutcome {
    ExecOutcome {
        envelope: quarry_engine::envelope::error_envelope(&error, engine),
        rows: 0,
        ok: false,
        elapsed_ms: 0,
        kind: kind.to_string(),
    }
}

/// Deduplicate column names the way the engine does (name, name_2, name_3 …)
/// so dict rows never silently drop values.
pub fn dedupe_names(names: Vec<String>) -> Vec<String> {
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    names
        .into_iter()
        .map(|name| {
            let n = counts.entry(name.clone()).or_insert(0);
            *n += 1;
            if *n > 1 {
                format!("{name}_{n}")
            } else {
                name
            }
        })
        .collect()
}

/// Enforce the per-source row cap: truncate with a warning, never fail (D-12).
pub fn apply_row_cap(rows: &mut Vec<Vec<Value>>, cap: u64, warnings: &mut Vec<Warning>) -> bool {
    if rows.len() as u64 > cap {
        rows.truncate(cap as usize);
        warnings.push(Warning {
            code: "row-cap".into(),
            message: format!(
                "result truncated to the source's max-rows = {cap}; raise it in quarry.toml if intended"
            ),
        });
        true
    } else {
        false
    }
}
