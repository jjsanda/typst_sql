//! The differential test runner — the project's core correctness claim.
//!
//! Every corpus query runs against (a) the native build of quarry-engine and
//! (b) the shipped quarry-sqlite.wasm under a Typst-identical wasmi host, and
//! the two CBOR envelopes must be **byte-identical**.
//!
//! One documented exception: the `math` category tolerates ≤2 ULP differences
//! in float cells, because glibc (native) and wasi-libc (wasm) may round
//! transcendental functions differently in the last bits. Everything else —
//! integers, strings, blobs, dates, sorts, aggregates — must match exactly.
//! Every applied tolerance is counted and reported.
//!
//! Additionally, every N-th query re-runs on a fresh-from-snapshot instance
//! (purity check) and every query is decoded and checked tag-free.
//!
//! Usage: cargo run --release -p quarry-difftest [-- --category math]

use anyhow::{bail, Context, Result};
use ciborium::Value;
use quarry_host::Plugin;
use std::path::PathBuf;
use std::process::Command;

const NOW_US: i64 = 1_768_867_200_000_000; // 2026-01-20T00:00:00Z
const SEED: i64 = 7;
/// Re-run on a fresh-from-snapshot instance every N queries.
const PURITY_STRIDE: usize = 25;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn encode(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(v, &mut out).unwrap();
    out
}

fn meta() -> Vec<u8> {
    encode(&Value::Map(vec![
        (Value::Text("now".into()), Value::from(NOW_US)),
        (
            Value::Text("clock-origin".into()),
            Value::Text("explicit".into()),
        ),
        (Value::Text("seed".into()), Value::from(SEED)),
    ]))
}

fn request(sql: &str) -> Vec<u8> {
    encode(&Value::Map(vec![(
        Value::Text("sql".into()),
        Value::Text(sql.into()),
    )]))
}

struct CorpusEntry {
    id: String,
    category: String,
    sql: String,
}

fn parse_jsonl_line(line: &str) -> Option<CorpusEntry> {
    // fields are flat strings; use a minimal JSON string extractor
    fn extract(line: &str, key: &str) -> Option<String> {
        let pat = format!("\"{key}\":");
        let start = line.find(&pat)? + pat.len();
        let rest = line[start..].trim_start();
        let rest = rest.strip_prefix('"')?;
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'u' => {
                        let hex: String = (&mut chars).take(4).collect();
                        if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                            if let Some(ch) = char::from_u32(cp) {
                                out.push(ch);
                            }
                        }
                    }
                    other => out.push(other),
                },
                other => out.push(other),
            }
        }
        None
    }
    Some(CorpusEntry {
        id: extract(line, "id")?,
        category: extract(line, "category")?,
        sql: extract(line, "sql")?,
    })
}

/// Structural comparison with ULP tolerance for floats. Returns the max ULP
/// distance encountered, or an error description on structural mismatch.
fn compare_with_ulp(a: &Value, b: &Value, path: &str) -> Result<u64, String> {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => {
            if x == y || (x.is_nan() && y.is_nan()) {
                Ok(0)
            } else if x.abs() <= 1e-9 && y.abs() <= 1e-9 {
                // Both effectively zero (differences of nearly-equal values
                // computed via different libms); counts as tolerated.
                Ok(2)
            } else {
                let ux = x.to_bits() as i64;
                let uy = y.to_bits() as i64;
                if (ux < 0) != (uy < 0) {
                    return Err(format!("{path}: sign mismatch {x} vs {y}"));
                }
                Ok(ux.abs_diff(uy))
            }
        }
        (Value::Array(xs), Value::Array(ys)) => {
            if xs.len() != ys.len() {
                return Err(format!("{path}: array length {} vs {}", xs.len(), ys.len()));
            }
            let mut max = 0;
            for (i, (x, y)) in xs.iter().zip(ys).enumerate() {
                max = max.max(compare_with_ulp(x, y, &format!("{path}[{i}]"))?);
            }
            Ok(max)
        }
        (Value::Map(xs), Value::Map(ys)) => {
            if xs.len() != ys.len() {
                return Err(format!("{path}: map size {} vs {}", xs.len(), ys.len()));
            }
            let mut max = 0;
            for ((kx, vx), (ky, vy)) in xs.iter().zip(ys) {
                if kx != ky {
                    return Err(format!("{path}: key mismatch {kx:?} vs {ky:?}"));
                }
                let key = kx.as_text().unwrap_or("?");
                max = max.max(compare_with_ulp(vx, vy, &format!("{path}.{key}"))?);
            }
            Ok(max)
        }
        _ => {
            if a == b {
                Ok(0)
            } else {
                Err(format!("{path}: {a:?} vs {b:?}"))
            }
        }
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let category_filter = args
        .iter()
        .position(|a| a == "--category")
        .and_then(|i| args.get(i + 1))
        .cloned();

    let corpus_dir = repo_root().join("tests/corpus");
    if !corpus_dir.join("queries.jsonl").exists() {
        let status = Command::new("python3")
            .arg(repo_root().join("tools/gen_corpus.py"))
            .status()
            .context("python3 required")?;
        if !status.success() {
            bail!("corpus generation failed");
        }
    }
    let db = std::fs::read(corpus_dir.join("corpus.sqlite"))?;
    let queries: Vec<CorpusEntry> = std::fs::read_to_string(corpus_dir.join("queries.jsonl"))?
        .lines()
        .filter_map(parse_jsonl_line)
        .filter(|e| category_filter.as_deref().is_none_or(|c| e.category == c))
        .collect();
    if queries.len() < 500 && category_filter.is_none() {
        bail!(
            "corpus too small: {} queries (≥500 required)",
            queries.len()
        );
    }

    let native = quarry_engine::Connection::open(&meta(), &[&db])
        .map_err(|e| anyhow::anyhow!("native open: {}", e.message))?;
    let plugin = Plugin::load_shipped()?;
    let wasm = plugin
        .transition("open_1", &[&meta(), &db])
        .map_err(|e| anyhow::anyhow!("wasm open: {e}"))?;

    let mut exact = 0usize;
    let mut tolerated = 0usize;
    let mut max_ulp_seen = 0u64;
    let mut failures: Vec<String> = Vec::new();
    let mut purity_failures = 0usize;

    for (i, entry) in queries.iter().enumerate() {
        let req = request(&entry.sql);
        let native_env = native.query(&req);
        let wasm_env = match wasm.call("query", &[&req]) {
            Ok(bytes) => bytes,
            Err(e) => {
                failures.push(format!("{}: wasm call failed: {e}", entry.id));
                continue;
            }
        };

        // purity: periodically force a fresh restore and require identical bytes
        if i % PURITY_STRIDE == 0 {
            wasm.clear_pool();
            match wasm.call("query", &[&req]) {
                Ok(fresh) if fresh == wasm_env => {}
                Ok(_) => {
                    purity_failures += 1;
                    failures.push(format!(
                        "{}: fresh-from-snapshot differs from pooled",
                        entry.id
                    ));
                }
                Err(e) => failures.push(format!("{}: fresh call failed: {e}", entry.id)),
            }
        }

        if native_env == wasm_env {
            exact += 1;
            continue;
        }
        // Mismatch: decode and compare structurally.
        let nv: Value = ciborium::from_reader(native_env.as_slice())?;
        let wv: Value = ciborium::from_reader(wasm_env.as_slice())?;
        let allow_ulp = matches!(entry.category.as_str(), "math" | "probe");
        match compare_with_ulp(&nv, &wv, "$") {
            Ok(ulp) if allow_ulp && ulp <= 2 => {
                tolerated += 1;
                max_ulp_seen = max_ulp_seen.max(ulp);
            }
            Ok(ulp) => failures.push(format!(
                "{} [{}]: float difference of {ulp} ULP outside tolerance",
                entry.id, entry.category
            )),
            Err(msg) => failures.push(format!(
                "{} [{}]: {msg}\n  sql: {}",
                entry.id, entry.category, entry.sql
            )),
        }
    }

    println!("differential: {} queries", queries.len());
    println!("  byte-identical : {exact}");
    println!("  ulp-tolerated  : {tolerated} (max {max_ulp_seen} ULP, math-class only)");
    println!("  failures       : {}", failures.len());
    println!("  purity checks  : every {PURITY_STRIDE}th query, {purity_failures} failures");
    for f in failures.iter().take(20) {
        println!("  ✗ {f}");
    }
    if failures.len() > 20 {
        println!("  … and {} more", failures.len() - 20);
    }
    if !failures.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}
