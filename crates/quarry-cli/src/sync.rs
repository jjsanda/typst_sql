//! `quarry sync`: discover → execute → cache → lock.
//!
//! Modes (04-project-description.md §6.3):
//! - strict: execute every discovered query fresh; missing sources fail.
//!   The default, and what CI should run before compiling.
//! - auto: refresh entries older than the TTL; keep fresh ones (authoring).
//! - offline: never connect — verify the cache covers every request and fail
//!   loudly (naming queries and the fix) if not.

use crate::cache::{self, Cache, Entry};
use crate::config::Config;
use crate::discover;
use crate::exec;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

pub struct SyncReport {
    pub executed: usize,
    pub kept: usize,
    pub failed_sql: usize,
    pub pruned: Vec<String>,
    pub drift: String,
}

pub fn sync(
    project_dir: &Path,
    config: &Config,
    docs: &[PathBuf],
    mode_override: Option<&str>,
    prune: bool,
) -> Result<SyncReport> {
    let mode = mode_override.unwrap_or(&config.quarry.mode).to_string();
    let typst = &config.quarry.typst;
    let requests = discover::discover_all(config, typst, project_dir, docs)?;
    let mut cache_store = Cache::open(project_dir, &config.quarry.cache_dir)?;
    let old_lock = std::fs::read_to_string(&cache_store.lock_path).unwrap_or_default();
    let ttl = config.ttl()?;
    let generated = cache::now_iso();

    let mut executed = 0usize;
    let mut kept = 0usize;
    let mut failed_sql = 0usize;
    let mut missing_offline: Vec<String> = Vec::new();

    for (key, request) in &requests {
        let existing = cache_store.entries.get(key);
        match mode.as_str() {
            "offline" => {
                if existing.is_none() {
                    missing_offline.push(format!(
                        "  {} against {:?}: {}",
                        key,
                        request.source,
                        request
                            .sql
                            .split('\n')
                            .map(str::trim)
                            .collect::<Vec<_>>()
                            .join(" "),
                    ));
                }
                kept += usize::from(existing.is_some());
                continue;
            }
            "auto" => {
                if let Some(entry) = existing {
                    if cache::entry_is_fresh(entry, ttl) {
                        kept += 1;
                        continue;
                    }
                }
            }
            _ => {} // strict: always refresh
        }

        let source = config
            .sources
            .get(&request.source)
            .expect("validated in discover_all");
        let fetched = cache::now_iso();
        let outcome = exec::execute(&request.source, source, request, &fetched)?;
        if !outcome.ok {
            failed_sql += 1;
        }
        let entry = Entry {
            key: key.clone(),
            source: request.source.clone(),
            kind: outcome.kind.clone(),
            sql: request.sql.clone(),
            params_repr: cache::params_repr(&request.params),
            rows: outcome.rows,
            ok: outcome.ok,
            digest: cache::digest_of(&outcome.envelope),
            fetched,
            elapsed_ms: outcome.elapsed_ms,
        };
        cache_store.store_envelope(entry, &outcome.envelope)?;
        executed += 1;
    }

    cache::bail_offline(&mode, &missing_offline)?;

    let mut pruned = Vec::new();
    if prune {
        let wanted: std::collections::BTreeSet<String> =
            requests.iter().map(|(k, _)| k.clone()).collect();
        pruned = cache_store.prune(&wanted)?;
    }

    cache_store.write(&mode, &generated)?;
    let new_lock = std::fs::read_to_string(&cache_store.lock_path)?;
    let drift = if old_lock.is_empty() {
        format!(
            "{} quer{} cached (first sync)\n",
            requests.len(),
            if requests.len() == 1 { "y" } else { "ies" }
        )
    } else {
        cache::diff_lockfiles(&old_lock, &new_lock)?
    };

    Ok(SyncReport {
        executed,
        kept,
        failed_sql,
        pruned,
        drift,
    })
}

/// Find the documents to sync: explicit args, or every top-level *.typ file.
pub fn resolve_docs(project_dir: &Path, args: &[String]) -> Result<Vec<PathBuf>> {
    if !args.is_empty() {
        return Ok(args.iter().map(PathBuf::from).collect());
    }
    let mut docs: Vec<PathBuf> = std::fs::read_dir(project_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "typ").unwrap_or(false))
        .collect();
    docs.sort();
    if docs.is_empty() {
        bail!(
            "no .typ documents found in {} — pass documents explicitly: quarry sync report.typ",
            project_dir.display()
        );
    }
    Ok(docs)
}
