//! An **untrusted** Canopy guest, compiled to `wasm32-unknown-unknown`.
//!
//! This is the runtime-enforced peer of a compiled-in native guest: the exact same
//! `canopy-core` [`Emitter`] code, but here it runs inside a wasmtime sandbox. The
//! guest cannot touch the host's retained tree directly — its *entire* contract with
//! the outside world is one import, [`canopy_apply`], which copies a batch of
//! `canopy-protocol` op bytes across the sandbox boundary for the host to validate
//! and apply. No WASI, no filesystem, no clock, no network: the module structurally
//! imports nothing else.
//!
//! [`Emitter`]: canopy_core::Emitter

// The single host import is, by definition, an FFI call, so this guest opts out of
// the workspace-wide `unsafe_code = "deny"` (it is excluded from the workspace).
#![deny(unsafe_op_in_unsafe_fn)]

use canopy_core::Emitter;
use canopy_dom_root::ROOT;
use canopy_paint::{BG, DIRECTION, FG, GAP, HEIGHT, PADDING};
use canopy_protocol::ElementTag;

// The host root id (`NodeId::new(0)`) lives in `canopy-dom`, a host-side crate we do
// not want to link into the guest. Re-declare the one constant we need locally so the
// guest depends only on the no_std op-builder.
mod canopy_dom_root {
    use canopy_protocol::NodeId;
    /// The implicit host root every top-level node is mounted under.
    pub const ROOT: NodeId = NodeId::new(0);
}

// Stand-in element kinds. The real ids come from the host widget registry; any
// non-zero tag is fine for this demo because the host's M1 `Dom` does not interpret
// them beyond "this is an element".
const COLUMN: ElementTag = ElementTag::new(1);

extern "C" {
    /// The ONE capability the host grants this guest: hand `len` bytes starting at
    /// `ptr` (a `canopy-protocol` op batch in this module's linear memory) to the
    /// host, which validates every handle and applies the mutations to its retained
    /// tree. The host bounds `len`, performs a checked read of guest memory, and
    /// swallows any decode/handle error — a malformed batch cannot crash the host.
    fn canopy_apply(ptr: *const u8, len: usize);
}

/// Build the plugin's UI and hand it to the host.
///
/// The wasmtime host calls this exported entry point with fuel and an epoch deadline
/// active. The whole body is wrapped so a panic aborts (the crate is built with
/// `panic = "abort"`) rather than unwinding across the FFI boundary.
#[no_mangle]
pub extern "C" fn run() {
    // A nice-looking little column: a heading and a subtitle.
    let mut e = Emitter::new();

    let column = e.create_element(COLUMN);
    e.append(ROOT, column);
    e.set_inline_style(column, DIRECTION, "column");
    e.set_inline_style(column, BG, "#1e1e2e");
    e.set_inline_style(column, PADDING, "16");
    e.set_inline_style(column, GAP, "8");

    let heading = e.create_text("hello from a sandboxed plugin");
    e.append(column, heading);
    e.set_inline_style(heading, FG, "#a6e3a1");
    e.set_inline_style(heading, HEIGHT, "20");

    let subtitle = e.create_text("(untrusted wasm built this)");
    e.append(column, subtitle);
    e.set_inline_style(subtitle, FG, "#cdd6f4");
    e.set_inline_style(subtitle, HEIGHT, "16");

    // Snapshot the batch. Bind it to a variable so the buffer stays alive for the
    // whole duration of the host call below.
    let bytes = e.take_batch(0);

    // SAFETY: `bytes` is a live `Vec<u8>` owned by this frame; `ptr`/`len` describe
    // exactly its initialized contents and remain valid until after `canopy_apply`
    // returns (the host copies the bytes out before returning). The host treats the
    // pointer/length as untrusted and bounds-checks the read on its side.
    unsafe {
        canopy_apply(bytes.as_ptr(), bytes.len());
    }

    // Keep `bytes` alive past the call (and make the intent explicit for readers).
    drop(bytes);
}
