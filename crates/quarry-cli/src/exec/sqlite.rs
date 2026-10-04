//! SQLite through the sidecar: the same engine crate the plugin ships,
//! compiled natively — full speed, no size ceiling, byte-compatible envelopes.
//! This is the F-31 path for local databases that outgrow the in-WASM route.

use crate::config::Source;
use crate::exec::ExecOutcome;
use crate::requests::DiscoveredRequest;
use anyhow::{anyhow, Context, Result};
use ciborium::Value;

fn normalize_clock(envelope: Vec<u8>) -> Result<Vec<u8>> {
    let mut value: Value = ciborium::from_reader(envelope.as_slice())?;
    if let Value::Map(fields) = &mut value {
        for (k, v) in fields.iter_mut() {
            if k.as_text() == Some("stats") {
                if let Value::Map(stats) = v {
                    for (sk, sv) in stats.iter_mut() {
                        if sk.as_text() == Some("clock") {
                            *sv = Value::Map(vec![
                                (Value::Text("value".into()), Value::Null),
                                (Value::Text("origin".into()), Value::Text("sidecar".into())),
                            ]);
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    ciborium::into_writer(&value, &mut out)?;
    Ok(out)
}

pub fn execute(
    name: &str,
    source: &Source,
    request: &DiscoveredRequest,
    fetched_iso: &str,
) -> Result<ExecOutcome> {
    let path = source.path.as_deref().expect("validated");
    let bytes = std::fs::read(path).with_context(|| format!("source {name:?}: reading {path}"))?;

    // The sidecar legitimately has a wall clock: the fetch moment is the
    // injected time, and it is pinned in the cache + lockfile — materialized,
    // deterministic thereafter.
    let now = chrono::Utc::now();
    let meta = Value::Map(vec![
        (
            Value::Text("now".into()),
            Value::from(now.timestamp_micros()),
        ),
        (
            Value::Text("clock-origin".into()),
            Value::Text("sidecar".into()),
        ),
        (
            Value::Text("clock-display".into()),
            Value::Text(fetched_iso.to_string()),
        ),
        (Value::Text("seed".into()), Value::from(0)),
    ]);
    let mut meta_bytes = Vec::new();
    ciborium::into_writer(&meta, &mut meta_bytes)?;

    let conn = quarry_engine::Connection::open(&meta_bytes, &[&bytes])
        .map_err(|e| anyhow!("source {name:?}: {}", e.message))?;

    // Build the engine request with the source's row cap.
    let mut fields = vec![(Value::Text("sql".into()), Value::Text(request.sql.clone()))];
    if !request.params.is_null() {
        fields.push((Value::Text("params".into()), request.params.clone()));
    }
    fields.push((
        Value::Text("options".into()),
        Value::Map(vec![
            (Value::Text("max-rows".into()), Value::from(source.max_rows)),
            (Value::Text("max-bytes".into()), Value::Null),
        ]),
    ));
    let mut request_bytes = Vec::new();
    ciborium::into_writer(&Value::Map(fields), &mut request_bytes)?;

    let envelope = conn.query(&request_bytes);
    // Normalize the clock out of the stats (same reasoning as sidecar_stats):
    // the wall time was injected so date('now') is CORRECT in the data, but
    // the stats timestamp itself would churn every digest on every sync.
    let envelope = normalize_clock(envelope)?;
    let decoded: Value = ciborium::from_reader(envelope.as_slice())?;
    let get = |k: &str| {
        decoded
            .as_map()
            .and_then(|m| m.iter().find(|(key, _)| key.as_text() == Some(k)))
            .map(|(_, v)| v.clone())
    };
    let ok = get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let rows = get("rows")
        .and_then(|v| v.as_array().map(|a| a.len() as u64))
        .unwrap_or(0);
    Ok(ExecOutcome {
        envelope,
        rows,
        ok,
        elapsed_ms: 0,
        kind: "sqlite".into(),
    })
}
