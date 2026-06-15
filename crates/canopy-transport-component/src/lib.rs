//! Canopy's **untrusted-component** transport: run a real WebAssembly Component
//! Model guest in wasmtime and let it drive the host UI through exactly one imported
//! capability, described by `wit/canopy.wit`.
//!
//! # What this adds over the core-wasm transport
//!
//! `canopy-transport-wasmtime` is the *core-module* peer: it loads a bare
//! `wasm32-unknown-unknown` module and grants it one raw import,
//! `env::canopy_apply(ptr: i32, len: i32)`, then does the bounds math by hand to read
//! the op bytes out of the guest's linear memory.
//!
//! This crate is the *Component Model* peer. The contract is the same single
//! capability, but it is expressed in the component's **type**: the world
//! [`canopy:ui/canopy-guest`](../../../wit/canopy.wit) imports only the interface
//! `host`, whose `apply: func(ops: list<u8>) -> result<_, apply-error>` carries its
//! bytes as an owned `list<u8>`. There is therefore:
//!
//! * **No raw pointer and no host-side bounds math.** The canonical ABI lifts the
//!   list into an owned `Vec<u8>` for us; an out-of-bounds access is impossible to
//!   express at this boundary. (Note: this crate contains *zero* `unsafe` as a
//!   result — it does not even need wasmtime's safe `Memory::read`.)
//! * **A type-checked capability surface.** The guest links against the generated
//!   bindings for the world; a component that tries to import anything else
//!   (WASI, a clock, a socket, …) fails to instantiate because [`Linker::instantiate`]
//!   only satisfies the one import we add. The threat model from the core-wasm
//!   transport is thus lifted into the component's type and enforced structurally.
//!
//! Everything *downstream* of the boundary is shared, byte-for-byte, with the other
//! transports: the op bytes are the same `canopy-protocol` wire format, and they are
//! validated and applied by the same [`canopy_dom::Dom`]. A forged node handle comes
//! back as [`HostError::BadHandle`]; undecodable bytes as [`HostError::Decode`]. The
//! host **never traps the guest** for bad ops — the WIT models the failure as
//! `result<_, apply-error>`, so [`apply`](Host::apply) returns the matching
//! [`ApplyError`] to the guest, and the original [`HostError`] is also recorded so
//! [`ComponentHost::run`] can surface it.
//!
//! # Capability model, mirrored
//!
//! | | core-wasm transport | **this (component) transport** |
//! |---|---|---|
//! | granted import | `env::canopy_apply(ptr,len)` | `host.apply(ops: list<u8>)` |
//! | reading bytes | manual bounds-check + `Memory::read` | owned `Vec<u8>` via canonical ABI |
//! | extra imports | none (no WASI) | none (no WASI) |
//! | bad ops | recorded `HostError` | `result::err(apply-error)` *and* recorded |
//! | applied to | `canopy_dom::Dom` | `canopy_dom::Dom` |
//!
//! [`Linker::instantiate`]: wasmtime::component::Linker::instantiate

use std::fmt;

use canopy_dom::Dom;
use canopy_traits::{HostError, OpSink};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

/// Generate the typed host bindings (and the import/export glue) from the SAME
/// `wit/canopy.wit` the guest builds against. This is the host side of the
/// cross-language thesis: guest and host share one interface artifact.
///
/// `bindgen!` produces:
///   * `CanopyGuest` — the bindings struct, with `CanopyGuest::instantiate` and a
///     `call_run` method for the world's `export run: func()`.
///   * `CanopyGuestImports` / the `host` interface's `Host` trait — the import
///     surface we must implement to grant the one capability.
///   * The generated `types::{Event, ApplyError}` matching the WIT.
///
/// The generated `apply` returns `Result<(), ApplyError>` where [`ApplyError`] is the
/// generated mirror of the WIT `apply-error` enum (the error arm of the WIT
/// `result<_, apply-error>`). We map our host-side [`HostError`] onto it. We do *not*
/// use `trappable_error_type`/`trappable_imports`: a rejected op is a *modeled* error
/// the guest must handle, never a host trap, so the plain `result` arm is exactly the
/// contract we want.
mod bindings {
    wasmtime::component::bindgen!({
        world: "canopy-guest",
        path: "../../wit",
    });
}

// The generated world bindings and the two import traits. `host::Host` is the trait
// for the granted `host` interface (`apply` / `poll_events`); `types::Host` is the
// empty marker trait the shared `types` interface generates. We must implement BOTH
// on the store state, because `CanopyGuest::add_to_linker` is bounded on both.
use bindings::canopy::ui::host::Host as HostInterface;
use bindings::canopy::ui::types::{ApplyError, Event, Host as TypesInterface};
use bindings::CanopyGuest;

/// Re-export the generated WIT enum as this crate's public `apply` error. It is the
/// error arm of the WIT `result<_, apply-error>` that crosses the component boundary;
/// [`host_error_to_apply_error`] maps the host's internal reason onto it. We use the
/// *generated* type directly (rather than a hand-rolled mirror) so the public surface
/// and the boundary representation can never drift — there is a single source of
/// truth, the WIT.
pub use bindings::canopy::ui::types::ApplyError as ComponentApplyError;

/// Map the host's internal [`HostError`] onto the WIT-facing [`ApplyError`]. (The
/// `too-large` arm has no `HostError` counterpart — it is the transport's own size
/// guard — so it is produced directly at the guard site, not here.)
fn host_error_to_apply_error(e: HostError) -> ApplyError {
    match e {
        HostError::BadHandle => ApplyError::BadHandle,
        HostError::Decode => ApplyError::Decode,
        HostError::Unsupported => ApplyError::Unsupported,
    }
}

/// Hard cap on a single `apply` batch, mirroring the core-wasm transport. A list
/// longer than this is rejected as [`ApplyError::TooLarge`] *before* the bytes are
/// decoded — the host never sizes downstream work from an untrusted length even
/// though the canonical ABI has already (safely) allocated the list.
pub const MAX_BATCH_BYTES: usize = 1 << 20; // 1 MiB

/// A failure running an untrusted component guest.
#[derive(Debug)]
pub enum ComponentError {
    /// The bytes were not a valid component, or it imported something we do NOT grant
    /// (e.g. WASI). With only `host` linked, any extra import fails instantiation, so
    /// the guest structurally cannot reach the OS.
    Instantiate(String),
    /// The guest trapped (an explicit trap, or — once caps are wired — a resource
    /// limit). Distinct from an [`ComponentError::Apply`], which is a *handled* op
    /// rejection, not a crash.
    Trap(String),
    /// A batch the guest handed to `apply` was rejected by the host: a forged handle
    /// or undecodable/oversized bytes. The boundary held; this is a clean error.
    Apply(HostError),
}

impl fmt::Display for ComponentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ComponentError::Instantiate(s) => write!(f, "component instantiation failed: {s}"),
            ComponentError::Trap(s) => write!(f, "component trapped: {s}"),
            ComponentError::Apply(e) => write!(f, "component op batch rejected by host: {e}"),
        }
    }
}

impl std::error::Error for ComponentError {}

impl From<HostError> for ComponentError {
    fn from(e: HostError) -> Self {
        ComponentError::Apply(e)
    }
}

/// Per-store host state, moved into the [`Store`] for the duration of a run.
///
/// It holds the retained tree the guest drives, plus the first [`HostError`] captured
/// inside an `apply` call. We record it (rather than relying solely on the value
/// returned to the guest) so [`ComponentHost::run`] surfaces the precise host reason
/// even if the guest swallows the `result::err` it gets back.
struct ComponentState {
    dom: Dom,
    apply_error: Option<HostError>,
}

/// The shared `types` interface generates an EMPTY marker trait (it declares only
/// data types, no functions). We must still implement it because the generated
/// `add_to_linker` is bounded on `types::Host + host::Host`. There is nothing to do.
impl TypesInterface for ComponentState {}

/// Implement the ONE granted capability: the generated `host` interface. This is the
/// guest's *entire* host surface — `apply` and `poll-events` — and nothing else is
/// added to the [`Linker`], so a component importing anything more cannot instantiate.
impl HostInterface for ComponentState {
    /// Validate and apply one batch of `canopy-protocol` op bytes.
    ///
    /// The `ops` arrive as an owned `Vec<u8>` (the canonical ABI already lifted the
    /// `list<u8>` out of guest memory — no pointer, no bounds math here). We:
    ///   1. reject an oversized batch as [`ApplyError::TooLarge`] before decoding, and
    ///   2. apply the rest through [`Dom`]'s [`OpSink`], which validates every node
    ///      handle (forged => [`HostError::BadHandle`]) and decodes the bytes
    ///      (garbage => [`HostError::Decode`]).
    ///
    /// On any failure the host records the [`HostError`] and returns the matching
    /// [`ApplyError`] to the guest — it never traps the guest for bad ops. The outer
    /// `wasmtime::Result` is reserved for a genuine host trap, which this never does.
    fn apply(&mut self, ops: Vec<u8>) -> Result<(), ApplyError> {
        // Bound the batch before doing any decoding work, mirroring the core-wasm
        // transport's `MAX_BATCH_BYTES` guard. Record it as a Decode-class host error.
        if ops.len() > MAX_BATCH_BYTES {
            if self.apply_error.is_none() {
                self.apply_error = Some(HostError::Decode);
            }
            return Err(ApplyError::TooLarge);
        }

        match self.dom.apply(&ops) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Keep the first failure (later calls this run are ignored for
                // reporting), then hand the guest the modeled error.
                if self.apply_error.is_none() {
                    self.apply_error = Some(e);
                }
                Err(host_error_to_apply_error(e))
            }
        }
    }

    /// Drain pending host->guest input events. M1 has no input source wired into this
    /// transport yet, so this is always empty — the capability exists in the type, and
    /// the host honestly returns "nothing pending" rather than fabricating events.
    fn poll_events(&mut self) -> Vec<Event> {
        Vec::new()
    }
}

/// Owns a [`Dom`] and runs untrusted **component** guests against it. See the crate
/// docs for the full capability/threat model.
pub struct ComponentHost {
    engine: Engine,
    linker: Linker<ComponentState>,
    /// The host's retained tree. Moved into the [`Store`] for the duration of a run so
    /// the sandboxed guest can drive it through `apply`, and moved back out after, so
    /// [`ComponentHost::dom`] can inspect the result.
    dom: Dom,
}

impl ComponentHost {
    /// Build a host configured for the component model, with exactly one import
    /// (`host`) added to the linker. No WASI, no clock, no filesystem.
    ///
    /// Returns [`ComponentError::Instantiate`] only if the (fixed, internal) host-
    /// import wiring fails to register, which would be a bug in this crate.
    pub fn new() -> Result<Self, ComponentError> {
        let mut config = Config::new();
        // Enable the Component Model. (`wasm_component_model` is the runtime switch
        // that pairs with the `component-model` cargo feature.)
        config.wasm_component_model(true);

        let engine = Engine::new(&config)
            .map_err(|e| ComponentError::Instantiate(format!("engine: {e}")))?;

        let mut linker: Linker<ComponentState> = Linker::new(&engine);
        // Grant ONLY the `host` interface. `add_to_linker` wires `apply`/`poll-events`
        // by projecting the store's `ComponentState` to our `Host` impl. Nothing else
        // is added, so the guest has no other authority to reach.
        CanopyGuest::add_to_linker(&mut linker, |state: &mut ComponentState| state)
            .map_err(|e| ComponentError::Instantiate(format!("linking host import: {e}")))?;

        Ok(Self {
            engine,
            linker,
            dom: Dom::new(),
        })
    }

    /// Instantiate `component_bytes` (a real wasm **component**, not a core module)
    /// against the sandbox and call its exported `run`. The guest drives the host
    /// [`Dom`] only through the `host.apply` import.
    ///
    /// Errors:
    /// * [`ComponentError::Instantiate`] if the bytes are not a valid component, or it
    ///   imports anything beyond the granted `host` interface (no WASI to satisfy).
    /// * [`ComponentError::Trap`] if the guest traps while running `run`.
    /// * [`ComponentError::Apply`] if a batch the guest emitted was rejected by the
    ///   host (forged handle / undecodable / oversized). The sandbox held.
    pub fn run(&mut self, component_bytes: &[u8]) -> Result<(), ComponentError> {
        let component = Component::new(&self.engine, component_bytes)
            .map_err(|e| ComponentError::Instantiate(describe(&e)))?;

        // Move the host Dom into the store so the guest can drive it. Each run starts
        // from a fresh tree; the previous one is replaced.
        let state = ComponentState {
            dom: std::mem::take(&mut self.dom),
            apply_error: None,
        };
        let mut store = Store::new(&self.engine, state);

        let outcome = self.run_in_store(&mut store, &component);

        // Reclaim the (possibly partially-mutated) Dom regardless of outcome.
        self.dom = store.into_data().dom;
        outcome
    }

    /// Instantiate and invoke `run` within an already-prepared store. Split out so the
    /// store's `ComponentState` (and thus the `Dom`) can be reclaimed on every path.
    fn run_in_store(
        &self,
        store: &mut Store<ComponentState>,
        component: &Component,
    ) -> Result<(), ComponentError> {
        // Instantiation only succeeds if every import the component declares is
        // satisfied. We added exactly `host`, so a component asking for WASI (or any
        // other interface) fails right here — the OS stays unreachable.
        let bindings = CanopyGuest::instantiate(&mut *store, component, &self.linker)
            .map_err(|e| ComponentError::Instantiate(describe(&e)))?;

        // Call the world's exported `run`. A trap surfaces as `ComponentError::Trap`;
        // a clean run may still have *recorded* an `apply` rejection, surfaced next.
        bindings
            .call_run(&mut *store)
            .map_err(|e| ComponentError::Trap(describe(&e)))?;

        if let Some(e) = store.data().apply_error {
            return Err(ComponentError::Apply(e));
        }
        Ok(())
    }

    /// The host's retained tree, reflecting whatever the last run applied.
    pub fn dom(&self) -> &Dom {
        &self.dom
    }
}

impl Default for ComponentHost {
    fn default() -> Self {
        Self::new().expect("host import wiring is fixed and must register")
    }
}

/// Render a wasmtime error with its cause chain, so "unknown import" instantiation
/// failures are legible (the root cause names the unsatisfied interface).
fn describe(e: &wasmtime::Error) -> String {
    let mut s = format!("{e}");
    for cause in e.chain().skip(1) {
        s.push_str(&format!("; caused by: {cause}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapping from the host's internal [`HostError`] onto the WIT-facing
    /// [`ApplyError`] is total and stable. Pure host logic; needs no component
    /// toolchain.
    #[test]
    fn host_error_maps_to_apply_error() {
        assert_eq!(
            host_error_to_apply_error(HostError::BadHandle),
            ApplyError::BadHandle
        );
        assert_eq!(
            host_error_to_apply_error(HostError::Decode),
            ApplyError::Decode
        );
        assert_eq!(
            host_error_to_apply_error(HostError::Unsupported),
            ApplyError::Unsupported
        );
    }

    /// Building a host wires the single import without error; nothing else is linked.
    #[test]
    fn host_constructs() {
        let host = ComponentHost::new().expect("host builds");
        assert_eq!(host.dom().node_count(), 0, "fresh host has an empty tree");
    }

    /// The size guard rejects an oversized batch as `TooLarge` *before* decoding and
    /// records a host error — exercised directly against the `Host` impl so it needs
    /// no component. Proves the host never sizes work from an untrusted length.
    #[test]
    fn oversized_batch_is_rejected_before_decode() {
        let mut state = ComponentState {
            dom: Dom::new(),
            apply_error: None,
        };
        let huge = vec![0u8; MAX_BATCH_BYTES + 1];
        let err = state.apply(huge).expect_err("oversized must be rejected");
        assert_eq!(err, ApplyError::TooLarge);
        assert_eq!(state.apply_error, Some(HostError::Decode));
        assert_eq!(state.dom.node_count(), 0, "nothing was applied");
    }

    /// A forged op-batch (mutating a node the guest never created) is a *handled*
    /// rejection: the `Host::apply` impl returns `BadHandle` and records the
    /// `HostError`, never panicking. This proves the bytes are untrusted exactly like
    /// the core-wasm transport, without needing a built component.
    #[test]
    fn forged_handle_is_handled_not_panic() {
        use canopy_core::Emitter;

        let mut e = Emitter::new();
        // Burn handles so the forged id is high and definitely absent.
        for _ in 0..50 {
            e.alloc_node();
        }
        let ghost = e.alloc_node();
        e.set_text(ghost, "haxx");
        let forged = e.take_batch(0);

        let mut state = ComponentState {
            dom: Dom::new(),
            apply_error: None,
        };
        let err = state
            .apply(forged)
            .expect_err("forged handle must be rejected");
        assert_eq!(err, ApplyError::BadHandle);
        assert_eq!(state.apply_error, Some(HostError::BadHandle));
        assert_eq!(state.dom.node_count(), 0);
    }

    /// A valid batch applies cleanly through the same `Host::apply` path the component
    /// would drive, leaving the expected tree in the host `Dom`.
    #[test]
    fn valid_batch_populates_dom() {
        use canopy_core::Emitter;
        use canopy_dom::ROOT;
        use canopy_protocol::ElementTag;

        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let label = e.create_text("hello component");
        e.append(col, label);
        let batch = e.take_batch(0);

        let mut state = ComponentState {
            dom: Dom::new(),
            apply_error: None,
        };
        state.apply(batch).expect("valid batch applies");
        assert_eq!(state.apply_error, None);
        assert_eq!(state.dom.node_count(), 2);
        let roots = state.dom.children(ROOT);
        assert_eq!(roots.len(), 1);
        let kids = state.dom.children(roots[0]);
        assert_eq!(state.dom.text_of(kids[0]), Some("hello component"));
    }
}
