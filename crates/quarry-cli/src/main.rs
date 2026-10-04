//! quarry — the sidecar CLI (F-07): reach databases the compile-time sandbox
//! cannot, materialize results into a committed, content-addressed cache, and
//! keep builds reproducible offline forever (F-25/F-31).

mod cache;
mod config;
mod discover;
mod exec;
mod lint;
mod requests;
mod sync;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "quarry",
    version,
    about = "Compile-time SQL for Typst — sidecar: sync remote/heavy queries into a reproducible cache"
)]
struct Cli {
    /// Project directory (holds quarry.toml, .quarry/ and the documents)
    #[arg(long, default_value = ".", global = true)]
    dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold quarry.toml and an empty .quarry cache
    Init,
    /// Discover queries in documents, execute them, refresh cache + lockfile
    Sync {
        /// Documents to scan (default: every top-level *.typ)
        docs: Vec<String>,
        /// Override the configured mode: strict | auto | offline
        #[arg(long)]
        mode: Option<String>,
        /// Also remove cache entries no document requests anymore
        #[arg(long)]
        prune: bool,
        /// Re-run sync whenever a document or quarry.toml changes
        #[arg(long)]
        watch: bool,
    },
    /// Show each cached entry: fresh / stale / orphaned
    Status,
    /// Check every manifest entry against its cache file digest
    Verify,
    /// Remove cache entries not requested by the given documents
    Prune { docs: Vec<String> },
    /// Show the query behind a cache key
    Explain { key: String },
    /// Scan for concatenated SQL and literal credentials (F-27)
    Lint,
    /// Compare two lockfiles (defaults: quarry.lock.orig vs quarry.lock)
    Diff {
        old: Option<String>,
        new: Option<String>,
    },
}

/// Restore default SIGPIPE behaviour so `quarry … | head` exits quietly like
/// every other Unix tool instead of panicking on a closed pipe.
#[cfg(unix)]
fn reset_sigpipe() {
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    unsafe {
        signal(13 /* SIGPIPE */, 0 /* SIG_DFL */);
    }
}
#[cfg(not(unix))]
fn reset_sigpipe() {}

fn main() {
    reset_sigpipe();
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("quarry: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let dir = cli.dir;
    match cli.command {
        Command::Init => {
            let path = dir.join("quarry.toml");
            if path.exists() {
                bail!("{} already exists", path.display());
            }
            std::fs::write(&path, config::INIT_TEMPLATE)?;
            cache::ensure_init(&dir, ".quarry")?;
            println!(
                "created {} and .quarry/ — declare sources, then run `quarry sync`",
                path.display()
            );
        }
        Command::Sync {
            docs,
            mode,
            prune,
            watch,
        } => {
            let run_once = || -> Result<()> {
                let (config, _) = config::Config::load(&dir)?;
                cache::ensure_init(&dir, &config.quarry.cache_dir)?;
                let doc_paths = sync::resolve_docs(&dir, &docs)?;
                let report = sync::sync(&dir, &config, &doc_paths, mode.as_deref(), prune)?;
                println!(
                    "sync: {} executed, {} kept{}{}",
                    report.executed,
                    report.kept,
                    if report.failed_sql > 0 {
                        format!(
                            ", {} SQL error(s) cached (catchable via qr.try-query)",
                            report.failed_sql
                        )
                    } else {
                        String::new()
                    },
                    if report.pruned.is_empty() {
                        String::new()
                    } else {
                        format!(", {} pruned", report.pruned.len())
                    },
                );
                print!("{}", report.drift);
                Ok(())
            };
            run_once()?;
            if watch {
                println!("watching for changes (Ctrl-C to stop)…");
                let mut last = scan_mtimes(&dir)?;
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                    let now = scan_mtimes(&dir)?;
                    if now != last {
                        last = now;
                        if let Err(e) = run_once() {
                            eprintln!("quarry: {e:#}");
                        }
                    }
                }
            }
        }
        Command::Status => {
            let (config, _) = config::Config::load(&dir)?;
            let store = cache::Cache::open(&dir, &config.quarry.cache_dir)?;
            let ttl = config.ttl()?;
            if store.entries.is_empty() {
                println!("cache is empty — run `quarry sync`");
                return Ok(());
            }
            println!("mode {}, generated {}", store.mode, store.generated);
            for entry in store.entries.values() {
                let fresh = cache::entry_is_fresh(entry, ttl);
                println!(
                    "  {} {} {:>8} rows  {}  {} [{}] {}",
                    if fresh { "fresh" } else { "STALE" },
                    if entry.ok { "ok " } else { "ERR" },
                    entry.rows,
                    entry.fetched,
                    entry.source,
                    entry.kind,
                    entry.key,
                );
            }
        }
        Command::Verify => {
            let (config, _) = config::Config::load(&dir)?;
            let store = cache::Cache::open(&dir, &config.quarry.cache_dir)?;
            let problems = store.verify()?;
            if problems.is_empty() {
                println!(
                    "verify: {} entr{} intact",
                    store.entries.len(),
                    if store.entries.len() == 1 { "y" } else { "ies" }
                );
            } else {
                for p in &problems {
                    eprintln!("  ✗ {p}");
                }
                bail!("{} problem(s) found", problems.len());
            }
        }
        Command::Prune { docs } => {
            let (config, _) = config::Config::load(&dir)?;
            let doc_paths = sync::resolve_docs(&dir, &docs)?;
            let requests = discover::discover_all(&config, &config.quarry.typst, &dir, &doc_paths)?;
            let wanted: std::collections::BTreeSet<String> =
                requests.into_iter().map(|(k, _)| k).collect();
            let mut store = cache::Cache::open(&dir, &config.quarry.cache_dir)?;
            let removed = store.prune(&wanted)?;
            store.write(&store.mode.clone(), &cache::now_iso())?;
            println!(
                "pruned {} entr{}",
                removed.len(),
                if removed.len() == 1 { "y" } else { "ies" }
            );
        }
        Command::Explain { key } => {
            let (config, _) = config::Config::load(&dir)?;
            let store = cache::Cache::open(&dir, &config.quarry.cache_dir)?;
            match store.entries.get(&key) {
                None => bail!("{key} is not in the manifest (see `quarry status`)"),
                Some(e) => {
                    println!("key      {}", e.key);
                    println!("source   {} [{}]", e.source, e.kind);
                    println!("sql      {}", e.sql);
                    if !e.params_repr.is_empty() {
                        println!("params   {}", e.params_repr);
                    }
                    println!("rows     {}", e.rows);
                    println!("ok       {}", e.ok);
                    println!("digest   {}", e.digest);
                    println!("fetched  {} ({} ms)", e.fetched, e.elapsed_ms);
                    println!("file     {}", store.envelope_path(&e.key).display());
                }
            }
        }
        Command::Lint => {
            let findings = lint::lint(&dir)?;
            if findings.is_empty() {
                println!("lint: clean");
            } else {
                for f in &findings {
                    println!("{}:{} [{}] {}", f.file, f.line, f.rule, f.message);
                }
                bail!("{} finding(s)", findings.len());
            }
        }
        Command::Diff { old, new } => {
            let old_path = old.unwrap_or_else(|| "quarry.lock.orig".into());
            let new_path = new.unwrap_or_else(|| "quarry.lock".into());
            let old_text = std::fs::read_to_string(dir.join(&old_path))?;
            let new_text = std::fs::read_to_string(dir.join(&new_path))?;
            print!("{}", cache::diff_lockfiles(&old_text, &new_text)?);
        }
    }
    Ok(())
}

fn scan_mtimes(dir: &std::path::Path) -> Result<Vec<(PathBuf, std::time::SystemTime)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let relevant = name == "quarry.toml"
            || path
                .extension()
                .map(|e| e == "typ" || e == "sql")
                .unwrap_or(false);
        if relevant {
            out.push((path.clone(), entry.metadata()?.modified()?));
        }
    }
    out.sort();
    Ok(out)
}
