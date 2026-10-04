//! quarry.toml: source declarations and sync policy.
//!
//! Credentials NEVER appear literally (F-26): `url` accepts `env:VAR` or
//! `file:path` references, resolved at sync time. A literal connection string
//! is a hard error here and a lint finding in `quarry lint`.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub quarry: General,
    #[serde(default)]
    pub sources: BTreeMap<String, Source>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct General {
    pub version: u32,
    /// strict (default in CI) | auto | offline
    pub mode: String,
    /// auto mode: refresh entries older than this (humantime, e.g. "24h")
    pub ttl: String,
    #[serde(rename = "cache-dir")]
    pub cache_dir: String,
    /// the typst binary used for discovery
    pub typst: String,
}

impl Default for General {
    fn default() -> Self {
        General {
            version: 1,
            mode: "strict".into(),
            ttl: "24h".into(),
            cache_dir: ".quarry".into(),
            typst: "typst".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// sqlite | analytics | postgres | mysql
    pub kind: String,
    /// sqlite: database file path (relative to quarry.toml)
    pub path: Option<String>,
    /// postgres/mysql: connection reference — `env:VAR` or `file:path` ONLY
    pub url: Option<String>,
    /// analytics: file globs registered as tables (name = file stem)
    #[serde(default)]
    pub files: Vec<String>,
    /// analytics: explicit table name → file path
    #[serde(default)]
    pub tables: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub read_only: bool,
    #[serde(default = "default_timeout", rename = "statement-timeout")]
    pub statement_timeout: String,
    #[serde(default = "default_max_rows", rename = "max-rows")]
    pub max_rows: u64,
    /// explicitly declared queries (always discovered, independent of the
    /// document scan): [sources.X.queries.NAME] sql = "…", params = {…}
    #[serde(default)]
    pub queries: BTreeMap<String, DeclaredQuery>,
}

fn default_true() -> bool {
    true
}
fn default_timeout() -> String {
    "30s".into()
}
fn default_max_rows() -> u64 {
    1_000_000
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredQuery {
    pub sql: String,
    #[serde(default)]
    pub params: toml::Table,
}

impl Config {
    pub fn load(dir: &Path) -> Result<(Config, PathBuf)> {
        let path = dir.join("quarry.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("{} not found — run `quarry init` first", path.display()))?;
        let config: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if config.quarry.version != 1 {
            bail!(
                "quarry.toml declares version {} (this CLI understands 1)",
                config.quarry.version
            );
        }
        if !matches!(config.quarry.mode.as_str(), "strict" | "auto" | "offline") {
            bail!(
                "quarry.toml mode must be strict, auto or offline, got {:?}",
                config.quarry.mode
            );
        }
        for (name, source) in &config.sources {
            source.validate(name)?;
        }
        Ok((config, path))
    }

    pub fn ttl(&self) -> Result<std::time::Duration> {
        humantime::parse_duration(&self.quarry.ttl)
            .map_err(|e| anyhow!("quarry.toml ttl {:?}: {e}", self.quarry.ttl))
    }
}

impl Source {
    fn validate(&self, name: &str) -> Result<()> {
        match self.kind.as_str() {
            "sqlite" => {
                if self.path.is_none() {
                    bail!("source {name:?}: kind = \"sqlite\" needs path = \"…\"");
                }
            }
            "analytics" => {
                if self.files.is_empty() && self.tables.is_empty() {
                    bail!("source {name:?}: kind = \"analytics\" needs files = […] or [sources.{name}.tables]");
                }
            }
            "postgres" | "mysql" => {
                if !self.read_only {
                    bail!(
                        "source {name:?}: read-only = false is not supported — read-only is a                          security property quarry keeps absolute (a document is not a migration tool)"
                    );
                }
                let url = self.url.as_deref().unwrap_or_default();
                if url.is_empty() {
                    bail!(
                        "source {name:?}: kind = {:?} needs url = \"env:VAR\"",
                        self.kind
                    );
                }
                if !(url.starts_with("env:") || url.starts_with("file:")) {
                    bail!(
                        "source {name:?}: url must be a reference (env:VAR or file:path), never a literal \
                         connection string — credentials do not belong in committed files (F-26)"
                    );
                }
            }
            other => bail!(
                "source {name:?}: unknown kind {other:?} (sqlite | analytics | postgres | mysql)"
            ),
        }
        Ok(())
    }

    /// Resolve the connection URL from its env:/file: reference.
    pub fn resolve_url(&self, name: &str) -> Result<String> {
        let reference = self.url.as_deref().unwrap_or_default();
        if let Some(var) = reference.strip_prefix("env:") {
            std::env::var(var).map_err(|_| {
                anyhow!(
                    "source {name:?}: environment variable {var} is not set \
                     (the refresh job needs it; compile-from-cache does not)"
                )
            })
        } else if let Some(path) = reference.strip_prefix("file:") {
            Ok(std::fs::read_to_string(path)
                .with_context(|| format!("source {name:?}: reading {path}"))?
                .trim()
                .to_string())
        } else {
            bail!("source {name:?}: url must start with env: or file:")
        }
    }

    pub fn timeout(&self) -> std::time::Duration {
        humantime::parse_duration(&self.statement_timeout)
            .unwrap_or(std::time::Duration::from_secs(30))
    }
}

pub const INIT_TEMPLATE: &str = r#"# quarry sidecar configuration — see docs/backend-contract.md
[quarry]
version = 1
mode = "strict"        # strict: missing/stale cache is an error (CI-safe)
                       # auto:   refresh entries older than ttl while authoring
                       # offline: never connect, cache only
ttl = "24h"
cache-dir = ".quarry"
typst = "typst"

# --- examples ---------------------------------------------------------------
# [sources.sales]
# kind = "sqlite"
# path = "data/sales.sqlite"
#
# [sources.events]
# kind = "analytics"                  # DataFusion over CSV/Parquet/JSON
# files = ["data/events-*.parquet", "data/accounts.csv"]
#
# [sources.warehouse]
# kind = "postgres"
# url = "env:WAREHOUSE_URL"           # never a literal — quarry lint enforces
# statement-timeout = "30s"
# max-rows = 1_000_000
#
# Queries living in the document are discovered automatically. Queries used
# only in code contexts can also be declared explicitly:
# [sources.warehouse.queries.churn]
# sql = "SELECT month, churn_rate FROM metrics WHERE cohort = $1"
# params = { cohort = "2026-enterprise" }
"#;
