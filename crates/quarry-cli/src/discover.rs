//! Query discovery: extract `<quarry-request>` metadata from documents.
//!
//! Documents compile with `--input quarry-discover=1`, which turns every
//! remote query into an empty placeholder envelope carrying its request —
//! that is what makes a cold checkout compile before any cache exists.
//! `typst query` is deprecated in Typst 0.15 (C-1); the primary path is
//! `typst eval 'query(<quarry-request>).map(m => m.value)' --in doc.typ`,
//! with `typst query` retained as the fallback for ≤ 0.14.
//!
//! Queries whose results never reach page content (pure code use — chart
//! data, inline values) are covered either by `#qr.declare(env)` in the
//! document or by explicit [sources.X.queries.NAME] declarations in
//! quarry.toml; `quarry sync` unions all of these.

use crate::config::Config;
use crate::requests::{json_to_cbor, toml_to_cbor, DiscoveredRequest};
use anyhow::{bail, Context, Result};
use ciborium::Value;
use std::path::Path;
use std::process::Command;

fn typst_version(typst: &str) -> Result<(u32, u32)> {
    let out = Command::new(typst)
        .arg("--version")
        .output()
        .with_context(|| format!("running {typst} --version — is Typst installed?"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let ver = text.split_whitespace().nth(1).unwrap_or("0.0.0");
    let mut parts = ver.split('.');
    let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Ok((major, minor))
}

fn parse_requests(json: &str, doc: &Path) -> Result<Vec<DiscoveredRequest>> {
    let values: serde_json::Value = serde_json::from_str(json)
        .with_context(|| format!("parsing discovery output for {}", doc.display()))?;
    let Some(items) = values.as_array() else {
        bail!("discovery output for {} is not a JSON array", doc.display())
    };
    let mut requests = Vec::new();
    for item in items {
        let source = item.get("source").and_then(|v| v.as_str());
        let sql = item.get("sql").and_then(|v| v.as_str());
        let (Some(source), Some(sql)) = (source, sql) else {
            continue; // foreign metadata under our label — skip, don't die
        };
        requests.push(DiscoveredRequest {
            source: source.to_string(),
            sql: sql.to_string(),
            params: item.get("params").map(json_to_cbor).unwrap_or(Value::Null),
            declared_key: item.get("key").and_then(|v| v.as_str()).map(String::from),
        });
    }
    Ok(requests)
}

/// Extract requests from one document.
pub fn discover_document(typst: &str, root: &Path, doc: &Path) -> Result<Vec<DiscoveredRequest>> {
    let (major, minor) = typst_version(typst)?;
    let use_eval = (major, minor) >= (0, 15);
    let output = if use_eval {
        Command::new(typst)
            .args(["eval", "query(<quarry-request>).map(m => m.value)"])
            .arg("--in")
            .arg(doc)
            .args(["--format", "json"])
            .arg("--root")
            .arg(root)
            .args(["--input", "quarry-discover=1"])
            .output()?
    } else {
        Command::new(typst)
            .arg("query")
            .arg(doc)
            .arg("<quarry-request>")
            .args(["--field", "value", "--format", "json"])
            .arg("--root")
            .arg(root)
            .args(["--input", "quarry-discover=1"])
            .output()?
    };
    if !output.status.success() {
        bail!(
            "discovery compile failed for {}:\n{}",
            doc.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    parse_requests(&String::from_utf8_lossy(&output.stdout), doc)
}

/// Requests declared explicitly in quarry.toml.
pub fn declared_requests(config: &Config) -> Vec<DiscoveredRequest> {
    let mut out = Vec::new();
    for (source_name, source) in &config.sources {
        for query in source.queries.values() {
            let params = if query.params.is_empty() {
                Value::Null
            } else {
                toml_to_cbor(&toml::Value::Table(query.params.clone()))
            };
            out.push(DiscoveredRequest {
                source: source_name.clone(),
                sql: query.sql.clone(),
                params,
                declared_key: None,
            });
        }
    }
    out
}

/// Union of document discovery (many docs) and explicit declarations,
/// de-duplicated by cache key. Returns (requests-with-keys).
pub fn discover_all(
    config: &Config,
    typst: &str,
    root: &Path,
    docs: &[std::path::PathBuf],
) -> Result<Vec<(String, DiscoveredRequest)>> {
    let mut requests = declared_requests(config);
    for doc in docs {
        requests.extend(discover_document(typst, root, doc)?);
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for request in requests {
        if !config.sources.contains_key(&request.source) {
            bail!(
                "a document queries source {:?} but quarry.toml does not declare it \
                 (declared: {})",
                request.source,
                config
                    .sources
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        let key = request.key()?;
        if seen.insert(key.clone()) {
            out.push((key, request));
        }
    }
    Ok(out)
}
