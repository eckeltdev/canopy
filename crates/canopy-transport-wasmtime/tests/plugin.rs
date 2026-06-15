//! Run-verified tests for the untrusted-plugin transport.
//!
//! These load the **real** wasm guest (`examples/canopy-plugin-counter`, built to
//! `wasm32-unknown-unknown` by this crate's `build.rs`) and run it through the
//! sandbox, plus a couple of hand-assembled adversarial modules that prove the
//! capability/safety properties hold.

use canopy_dom::ROOT;
use canopy_transport_wasmtime::{PluginError, PluginHost, MAX_BATCH_BYTES, MAX_MEMORY_BYTES};

/// The real guest module, compiled by `build.rs` and its path baked in here.
const GUEST_WASM_PATH: &str = env!("CANOPY_PLUGIN_WASM");

fn guest_wasm() -> Vec<u8> {
    std::fs::read(GUEST_WASM_PATH)
        .unwrap_or_else(|e| panic!("reading guest wasm at {GUEST_WASM_PATH}: {e}"))
}

/// The untrusted guest builds its tree through the one granted import, and the host's
/// retained tree reflects exactly what it emitted.
#[test]
fn plugin_builds_the_tree() {
    let mut host = PluginHost::new().expect("host");
    host.run(&guest_wasm()).expect("guest run should succeed");

    let dom = host.dom();

    // The guest mounts a column with two text children: 1 element + 2 text = 3 nodes.
    assert_eq!(
        dom.node_count(),
        3,
        "column + heading + subtitle = three nodes"
    );

    // One top-level node (the column) under the implicit ROOT.
    let roots = dom.children(ROOT);
    assert_eq!(roots.len(), 1, "exactly one top-level node");
    let column = roots[0];

    // The column carries the two text children, in order.
    let kids = dom.children(column);
    assert_eq!(kids.len(), 2, "heading + subtitle");

    // Some text node mentions the sandboxed plugin — proving the bytes the *guest*
    // emitted (inside the sandbox) actually landed in the host tree.
    let mentions_plugin = kids
        .iter()
        .filter_map(|&n| dom.text_of(n))
        .any(|t| t.contains("sandboxed plugin"));
    assert!(
        mentions_plugin,
        "a text node should contain \"sandboxed plugin\""
    );
}

/// Capability proof: the guest instantiates with ONLY `env::canopy_apply` linked.
/// `PluginHost` links no WASI (`wasi_snapshot_preview1`), no clock, randomness, or
/// filesystem — so a successful run demonstrates the guest needs nothing else and
/// therefore structurally cannot touch the OS (no syscalls reachable from wasm).
#[test]
fn no_wasi_required() {
    // `PluginHost::new` wires exactly one host import. If the guest depended on WASI
    // or any other host capability, instantiation here would fail with an
    // "unknown import" error instead of running to completion.
    let mut host = PluginHost::new().expect("host");
    let result = host.run(&guest_wasm());
    assert!(
        result.is_ok(),
        "guest instantiates and runs with only canopy_apply linked (no wasi): {result:?}"
    );
    // It still produced a tree, confirming the single import was sufficient.
    assert_eq!(host.dom().node_count(), 3);
}

/// Capability proof, the other direction: a module that imports something we do NOT
/// grant (here a fake `wasi_snapshot_preview1::fd_write`) must fail to instantiate.
/// The host satisfies no such import, so the OS stays unreachable.
#[test]
fn module_importing_wasi_is_rejected() {
    // A minimal module that imports a WASI function. We never link WASI, so this must
    // not instantiate.
    let wat = r#"
        (module
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (func (export "run")))
    "#;
    let wasm = wat::parse_str(wat).expect("assemble wat");

    let mut host = PluginHost::new().expect("host");
    let err = host.run(&wasm).expect_err("a WASI import must be rejected");
    assert!(
        matches!(err, PluginError::Instantiate(_)),
        "unknown WASI import should fail instantiation, got {err:?}"
    );
}

/// Safety proof #1: a guest that forges a node handle it never created is rejected by
/// the host's handle validation — a handled [`PluginError::Apply`], never a host
/// panic. The bytes below are a valid `canopy-protocol` batch that does
/// `SetText(node=999, ...)` without ever creating node 999.
#[test]
fn forged_handle_is_a_handled_error_not_a_panic() {
    // Assemble a guest in WAT that writes a forged op batch into its memory and calls
    // canopy_apply on it. We compute the exact bytes here and embed them as a data
    // segment so the wasm just points the host at them.
    let forged = forged_set_text_batch();
    let wasm = data_blob_guest(&forged);

    let mut host = PluginHost::new().expect("host");
    let err = host
        .run(&wasm)
        .expect_err("forged handle must surface as an error");
    match err {
        PluginError::Apply(canopy_traits::HostError::BadHandle) => {}
        other => panic!("expected Apply(BadHandle), got {other:?}"),
    }
    // The host survived: the tree simply stayed empty.
    assert_eq!(host.dom().node_count(), 0);
}

/// Safety proof #2: garbage bytes (an unknown opcode) decode-fail and surface as a
/// handled [`PluginError::Apply`] with [`canopy_traits::HostError::Decode`].
#[test]
fn corrupt_bytes_are_a_handled_decode_error() {
    // 0xFF is not a valid op tag, so the reader fails to decode.
    let garbage = vec![0xFFu8, 0x00, 0x01, 0x02, 0x03];
    let wasm = data_blob_guest(&garbage);

    let mut host = PluginHost::new().expect("host");
    let err = host
        .run(&wasm)
        .expect_err("corrupt bytes must surface as an error");
    assert!(
        matches!(err, PluginError::Apply(canopy_traits::HostError::Decode)),
        "expected Apply(Decode), got {err:?}"
    );
}

/// Safety proof #3: an out-of-bounds pointer/length passed to `canopy_apply` is a
/// handled error, not a host out-of-bounds read or panic. This guest exports a 1-page
/// (64 KiB) memory and asks the host to read 1 KiB starting at an offset past the end.
#[test]
fn out_of_bounds_read_is_handled() {
    let wat = r#"
        (module
          (import "env" "canopy_apply" (func $apply (param i32 i32)))
          (memory (export "memory") 1)            ;; one 64 KiB page
          (func (export "run")
            ;; ptr = 0x7fff_0000 (way past the 64 KiB page), len = 1024
            (call $apply (i32.const 0x7fff0000) (i32.const 1024))))
    "#;
    let wasm = wat::parse_str(wat).expect("assemble wat");

    let mut host = PluginHost::new().expect("host");
    let err = host
        .run(&wasm)
        .expect_err("an OOB read must surface as an error");
    // The read fails the bounds check and is recorded as a Decode-class error.
    assert!(
        matches!(err, PluginError::Apply(canopy_traits::HostError::Decode)),
        "expected Apply(Decode) for OOB read, got {err:?}"
    );
}

/// Safety proof #4: an oversized length is rejected without the host ever attempting
/// the (huge) allocation. `len` is bounded by [`MAX_BATCH_BYTES`] before any read.
#[test]
fn oversized_length_is_rejected_before_allocation() {
    // Comfortably over the 1 MiB cap (and a valid positive i32). `ptr` is irrelevant:
    // the length check fires before any memory is touched. Compile-time check that we
    // really are over the cap, without a runtime always-true assert.
    const OVERSIZED: usize = 2 * 1024 * 1024;
    const _: () = assert!(OVERSIZED > MAX_BATCH_BYTES);

    let wat = format!(
        r#"
        (module
          (import "env" "canopy_apply" (func $apply (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "run")
            (call $apply (i32.const 0) (i32.const {OVERSIZED}))))
    "#
    );
    let wasm = wat::parse_str(&wat).expect("assemble wat");

    let mut host = PluginHost::new().expect("host");
    let err = host
        .run(&wasm)
        .expect_err("oversized length must be rejected");
    assert!(
        matches!(err, PluginError::Apply(canopy_traits::HostError::Decode)),
        "expected Apply(Decode) for oversized length, got {err:?}"
    );
}

/// Resource-cap proof: a guest with an unbounded loop is interrupted by fuel
/// exhaustion / the epoch deadline — it traps rather than hanging the host.
#[test]
fn runaway_guest_is_interrupted_not_hung() {
    // An infinite loop with no host calls. Both fuel metering and the epoch deadline
    // are configured; whichever fires first traps the guest.
    let wat = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run")
            (loop $forever
              br $forever)))
    "#;
    let wasm = wat::parse_str(wat).expect("assemble wat");

    let mut host = PluginHost::new().expect("host");
    let err = host
        .run(&wasm)
        .expect_err("an infinite loop must be interrupted");
    assert!(
        matches!(err, PluginError::Trap(_)),
        "a runaway guest should trap (fuel/epoch), got {err:?}"
    );
}

/// The configured caps are sane and documented as part of the public surface.
#[test]
fn caps_are_configured() {
    assert_eq!(MAX_BATCH_BYTES, 1 << 20, "1 MiB batch cap");
    assert_eq!(MAX_MEMORY_BYTES, 16 << 20, "16 MiB memory cap");
}

// ---------------------------------------------------------------------------
// Helpers: build adversarial wasm guests that hand the host a fixed byte blob.
// ---------------------------------------------------------------------------

/// A `canopy-protocol` batch that does `SetText` on a node id (999) that was never
/// created — a forged handle. Built with the guest-side `canopy-core` Emitter (a
/// dev-dependency), then we burn handles so the id is stable and high.
fn forged_set_text_batch() -> Vec<u8> {
    use canopy_core::Emitter;
    let mut e = Emitter::new();
    // Burn handles so the forged node doesn't accidentally exist.
    for _ in 0..998 {
        e.alloc_node();
    }
    let ghost = e.alloc_node(); // node id 999
    e.set_text(ghost, "haxx");
    e.take_batch(0)
}

/// Assemble a tiny wasm guest whose `run` calls `canopy_apply(ptr, len)` pointing at
/// `blob`, which is embedded as a data segment at offset 0 of a fresh memory page.
fn data_blob_guest(blob: &[u8]) -> Vec<u8> {
    let len = blob.len();
    // Escape the blob bytes as a WAT data string.
    let mut data = String::new();
    for &b in blob {
        data.push_str(&format!("\\{b:02x}"));
    }
    let wat = format!(
        r#"
        (module
          (import "env" "canopy_apply" (func $apply (param i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "{data}")
          (func (export "run")
            (call $apply (i32.const 0) (i32.const {len}))))
    "#
    );
    wat::parse_str(&wat).expect("assemble data-blob guest")
}
