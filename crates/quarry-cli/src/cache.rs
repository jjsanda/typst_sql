//! The cache trio (F-25/F-31, fixing the SQLTeX reproducibility gap):
//!
//! - `.quarry/manifest.cbor` — machine index the DOCUMENT reads (a cache miss
//!   must be a catchable dictionary lookup, because Typst's read() of a
//!   missing file is uncatchable);
//! - `.quarry/cache/<key>.cbor` — one envelope v1 per query, content-addressed;
//! - `quarry.lock` — the human/review surface: TOML, diff-friendly, committed.
//!   Data drift shows up in `git diff` as row counts and digests changing.

use anyhow::{bail, Context, Result};
use ciborium::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct Entry {
    pub key: String,
    pub source: String,
    pub kind: String,
    pub sql: String,
    pub params_repr: String,
    pub rows: u64,
    pub ok: bool,
    pub digest: String,
    pub fetched: String,
    pub elapsed_ms: u64,
}

pub struct Cache {
    pub dir: PathBuf,       // .quarry
    pub cache_dir: PathBuf, // .quarry/cache
    pub lock_path: PathBuf, // quarry.lock (beside quarry.toml)
    pub entries: BTreeMap<String, Entry>,
    pub mode: String,
    pub generated: String,
}

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_map()?
        .iter()
        .find(|(k, _)| k.as_text() == Some(key))
        .map(|(_, v)| v)
}

fn text(v: Option<&Value>) -> String {
    v.and_then(|v| v.as_text()).unwrap_or_default().to_string()
}

fn integer(v: Option<&Value>) -> u64 {
    v.and_then(|v| v.as_integer())
        .and_then(|i| u64::try_from(i128::from(i)).ok())
        .unwrap_or(0)
}

impl Cache {
    pub fn open(project_dir: &Path, cache_dir_name: &str) -> Result<Cache> {
        let dir = project_dir.join(cache_dir_name);
        let mut cache = Cache {
            cache_dir: dir.join("cache"),
            lock_path: project_dir.join("quarry.lock"),
            dir,
            entries: BTreeMap::new(),
            mode: "strict".into(),
            generated: String::new(),
        };
        let manifest_path = cache.dir.join("manifest.cbor");
        if manifest_path.exists() {
            let bytes = std::fs::read(&manifest_path)?;
            let manifest: Value = ciborium::from_reader(bytes.as_slice())
                .with_context(|| format!("parsing {}", manifest_path.display()))?;
            cache.mode = text(field(&manifest, "mode"));
            cache.generated = text(field(&manifest, "generated"));
            if let Some(queries) = field(&manifest, "queries").and_then(|q| q.as_map()) {
                for (k, v) in queries {
                    let key = k.as_text().unwrap_or_default().to_string();
                    cache.entries.insert(
                        key.clone(),
                        Entry {
                            key,
                            source: text(field(v, "source")),
                            kind: text(field(v, "kind")),
                            sql: text(field(v, "sql")),
                            params_repr: text(field(v, "params")),
                            rows: integer(field(v, "rows")),
                            ok: field(v, "ok").and_then(|v| v.as_bool()).unwrap_or(true),
                            digest: text(field(v, "digest")),
                            fetched: text(field(v, "fetched")),
                            elapsed_ms: integer(field(v, "elapsed-ms")),
                        },
                    );
                }
            }
        }
        Ok(cache)
    }

    pub fn envelope_path(&self, key: &str) -> PathBuf {
        self.cache_dir.join(format!("{key}.cbor"))
    }

    pub fn store_envelope(&mut self, entry: Entry, envelope: &[u8]) -> Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::write(self.envelope_path(&entry.key), envelope)?;
        self.entries.insert(entry.key.clone(), entry);
        Ok(())
    }

    pub fn remove(&mut self, key: &str) -> Result<()> {
        let _ = std::fs::remove_file(self.envelope_path(key));
        self.entries.remove(key);
        Ok(())
    }

    /// Persist manifest.cbor (for the document) and quarry.lock (for review).
    pub fn write(&self, mode: &str, generated: &str) -> Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        let queries: Vec<(Value, Value)> = self
            .entries
            .values()
            .map(|e| {
                (
                    Value::Text(e.key.clone()),
                    Value::Map(vec![
                        (Value::Text("source".into()), Value::Text(e.source.clone())),
                        (Value::Text("kind".into()), Value::Text(e.kind.clone())),
                        (Value::Text("sql".into()), Value::Text(e.sql.clone())),
                        (
                            Value::Text("params".into()),
                            Value::Text(e.params_repr.clone()),
                        ),
                        (Value::Text("rows".into()), Value::from(e.rows)),
                        (Value::Text("ok".into()), Value::Bool(e.ok)),
                        (Value::Text("digest".into()), Value::Text(e.digest.clone())),
                        (
                            Value::Text("fetched".into()),
                            Value::Text(e.fetched.clone()),
                        ),
                        (Value::Text("elapsed-ms".into()), Value::from(e.elapsed_ms)),
                    ]),
                )
            })
            .collect();
        let manifest = Value::Map(vec![
            (Value::Text("version".into()), Value::from(1u64)),
            (
                Value::Text("generated".into()),
                Value::Text(generated.to_string()),
            ),
            (Value::Text("mode".into()), Value::Text(mode.to_string())),
            (Value::Text("queries".into()), Value::Map(queries)),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&manifest, &mut bytes)?;
        std::fs::write(self.dir.join("manifest.cbor"), bytes)?;

        // quarry.lock — one [[query]] block per entry, sorted by key for
        // stable diffs.
        let mut lock = String::new();
        lock.push_str("# quarry.lock — generated by `quarry sync`; commit this file.\n");
        lock.push_str("# Data drift is visible here in review: row counts, digests, timestamps.\n");
        lock.push_str(&format!(
            "version = 1\ngenerated = {generated:?}\nmode = {mode:?}\n"
        ));
        for entry in self.entries.values() {
            lock.push_str("\n[[query]]\n");
            lock.push_str(&format!("key = {:?}\n", entry.key));
            lock.push_str(&format!("source = {:?}\n", entry.source));
            lock.push_str(&format!("kind = {:?}\n", entry.kind));
            lock.push_str(&format!("sql = {:?}\n", entry.sql));
            if !entry.params_repr.is_empty() {
                lock.push_str(&format!("params = {:?}\n", entry.params_repr));
            }
            lock.push_str(&format!("rows = {}\n", entry.rows));
            lock.push_str(&format!("ok = {}\n", entry.ok));
            lock.push_str(&format!("digest = {:?}\n", entry.digest));
            lock.push_str(&format!("fetched = {:?}\n", entry.fetched));
            lock.push_str(&format!("elapsed-ms = {}\n", entry.elapsed_ms));
        }
        std::fs::write(&self.lock_path, lock)?;
        Ok(())
    }

    /// Verify every manifest entry's cache file exists and matches its digest.
    pub fn verify(&self) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        for entry in self.entries.values() {
            let path = self.envelope_path(&entry.key);
            match std::fs::read(&path) {
                Err(_) => problems.push(format!(
                    "{}: cache file missing ({}) — run `quarry sync`",
                    entry.key,
                    path.display()
                )),
                Ok(bytes) => {
                    let digest = format!("blake3:{}", blake3::hash(&bytes).to_hex());
                    if digest != entry.digest {
                        problems.push(format!(
                            "{}: digest mismatch (manifest {}, file {}) — cache edited by hand?",
                            entry.key, entry.digest, digest
                        ));
                    }
                }
            }
        }
        // orphaned cache files (in cache/, not in manifest)
        if self.cache_dir.exists() {
            for f in std::fs::read_dir(&self.cache_dir)? {
                let name = f?.file_name().to_string_lossy().to_string();
                if let Some(key) = name.strip_suffix(".cbor") {
                    if !self.entries.contains_key(key) {
                        problems.push(format!(
                            "{key}: orphaned cache file (prune with `quarry prune`)"
                        ));
                    }
                }
            }
        }
        Ok(problems)
    }

    /// Drop entries not in the wanted set. Returns removed keys.
    pub fn prune(&mut self, wanted: &std::collections::BTreeSet<String>) -> Result<Vec<String>> {
        let stale: Vec<String> = self
            .entries
            .keys()
            .filter(|k| !wanted.contains(*k))
            .cloned()
            .collect();
        for key in &stale {
            self.remove(key)?;
        }
        // orphaned files too
        if self.cache_dir.exists() {
            for f in std::fs::read_dir(&self.cache_dir)? {
                let path = f?.path();
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                if let Some(key) = name.strip_suffix(".cbor") {
                    if !self.entries.contains_key(key) {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
        Ok(stale)
    }
}

/// F-42: compare two lockfiles and describe the drift.
pub fn diff_lockfiles(old: &str, new: &str) -> Result<String> {
    fn parse(text: &str) -> Result<BTreeMap<String, toml::Table>> {
        let value: toml::Table = toml::from_str(text)?;
        let mut out = BTreeMap::new();
        if let Some(toml::Value::Array(queries)) = value.get("query") {
            for q in queries {
                if let toml::Value::Table(t) = q {
                    if let Some(toml::Value::String(key)) = t.get("key") {
                        out.insert(key.clone(), t.clone());
                    }
                }
            }
        }
        Ok(out)
    }
    let old = parse(old).context("parsing old lockfile")?;
    let new = parse(new).context("parsing new lockfile")?;
    let mut report = String::new();
    for (key, entry) in &new {
        match old.get(key) {
            None => {
                report.push_str(&format!(
                    "+ {key} (new) — {} rows, source {}\n",
                    entry.get("rows").and_then(|v| v.as_integer()).unwrap_or(0),
                    entry.get("source").and_then(|v| v.as_str()).unwrap_or("?"),
                ));
            }
            Some(prev) => {
                let (d0, d1) = (
                    prev.get("digest").and_then(|v| v.as_str()).unwrap_or(""),
                    entry.get("digest").and_then(|v| v.as_str()).unwrap_or(""),
                );
                if d0 != d1 {
                    let (r0, r1) = (
                        prev.get("rows").and_then(|v| v.as_integer()).unwrap_or(0),
                        entry.get("rows").and_then(|v| v.as_integer()).unwrap_or(0),
                    );
                    report.push_str(&format!(
                        "~ {key} — data changed: {r0} → {r1} rows ({})\n",
                        entry.get("source").and_then(|v| v.as_str()).unwrap_or("?"),
                    ));
                }
            }
        }
    }
    for key in old.keys() {
        if !new.contains_key(key) {
            report.push_str(&format!("- {key} (removed)\n"));
        }
    }
    if report.is_empty() {
        report.push_str("no drift: lockfiles describe identical data\n");
    }
    Ok(report)
}

pub fn digest_of(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

pub fn params_repr(params: &Value) -> String {
    match params {
        Value::Null => String::new(),
        other => format!("{other:?}"),
    }
}

pub fn ensure_init(project_dir: &Path, cache_dir_name: &str) -> Result<()> {
    let dir = project_dir.join(cache_dir_name);
    std::fs::create_dir_all(dir.join("cache"))?;
    let manifest = dir.join("manifest.cbor");
    if !manifest.exists() {
        let empty = Value::Map(vec![
            (Value::Text("version".into()), Value::from(1u64)),
            (Value::Text("generated".into()), Value::Text("never".into())),
            (Value::Text("mode".into()), Value::Text("strict".into())),
            (Value::Text("queries".into()), Value::Map(vec![])),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&empty, &mut bytes)?;
        std::fs::write(manifest, bytes)?;
    }
    Ok(())
}

pub fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub fn parse_iso(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

pub fn entry_is_fresh(entry: &Entry, ttl: std::time::Duration) -> bool {
    match parse_iso(&entry.fetched) {
        Some(fetched) => {
            let age = chrono::Utc::now().signed_duration_since(fetched);
            age.to_std().map(|age| age < ttl).unwrap_or(true)
        }
        None => false,
    }
}

pub fn bail_offline(mode: &str, missing: &[String]) -> Result<()> {
    if missing.is_empty() {
        return Ok(());
    }
    bail!(
        "{} quer{} missing from the cache in {mode} mode:\n{}\n\
         fix: run `quarry sync` in an environment with source access",
        missing.len(),
        if missing.len() == 1 {
            "y is"
        } else {
            "ies are"
        },
        missing.join("\n"),
    )
}
