//! Canopy's **untrusted-plugin** transport: run a wasm32 guest in a wasmtime
//! sandbox and let it drive the host UI through exactly one granted import.
//!
//! # Threat model
//!
//! This crate is the *runtime-enforced* peer of `canopy-transport-native`. The native
//! transport trusts a compiled-in guest and enforces the capability boundary at
//! **compile time** (the guest can only call the host APIs it was linked against).
//! Here the guest is an **untrusted** `.wasm` blob, so the same boundary is enforced
//! at **runtime** by the sandbox. Concretely:
//!
//! * **One granted import.** The [`wasmtime::Linker`] exposes a single host function,
//!   `env::canopy_apply(ptr: i32, len: i32)`. That is the guest's *entire* interface
//!   to the host. There is **no WASI**: no `wasi_snapshot_preview1`, no filesystem,
//!   clock, randomness, or network. A module that imports anything else fails to
//!   instantiate, so the guest structurally cannot reach the OS.
//! * **Bounded reads.** `canopy_apply` never trusts a guest-controlled size for a
//!   host allocation: `len` is rejected if it exceeds [`MAX_BATCH_BYTES`], and the
//!   bytes are pulled out of the guest's exported `memory` with wasmtime's *safe*,
//!   bounds-checked [`wasmtime::Memory::read`]. An out-of-bounds pointer/length is a
//!   handled error, never a panic or a host out-of-bounds read.
//! * **Host-side handle validation.** The bytes are applied to the host's
//!   [`canopy_dom::Dom`] via the [`canopy_traits::OpSink`] trait. The `Dom` mints and
//!   validates every node handle: a forged batch that names a node the guest never
//!   created is rejected with [`canopy_traits::HostError::BadHandle`]. Decode and
//!   handle errors are *recorded*, not propagated as host panics.
//! * **Resource caps.** The engine enables [`wasmtime::Config::epoch_interruption`]
//!   and [`wasmtime::Config::consume_fuel`]; the store caps linear memory at
//!   [`MAX_MEMORY_BYTES`] via a [`wasmtime::StoreLimits`] limiter, is given a finite
//!   [`FUEL_BUDGET`], and runs under an epoch deadline. A runaway guest (infinite
//!   loop, unbounded allocation) is **interrupted**, not left to hang or OOM the host.
//!
//! The wire format is identical to the native transport's: both carry the same
//! `canopy-protocol` op bytes. Only the delivery mechanism and the trust model
//! differ.

use std::fmt;

use canopy_dom::Dom;
use canopy_traits::{HostError, OpSink};
use wasmtime::{
    Caller, Config, Engine, Error as WasmError, Linker, Module, Store, StoreLimits,
    StoreLimitsBuilder, Trap,
};

/// Hard cap on a single `canopy_apply` batch. A guest-supplied length over this is
/// rejected outright — the host never sizes an allocation from an untrusted number.
pub const MAX_BATCH_BYTES: usize = 1 << 20; // 1 MiB

/// Hard cap on the guest's linear memory, enforced by the store limiter.
pub const MAX_MEMORY_BYTES: usize = 16 << 20; // 16 MiB

/// Fuel handed to a single [`PluginHost::run`]. Each wasm operation consumes fuel;
/// exhausting it traps the guest so a compute-bound loop cannot hang the host.
pub const FUEL_BUDGET: u64 = 50_000_000;

/// Epoch ticks the guest may run before the deadline interrupts it. The host bumps
/// the engine epoch once before each run, so a deadline of 1 means "this run only".
const EPOCH_DEADLINE: u64 = 1;

/// Per-store host state, moved into the [`Store`] for the duration of a run: the
/// retained tree the guest drives, the resource limiter, and any error captured
/// inside the `canopy_apply` host call (a host call cannot return a `Result` to the
/// guest, so errors are stashed here and surfaced by [`PluginHost::run`]).
struct HostState {
    dom: Dom,
    limits: StoreLimits,
    apply_error: Option<HostError>,
}

/// A failure running an untrusted plugin.
#[derive(Debug)]
pub enum PluginError {
    /// The module could not be compiled or instantiated (e.g. it imported something
    /// other than the single granted `canopy_apply` — there is no WASI to satisfy).
    Instantiate(String),
    /// The guest trapped: an explicit trap, an out-of-bounds access, or — crucially —
    /// running out of [`FUEL_BUDGET`] or past the epoch deadline (a runaway guest).
    Trap(String),
    /// The module has no exported `run` function to call.
    MissingRun,
    /// The module has no exported `memory`, so `canopy_apply` has nothing to read.
    MissingMemory,
    /// The guest's op batch was applied but the host rejected it (a forged/unknown
    /// handle, or undecodable bytes). The boundary held: this is a handled error, not
    /// a host crash.
    Apply(HostError),
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginError::Instantiate(s) => write!(f, "plugin instantiation failed: {s}"),
            PluginError::Trap(s) => write!(f, "plugin trapped: {s}"),
            PluginError::MissingRun => f.write_str("plugin exports no `run` function"),
            PluginError::MissingMemory => f.write_str("plugin exports no `memory`"),
            PluginError::Apply(e) => write!(f, "plugin op batch rejected by host: {e}"),
        }
    }
}

impl std::error::Error for PluginError {}

impl From<HostError> for PluginError {
    fn from(e: HostError) -> Self {
        PluginError::Apply(e)
    }
}

/// Owns a [`Dom`] and runs untrusted wasm guests against it inside a wasmtime
/// sandbox. See the crate docs for the full threat model.
pub struct PluginHost {
    engine: Engine,
    linker: Linker<HostState>,
    /// The host's retained tree. It is moved into the [`Store`] for the duration of a
    /// run (so the sandboxed guest can mutate it through `canopy_apply`) and moved
    /// back out when the run returns, so [`PluginHost::dom`] can inspect the result.
    dom: Dom,
}

impl PluginHost {
    /// Build a host with the sandbox configured: epoch interruption + fuel metering
    /// on the engine, and exactly one host import (`env::canopy_apply`) on the linker.
    ///
    /// Returns an error only if the (fixed, internal) host-import wiring fails to
    /// register, which would be a bug in this crate.
    pub fn new() -> Result<Self, PluginError> {
        let mut config = Config::new();
        config.epoch_interruption(true);
        config.consume_fuel(true);
        // Core modules only. The `threads` wasmtime feature is not even compiled in
        // (see Cargo.toml), so a guest cannot ask for shared memory.

        let engine =
            Engine::new(&config).map_err(|e| PluginError::Instantiate(format!("engine: {e}")))?;

        let mut linker: Linker<HostState> = Linker::new(&engine);
        Self::link_host_imports(&mut linker)?;

        Ok(Self {
            engine,
            linker,
            dom: Dom::new(),
        })
    }

    /// Register the ONE import the guest may call. Nothing else is linked — no WASI,
    /// no clock, no randomness, no filesystem.
    fn link_host_imports(linker: &mut Linker<HostState>) -> Result<(), PluginError> {
        linker
            .func_wrap(
                "env",
                "canopy_apply",
                |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| {
                    // Already recorded an error this run? Ignore further calls so the
                    // first failure is the one surfaced.
                    if caller.data().apply_error.is_some() {
                        return;
                    }
                    if let Err(e) = host_apply(&mut caller, ptr, len) {
                        caller.data_mut().apply_error = Some(e);
                    }
                },
            )
            .map_err(|e| PluginError::Instantiate(format!("linking canopy_apply: {e}")))?;
        Ok(())
    }

    /// Instantiate `wasm` against the sandbox and call its exported `run`, with fuel
    /// and the epoch deadline active. The guest drives the host [`Dom`] only through
    /// `canopy_apply`.
    ///
    /// Errors:
    /// * [`PluginError::Instantiate`] if the module imports anything beyond the single
    ///   granted host function (there is no WASI to satisfy extra imports), or fails
    ///   to compile.
    /// * [`PluginError::Trap`] if the guest traps — including exhausting its fuel or
    ///   epoch deadline (a runaway guest is interrupted, not hung).
    /// * [`PluginError::Apply`] if a batch the guest emitted was rejected by the host
    ///   (forged handle / undecodable bytes). The sandbox held; this is a clean error.
    pub fn run(&mut self, wasm: &[u8]) -> Result<(), PluginError> {
        let module = Module::new(&self.engine, wasm)
            .map_err(|e| PluginError::Instantiate(format!("compile: {e}")))?;

        let limits = StoreLimitsBuilder::new()
            .memory_size(MAX_MEMORY_BYTES)
            .memories(1)
            .instances(1)
            .build();

        // Move the host Dom into the store so the sandboxed guest can drive it. Start
        // each run from a fresh tree; the previous one is replaced.
        let state = HostState {
            dom: std::mem::take(&mut self.dom),
            limits,
            apply_error: None,
        };

        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);

        // Fuel: a finite budget; running out traps the guest.
        store
            .set_fuel(FUEL_BUDGET)
            .map_err(|e| PluginError::Instantiate(format!("set_fuel: {e}")))?;

        // Epoch: bump the engine epoch and set a deadline so a guest that ignores fuel
        // accounting is still cut off.
        self.engine.increment_epoch();
        store.set_epoch_deadline(EPOCH_DEADLINE);

        let outcome = self.run_in_store(&mut store, &module);

        // Reclaim the (possibly partially-mutated) Dom regardless of outcome.
        self.dom = store.into_data().dom;
        outcome
    }

    /// Instantiate and invoke `run` within an already-prepared store. Split out so the
    /// store's `HostState` (and thus the `Dom`) can be reclaimed by the caller on
    /// every path, including errors.
    fn run_in_store(
        &self,
        store: &mut Store<HostState>,
        module: &Module,
    ) -> Result<(), PluginError> {
        let instance = self
            .linker
            .instantiate(&mut *store, module)
            .map_err(|e| PluginError::Instantiate(describe_err(&e)))?;

        // The guest must export `memory` (so `canopy_apply` can read it) and `run`.
        if instance.get_memory(&mut *store, "memory").is_none() {
            return Err(PluginError::MissingMemory);
        }
        let run = instance
            .get_typed_func::<(), ()>(&mut *store, "run")
            .map_err(|_| PluginError::MissingRun)?;

        run.call(&mut *store, ()).map_err(|e| classify_trap(&e))?;

        // Surface any error the host recorded inside `canopy_apply`.
        if let Some(e) = store.data().apply_error {
            return Err(PluginError::Apply(e));
        }
        Ok(())
    }

    /// The host's retained tree, reflecting whatever the last run applied.
    pub fn dom(&self) -> &Dom {
        &self.dom
    }
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new().expect("host import wiring is fixed and must register")
    }
}

/// Read `len` bytes at `ptr` from the guest's exported `memory` and apply them to the
/// host [`Dom`]. All failure modes are returned as [`HostError`]; this never panics.
fn host_apply(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> Result<(), HostError> {
    // Reject nonsensical or oversized lengths before touching memory. Never size a
    // host buffer from an untrusted number.
    if len < 0 || ptr < 0 {
        return Err(HostError::Decode);
    }
    let len = len as usize;
    if len > MAX_BATCH_BYTES {
        return Err(HostError::Decode);
    }
    let ptr = ptr as usize;

    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or(HostError::Decode)?;

    // Safe, bounds-checked read out of guest linear memory into a host buffer. An
    // out-of-bounds pointer/length returns Err — not a panic, not a host OOB read.
    let mut buf = vec![0u8; len];
    memory
        .read(&caller, ptr, &mut buf)
        .map_err(|_| HostError::Decode)?;

    // Apply via OpSink: the Dom validates every handle (forged handle => BadHandle)
    // and decodes the bytes (garbage => Decode).
    caller.data_mut().dom.apply(&buf)
}

/// Turn a wasmtime call error into the right [`PluginError`], teasing apart a fuel /
/// epoch interruption (the runaway-guest case) from an ordinary trap.
fn classify_trap(e: &WasmError) -> PluginError {
    if let Some(trap) = e.downcast_ref::<Trap>() {
        return PluginError::Trap(format!("{trap}"));
    }
    PluginError::Trap(describe_err(e))
}

fn describe_err(e: &WasmError) -> String {
    // Include the cause chain so "unknown import" instantiation failures are legible.
    let mut s = format!("{e}");
    for cause in e.chain().skip(1) {
        s.push_str(&format!("; caused by: {cause}"));
    }
    s
}
