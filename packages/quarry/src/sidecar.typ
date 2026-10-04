// Document side of the sidecar flow (F-07/F-25/F-31):
//
//   quarry.toml declares sources → `quarry sync` executes queries natively →
//   .quarry/cache/<key>.cbor + .quarry/manifest.cbor + quarry.lock →
//   `typst compile` reads only the cache. No network, ever, at compile time.
//
// The cache key is computed by the SAME code on both sides: the document
// calls the plugin's cache_key export; the CLI calls the native build of the
// identical function. A key miss is a *catchable dictionary lookup* against
// the manifest (Typst's read() of a missing file is uncatchable, so the
// manifest — which quarry init creates — is the only file that must exist).
//
// Discovery: compiling with --input quarry-discover=1 turns every remote
// query into an empty placeholder envelope that carries its request, so a
// cold checkout compiles; renderers (and qr.declare) emit those requests as
// metadata under <quarry-request>, which `quarry sync` extracts via
// typst query/eval.

#import "core.typ": cache-key-request

#let discover-mode() = sys.inputs.at("quarry-discover", default: none) == "1"

#let _placeholder(request) = (
  version: 1,
  ok: true,
  columns: (),
  rows: (),
  stats: (
    "row-count": 0,
    truncated: false,
    source: request.at("source", default: "?"),
    statements: 0,
    clock: (value: none, origin: "none"),
    seed: none,
    engine: "placeholder",
    placeholder: true,
  ),
  warnings: (),
  request: request,
)

// The run hook core.typ calls for remote handles.
#let _remote-run(db, request) = {
  let sql = request.sql
  let key = {
    let req = request
    req.insert("source", db.source)
    cache-key-request(req)
  }
  let full-request = (
    version: 1,
    source: db.source,
    sql: sql,
    params: request.at("params", default: none),
    key: key,
  )
  if discover-mode() {
    return _placeholder(full-request)
  }
  let manifest = cbor(read(db.manifest-path, encoding: none))
  if manifest.at("version", default: 0) != 1 {
    panic("quarry: " + db.manifest-path + " has unsupported manifest version — update the quarry CLI and re-run quarry sync")
  }
  let entries = manifest.at("queries", default: (:))
  if key not in entries {
    panic(
      "quarry: no cached result for this query against source \"" + db.source + "\"\n"
        + "  │ " + sql.split("\n").map(l => l.trim()).join(" ") + "\n"
        + "  = key " + key + " is not in " + db.manifest-path + "\n"
        + "  = fix: run `quarry sync " + sys.inputs.at("quarry-doc", default: "<your-document>.typ") + "`",
    )
  }
  let entry = entries.at(key)
  let env = cbor(read(db.cache-dir + "/" + key + ".cbor", encoding: none))
  if env.at("version", default: 0) != 1 {
    panic("quarry: cache entry " + key + " has envelope version " + str(env.at("version", default: 0)) + "; expected 1 — re-run quarry sync")
  }
  if not env.ok {
    // The CLI caches errors too (so offline builds reproduce them); make the
    // failure catchable exactly like a live one.
    return env
  }
  env.insert("cached", (
    fetched: entry.at("fetched", default: none),
    digest: entry.at("digest", default: none),
    kind: entry.at("kind", default: none),
  ))
  env
}

/// A handle for a source declared in quarry.toml, resolved through the
/// sidecar cache. The document API is identical to qr.sqlite handles.
#let remote(name, cache-dir: "/.quarry") = {
  if type(name) != str or name.len() == 0 {
    panic("quarry: remote() expects the source name from quarry.toml")
  }
  (
    quarry: "remote",
    source: name,
    manifest-path: cache-dir + "/manifest.cbor",
    cache-dir: cache-dir + "/cache",
    run: _remote-run,
  )
}

/// Emit the discovery metadata for query results used only in code (charts,
/// inline values): place once anywhere in the document.
/// `#qr.declare(churn, monthly)` — a no-op outside discover mode.
#let declare(..envs) = {
  for env in envs.pos() {
    // tolerate try-query shapes, error envelopes and none — declare is a
    // no-op for anything that carries no request
    let env = if type(env) == dictionary and "value" in env and env.at("value") != none {
      env.value
    } else { env }
    if type(env) == dictionary {
      let request = env.at("request", default: none)
      if request != none {
        [#metadata(request) <quarry-request>]
      }
    }
  }
}

/// Render sync status for a debugging footer: which entries the manifest has.
#let sync-status(cache-dir: "/.quarry") = {
  let manifest = cbor(read(cache-dir + "/manifest.cbor", encoding: none))
  let entries = manifest.at("queries", default: (:))
  [
    #entries.len() cached quer#if entries.len() == 1 [y] else [ies],
    manifest generated #manifest.at("generated", default: "?")
  ]
}
