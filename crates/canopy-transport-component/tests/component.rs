//! End-to-end tests for the untrusted **component** transport.
//!
//! These load the **real** guest component (`examples/canopy-component-guest`, built
//! by this crate's `build.rs` via wit-bindgen + `wasm-tools component new`) and run it
//! through the host, proving the cross-language Component Model path works end to end:
//! a component built from `wit/canopy.wit` drives the host `Dom` through the single
//! granted `host.apply` import and nothing else.
//!
//! # Build-and-skip
//!
//! Producing a component needs the `wasm32-unknown-unknown` target *and* the
//! `wasm-tools` CLI. If `build.rs` could not run that pipeline it leaves
//! `CANOPY_COMPONENT_WASM` unset; [`option_env!`] makes that a compile-time `None`, so
//! these tests **skip with a clear message** rather than failing. The crate's own unit
//! tests (host validation logic, in `src/lib.rs`) always run regardless.

use canopy_dom::ROOT;
use canopy_transport_component::{ComponentError, ComponentHost};

/// The real guest component, if `build.rs` managed to build it. `None` => skip.
const GUEST_COMPONENT: Option<&str> = option_env!("CANOPY_COMPONENT_WASM");

/// The adversarial WASI-importing component, if `wasm-tools` assembled it. `None` =>
/// skip just the negative-direction assertion.
const ADVERSARY_COMPONENT: Option<&str> = option_env!("CANOPY_ADVERSARY_WASM");

/// Read a built artifact off disk, failing loudly if the path was baked but the file
/// vanished (a real error, distinct from "the toolchain was unavailable").
fn read_artifact(path: &str, what: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("reading {what} at {path}: {e}"))
}

/// End-to-end: the host instantiates the real component, calls its exported `run`, and
/// the component drives the host `Dom` through `host.apply` to exactly the tree it
/// emitted — a column with three text children.
#[test]
fn component_run_populates_the_dom() {
    let Some(path) = GUEST_COMPONENT else {
        eprintln!(
            "SKIP component_run_populates_the_dom: guest component not built \
             (need wasm32-unknown-unknown + wasm-tools; see crate README)."
        );
        return;
    };
    let bytes = read_artifact(path, "guest component");

    let mut host = ComponentHost::new().expect("host builds");
    host.run(&bytes).expect("component run should succeed");

    let dom = host.dom();

    // column + heading + line + subtitle = four nodes.
    assert_eq!(
        dom.node_count(),
        4,
        "column + heading + line + subtitle = four nodes"
    );

    // Exactly one top-level node (the column) under the implicit ROOT.
    let roots = dom.children(ROOT);
    assert_eq!(roots.len(), 1, "exactly one top-level node");
    let column = roots[0];

    // It carries the three text children, in order.
    let kids = dom.children(column);
    assert_eq!(kids.len(), 3, "heading + line + subtitle");

    // A text node mentions the component guest — proving the bytes the *component*
    // emitted (inside the sandbox, through `host.apply`) actually landed in the host
    // tree. This is the cross-language thesis end to end.
    let mentions_component = kids
        .iter()
        .filter_map(|&n| dom.text_of(n))
        .any(|t| t.contains("component guest"));
    assert!(
        mentions_component,
        "a text node should contain \"component guest\""
    );
}

/// Capability proof (positive direction): the guest instantiates and runs with ONLY
/// the `canopy:ui/host` interface linked. The host adds no WASI, clock, or filesystem,
/// so a successful run demonstrates the component needs nothing else — it structurally
/// cannot reach the OS. Symmetric to the core-wasm transport's `no_wasi_required`.
#[test]
fn host_links_only_the_one_capability() {
    let Some(path) = GUEST_COMPONENT else {
        eprintln!("SKIP host_links_only_the_one_capability: guest component not built.");
        return;
    };
    let bytes = read_artifact(path, "guest component");

    // `ComponentHost::new` adds exactly the `host` interface to the linker. If the
    // guest needed any other import, instantiation here would fail.
    let mut host = ComponentHost::new().expect("host builds");
    let result = host.run(&bytes);
    assert!(
        result.is_ok(),
        "component instantiates and runs with only `host` linked (no wasi): {result:?}"
    );
    assert_eq!(host.dom().node_count(), 4, "it still produced its tree");
}

/// Capability proof (negative direction): a component that imports something the host
/// does NOT grant — here `wasi:cli/environment` — fails to instantiate. The host
/// satisfies no such import, so the OS stays unreachable. This is the runtime
/// enforcement of the world's no-ambient-authority shape.
#[test]
fn component_importing_wasi_is_rejected() {
    let Some(path) = ADVERSARY_COMPONENT else {
        eprintln!(
            "SKIP component_importing_wasi_is_rejected: adversary component not assembled \
             (need wasm-tools)."
        );
        return;
    };
    let bytes = read_artifact(path, "adversary component");

    let mut host = ComponentHost::new().expect("host builds");
    let err = host
        .run(&bytes)
        .expect_err("a component importing un-granted WASI must be rejected");
    assert!(
        matches!(err, ComponentError::Instantiate(_)),
        "an unsatisfied import should fail instantiation, got {err:?}"
    );
    // The host survived and its tree stayed empty — no partial application.
    assert_eq!(host.dom().node_count(), 0);
}
