//! Deterministic robustness sweep (the stable-toolchain cousin of the
//! cargo-fuzz targets, runnable in every environment): seeded mutations of a
//! real database plus the curated malformed corpus, through open + query.
//! Any panic or crash fails the build; errors are the expected outcome.
//!
//! Usage: cargo run --release -p quarry-difftest --bin fuzz_lite [-- ROUNDS]

use ciborium::Value;
use std::path::PathBuf;
use std::process::Command;

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
        (
            Value::Text("now".into()),
            Value::from(1_768_867_200_000_000i64),
        ),
        (Value::Text("seed".into()), Value::from(1)),
    ]))
}

fn request(sql: &str) -> Vec<u8> {
    encode(&Value::Map(vec![(
        Value::Text("sql".into()),
        Value::Text(sql.into()),
    )]))
}

/// xorshift64* — deterministic, dependency-free.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn try_open_and_probe(bytes: &[u8]) -> &'static str {
    match quarry_engine::Connection::open(&meta(), &[bytes]) {
        Err(_) => "rejected-at-open",
        Ok(conn) => {
            let probes = [
                "SELECT * FROM sqlite_schema",
                "SELECT count(*) FROM orders",
                "PRAGMA integrity_check",
                "SELECT * FROM orders ORDER BY note LIMIT 50",
            ];
            let mut all_ok = true;
            for sql in probes {
                let env: Value =
                    ciborium::from_reader(conn.query(&request(sql)).as_slice()).unwrap();
                let ok = env
                    .as_map()
                    .and_then(|m| m.iter().find(|(k, _)| k.as_text() == Some("ok")))
                    .and_then(|(_, v)| v.as_bool())
                    .unwrap_or(false);
                all_ok &= ok;
            }
            if all_ok {
                "served"
            } else {
                "errored-cleanly"
            }
        }
    }
}

fn main() {
    let rounds: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(400);
    let fixtures = repo_root().join("tests/fixtures/generated");
    if !fixtures.join("basic.sqlite").exists() {
        assert!(Command::new("python3")
            .arg(repo_root().join("tools/gen_fixtures.py"))
            .status()
            .unwrap()
            .success());
    }
    let base = std::fs::read(fixtures.join("basic.sqlite")).unwrap();

    // 1. the curated malformed corpus
    let mut outcomes: std::collections::BTreeMap<&str, usize> = Default::default();
    let malformed_dir = fixtures.join("malformed");
    for entry in std::fs::read_dir(&malformed_dir).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        *outcomes.entry(try_open_and_probe(&bytes)).or_default() += 1;
    }

    // 2. seeded structured mutations of a valid database
    let mut rng = Rng(0x51CA_F00D_2026_0730);
    for round in 0..rounds {
        let mut mutant = base.clone();
        match round % 5 {
            // byte flips scattered anywhere
            0 => {
                for _ in 0..1 + rng.below(32) {
                    let i = rng.below(mutant.len());
                    mutant[i] ^= (rng.next() as u8) | 1;
                }
            }
            // header-focused flips (the parser's most trusted region)
            1 => {
                for _ in 0..1 + rng.below(8) {
                    let i = rng.below(100.min(mutant.len()));
                    mutant[i] = rng.next() as u8;
                }
            }
            // truncation
            2 => {
                let cut = 1 + rng.below(mutant.len());
                mutant.truncate(cut);
            }
            // page-boundary swaps
            3 => {
                let pages = mutant.len() / 4096;
                if pages >= 2 {
                    let a = rng.below(pages) * 4096;
                    let b = rng.below(pages) * 4096;
                    if a != b {
                        for k in 0..4096 {
                            mutant.swap(a + k, b + k);
                        }
                    }
                }
            }
            // zero runs
            _ => {
                let start = rng.below(mutant.len());
                let len = (1 + rng.below(8192)).min(mutant.len() - start);
                for b in &mut mutant[start..start + len] {
                    *b = 0;
                }
            }
        }
        *outcomes.entry(try_open_and_probe(&mutant)).or_default() += 1;
    }

    // 3. adversarial SQL through the request path (parser robustness)
    let conn = quarry_engine::Connection::open(&meta(), &[&base]).unwrap();
    let nasty_sql = [
        "SELECT",
        "SELECT (((((((((",
        ";;;;;;;;",
        "WITH x AS (SELECT 1",
        "SELECT * FROM orders WHERE region = '",
        "'; DROP TABLE orders; --",
        "SELECT \u{0}",
        "SELECT x'GG'",
        "/**/",
        "--",
        "SELECT 1e999999",
        "SELECT 99999999999999999999999999999999",
        "SELECT ?999999",
        "SELECT :a:b:c",
        "PRAGMA \"",
        "SELECT \"unclosed",
    ];
    for sql in nasty_sql {
        let _ = conn.query(&request(sql));
    }
    for _ in 0..rounds {
        let len = rng.below(64);
        let garbage: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let _ = conn.query(&garbage);
    }

    println!(
        "fuzz-lite: {} malformed+mutant inputs, all handled without crashing",
        outcomes.values().sum::<usize>()
    );
    for (outcome, n) in &outcomes {
        println!("  {outcome:18} {n}");
    }
    println!("  adversarial SQL   {}", nasty_sql.len() + rounds);
}
