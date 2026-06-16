//! An **untrusted** Canopy guest as a real WebAssembly **Component**.
//!
//! This is the Component Model peer of `examples/full/plugin-counter` (the core-wasm
//! guest). Both run the *exact same* `canopy-core` [`Emitter`] code and produce the
//! *exact same* `canopy-protocol` op bytes — the only difference is the boundary they
//! cross to reach the host:
//!
//! * The core-wasm guest declares a raw import `canopy_apply(ptr, len)` and calls it
//!   through `unsafe extern "C"`, handing the host a pointer + length into its own
//!   linear memory.
//! * **This** guest is generated from `wit/canopy.wit` by [`wit_bindgen::generate!`].
//!   It implements the world's `run` export and calls the imported
//!   `host::apply(&[u8]) -> Result<(), ApplyError>`. The bytes cross as an owned
//!   `list<u8>` via the Component Model's canonical ABI — no raw pointer, and the
//!   host gets a typed `result` back, so a rejected batch is a value the guest can
//!   inspect rather than a silent failure.
//!
//! Crucially, the generated bindings link **only** what the world grants: the single
//! `host` interface. There is no WASI, no clock, no filesystem, no network — the guest
//! *structurally* cannot reach the OS, and a host that links only `host` (see
//! `canopy-transport-component`) will refuse to instantiate a component that asks for
//! anything more.
//!
//! [`Emitter`]: canopy_core::Emitter

// wit-bindgen's generated canonical-ABI glue is `unsafe` by construction — it is the
// one explicit FFI seam (this crate is excluded from the workspace `unsafe_code`
// lint, exactly like the core-wasm guest).
#![deny(unsafe_op_in_unsafe_fn)]

use canopy_core::Emitter;
use canopy_dom_root::ROOT;
use canopy_paint::{BG, DIRECTION, FG, GAP, HEIGHT, PADDING};
use canopy_protocol::ElementTag;

// Generate the guest bindings from the SAME `wit/canopy.wit` the host builds against.
// This produces:
//   * the `Guest` trait for the `canopy-guest` world (with `run`), which we implement,
//   * the imported `host` interface as a module (`apply`, `poll_events`), and
//   * the shared `types` (`ApplyError`, `Event`) — all under a `bindings` module.
// Nothing here imports WASI: the world grants only `host`, so that is all that links.
mod bindings {
    wit_bindgen::generate!({
        world: "canopy-guest",
        // Repo-root `wit/` — three dirs up from examples/full/component-guest.
        path: "../../../wit",
    });
}

use bindings::canopy::ui::host;

// The host root id (`NodeId::new(0)`) lives in `canopy-dom`, a host-side crate we do
// not want to link into the guest. Re-declare the one constant we need locally so the
// guest depends only on the no_std op-builder — same as the core-wasm guest.
mod canopy_dom_root {
    use canopy_protocol::NodeId;
    /// The implicit host root every top-level node is mounted under.
    pub const ROOT: NodeId = NodeId::new(0);
}

// Stand-in element kind. The real ids come from the host widget registry; any non-zero
// tag is fine because the host's M1 `Dom` does not interpret it beyond "this is an
// element".
const COLUMN: ElementTag = ElementTag::new(1);

/// The zero-sized type the `export!` macro wires the world's exports onto.
struct CanopyGuest;

impl bindings::Guest for CanopyGuest {
    /// Build the plugin's UI and hand it to the host.
    ///
    /// The component host calls this exported entry point once. Inside it we build a
    /// small Canopy op-batch — a styled column with three text lines, the same shape
    /// as the core-wasm `canopy-plugin-counter` guest — and pass it to the single
    /// granted import, `host::apply`. The host validates every handle and applies the
    /// batch to its retained tree; a rejection comes back as a typed `ApplyError`.
    fn run() {
        let mut e = Emitter::new();

        let column = e.create_element(COLUMN);
        e.append(ROOT, column);
        e.set_inline_style(column, DIRECTION, "column");
        e.set_inline_style(column, BG, "#181825");
        e.set_inline_style(column, PADDING, "12");
        e.set_inline_style(column, GAP, "6");

        let heading = e.create_text("component guest");
        e.append(column, heading);
        e.set_inline_style(heading, FG, "#a6e3a1");
        e.set_inline_style(heading, HEIGHT, "16");

        let l2 = e.create_text("untrusted wasm component");
        e.append(column, l2);
        e.set_inline_style(l2, FG, "#cdd6f4");
        e.set_inline_style(l2, HEIGHT, "16");

        let subtitle = e.create_text("built via wit-bindgen");
        e.append(column, subtitle);
        e.set_inline_style(subtitle, FG, "#6c7086");
        e.set_inline_style(subtitle, HEIGHT, "14");

        // Snapshot the batch and hand it to the host across the component boundary.
        // The canonical ABI copies the `list<u8>` out for us — no pointer math, no
        // lifetime juggling. We deliberately ignore an `Err` here: the host has
        // *recorded* the precise reason (and `canopy-transport-component`'s `run`
        // surfaces it), and an untrusted guest has no business deciding how the host
        // reports its own validation failure. A real guest could match on the result.
        let bytes = e.take_batch(0);
        let _ = host::apply(&bytes);
    }
}

// Register `CanopyGuest` as the implementation of the world's exports. This emits the
// component's exported `run` (and the canonical-ABI shims) — the counterpart of the
// core-wasm guest's `#[no_mangle] pub extern "C" fn run`.
bindings::export!(CanopyGuest with_types_in bindings);
