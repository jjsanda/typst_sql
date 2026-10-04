//! A faithful replica of Typst's WASM plugin host, built on the same wasmi
//! version and configuration Typst uses.
//!
//! Purpose: run the actual `quarry-sqlite.wasm` artifact in `cargo test` with
//! Typst's exact semantics — instance pooling with dirty memory reuse,
//! whole-memory snapshots for `plugin.transition()`, restore-on-demand — so
//! the purity invariant (C-4) and the differential suite exercise the plugin
//! the way Typst will, without needing a Typst binary in the loop.

use anyhow::{anyhow, bail, Context, Result};
use std::sync::{Arc, Mutex};
use wasmi::{Caller, Engine, Instance, Linker, Memory, Module, Store, Val};

/// Store data: the argument buffer the guest reads and the result it writes.
#[derive(Default)]
struct CallData {
    args: Vec<u8>,
    result: Option<Vec<u8>>,
}

struct SharedModule {
    engine: Engine,
    module: Module,
    linker: Linker<CallData>,
}

/// Whole-linear-memory snapshot, exactly like Typst's `plugin.transition()`.
#[derive(Clone)]
pub struct Snapshot {
    mem_pages: u32,
    mem_data: Vec<u8>,
}

struct PluginInstance {
    store: Store<CallData>,
    instance: Instance,
    memory: Memory,
}

/// A plugin value: module + optional transition snapshot + instance pool.
pub struct Plugin {
    shared: Arc<SharedModule>,
    snapshot: Option<Snapshot>,
    pool: Mutex<Vec<PluginInstance>>,
}

impl Plugin {
    /// Load a compiled (and wasi-stubbed) plugin binary.
    pub fn new(bytes: &[u8]) -> Result<Plugin> {
        // Typst: wasmi Config::default() with relaxed SIMD disabled.
        let mut config = wasmi::Config::default();
        config.wasm_relaxed_simd(false);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, bytes).context("invalid wasm module")?;

        let mut linker: Linker<CallData> = Linker::new(&engine);
        linker
            .func_wrap(
                "typst_env",
                "wasm_minimal_protocol_write_args_to_buffer",
                |mut caller: Caller<'_, CallData>, ptr: u32| {
                    let memory = get_memory(&mut caller);
                    let args = std::mem::take(&mut caller.data_mut().args);
                    memory
                        .write(&mut caller, ptr as usize, &args)
                        .expect("write args");
                    caller.data_mut().args = args;
                },
            )
            .map_err(|e| anyhow!("linker: {e}"))?;
        linker
            .func_wrap(
                "typst_env",
                "wasm_minimal_protocol_send_result_to_host",
                |mut caller: Caller<'_, CallData>, ptr: u32, len: u32| {
                    let memory = get_memory(&mut caller);
                    let mut out = vec![0u8; len as usize];
                    memory
                        .read(&caller, ptr as usize, &mut out)
                        .expect("read result");
                    caller.data_mut().result = Some(out);
                },
            )
            .map_err(|e| anyhow!("linker: {e}"))?;

        // Refuse un-stubbed builds so tests always exercise the shipped
        // artifact shape (typst_env is the only import Typst provides).
        for import in module.imports() {
            if import.module() != "typst_env" {
                bail!(
                    "module imports {}::{} — run wasi-stub first (tests must use the shipped artifact)",
                    import.module(),
                    import.name()
                );
            }
        }

        Ok(Plugin {
            shared: Arc::new(SharedModule {
                engine,
                module,
                linker,
            }),
            snapshot: None,
            pool: Mutex::new(Vec::new()),
        })
    }

    /// Load the shipped build/quarry-sqlite.wasm from the repo root.
    pub fn load_shipped() -> Result<Plugin> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("build/quarry-sqlite.wasm");
        let bytes = std::fs::read(&path)
            .with_context(|| format!("{} missing — run tools/build_wasm.sh", path.display()))?;
        Plugin::new(&bytes)
    }

    fn fresh_instance(&self) -> Result<PluginInstance> {
        let mut store = Store::new(&self.shared.engine, CallData::default());
        let instance = self
            .shared
            .linker
            .instantiate_and_start(&mut store, &self.shared.module)
            .map_err(|e| anyhow!("instantiate: {e}"))?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| anyhow!("plugin exports no memory"))?;
        let mut pi = PluginInstance {
            store,
            instance,
            memory,
        };
        if let Some(snapshot) = &self.snapshot {
            restore(&mut pi, snapshot)?;
        }
        Ok(pi)
    }

    fn take_instance(&self) -> Result<PluginInstance> {
        if let Some(pi) = self.pool.lock().unwrap().pop() {
            return Ok(pi);
        }
        self.fresh_instance()
    }

    /// Call an export, Typst-style: concatenated arg bytes, one i32 length per
    /// arg, rc 0 ⇒ data, rc 1 ⇒ error message. A successful instance returns
    /// to the pool **dirty**, exactly like Typst; a failing one is dropped.
    pub fn call(&self, name: &str, args: &[&[u8]]) -> Result<Vec<u8>, String> {
        let (result, pi) = self.raw_call(name, args)?;
        self.pool.lock().unwrap().push(pi);
        result
    }

    fn raw_call(
        &self,
        name: &str,
        args: &[&[u8]],
    ) -> Result<(Result<Vec<u8>, String>, PluginInstance), String> {
        let mut pi = self
            .take_instance()
            .map_err(|e| format!("instance: {e:?}"))?;
        let func = pi
            .instance
            .get_func(&pi.store, name)
            .ok_or_else(|| format!("plugin function \"{name}\" not found"))?;

        let mut concat = Vec::new();
        let mut params = Vec::with_capacity(args.len());
        for a in args {
            concat.extend_from_slice(a);
            params.push(Val::I32(a.len() as i32));
        }
        pi.store.data_mut().args = concat;
        pi.store.data_mut().result = None;

        let mut results = [Val::I32(-1)];
        if let Err(trap) = func.call(&mut pi.store, &params, &mut results) {
            // Failing instances are dropped, never pooled (Typst semantics).
            return Err(format!("plugin call trapped: {trap}"));
        }
        let code = match results[0] {
            Val::I32(c) => c,
            _ => return Err("plugin returned a non-i32 code".into()),
        };
        let output = pi.store.data_mut().result.take().unwrap_or_default();
        match code {
            0 => Ok((Ok(output), pi)),
            1 => Ok((Err(String::from_utf8_lossy(&output).into_owned()), pi)),
            other => Err(format!("plugin returned unknown code {other}")),
        }
    }

    /// `plugin.transition(func, args)`: run the call, then snapshot the whole
    /// linear memory into a derived Plugin (C-3: one full-memory copy, kept).
    pub fn transition(&self, name: &str, args: &[&[u8]]) -> Result<Plugin, String> {
        let (result, pi) = self.raw_call(name, args)?;
        result?;
        let snapshot = snapshot(&pi);
        Ok(Plugin {
            shared: Arc::clone(&self.shared),
            snapshot: Some(snapshot),
            pool: Mutex::new(vec![pi]),
        })
    }

    /// Byte size of the transition snapshot, if any (Phase-0 measurements).
    pub fn snapshot_bytes(&self) -> Option<usize> {
        self.snapshot.as_ref().map(|s| s.mem_data.len())
    }

    /// Drop all pooled instances, forcing the next call to restore from the
    /// snapshot — the "parallel compilation" path in Typst.
    pub fn clear_pool(&self) {
        self.pool.lock().unwrap().clear();
    }

    /// Linear-memory size of the pooled instance, if one is pooled. Used by
    /// memory-plateau tests (D-10) and Phase-0 measurements.
    pub fn pooled_memory_bytes(&self) -> Option<usize> {
        let pool = self.pool.lock().unwrap();
        pool.last().map(|pi| pi.memory.data(&pi.store).len())
    }
}

fn get_memory(caller: &mut Caller<'_, CallData>) -> Memory {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .expect("plugin exports memory")
}

fn snapshot(pi: &PluginInstance) -> Snapshot {
    let mem_pages = pi.memory.size(&pi.store);
    let mem_data = pi.memory.data(&pi.store).to_vec();
    Snapshot {
        mem_pages: mem_pages as u32,
        mem_data,
    }
}

fn restore(pi: &mut PluginInstance, snapshot: &Snapshot) -> Result<()> {
    let current = pi.memory.size(&pi.store) as u32;
    if current < snapshot.mem_pages {
        pi.memory
            .grow(&mut pi.store, (snapshot.mem_pages - current) as u64)
            .map_err(|e| anyhow!("memory grow: {e}"))?;
    }
    let data = pi.memory.data_mut(&mut pi.store);
    data[..snapshot.mem_data.len()].copy_from_slice(&snapshot.mem_data);
    for b in data[snapshot.mem_data.len()..].iter_mut() {
        *b = 0;
    }
    Ok(())
}
