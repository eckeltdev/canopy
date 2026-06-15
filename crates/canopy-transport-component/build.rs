//! Build the untrusted guest **component** (`examples/canopy-component-guest`) so the
//! integration test can instantiate a real, end-to-end Component Model guest.
//!
//! Producing a component is a two-step pipeline:
//!   1. `cargo build --target wasm32-unknown-unknown` the guest into a *core* wasm
//!      module (wit-bindgen emits the canonical-ABI glue), then
//!   2. `wasm-tools component new` wraps that core module into a **component** that
//!      imports `canopy:ui/host` and exports `run`. No WASI adapter is needed because
//!      the world grants no WASI — the guest imports nothing else.
//!
//! Unlike the core-wasm transport's build.rs, this needs the external `wasm-tools`
//! CLI for step 2. **The build degrades gracefully**: if the `wasm32-unknown-unknown`
//! target or `wasm-tools` is unavailable, it prints a `cargo:warning` and skips,
//! leaving `CANOPY_COMPONENT_WASM` unset. The integration test then build-and-skips
//! with a clear message instead of failing. The crate's own unit tests (host
//! validation logic, no component needed) are unaffected.
//!
//! On success the absolute path of the produced component is exported to the test via
//! `cargo:rustc-env=CANOPY_COMPONENT_WASM`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env("OUT_DIR"));

    // The guest crate lives under the repo's `examples/`.
    let guest_dir = manifest_dir
        .join("..")
        .join("..")
        .join("examples")
        .join("canopy-component-guest");
    let guest_manifest = guest_dir.join("Cargo.toml");

    // Rebuild whenever the guest sources, its manifest, or the shared WIT change.
    println!("cargo:rerun-if-changed={}", guest_manifest.display());
    println!(
        "cargo:rerun-if-changed={}",
        guest_dir.join("src").join("lib.rs").display()
    );
    let wit_dir = manifest_dir.join("..").join("..").join("wit");
    println!(
        "cargo:rerun-if-changed={}",
        wit_dir.join("canopy.wit").display()
    );

    let target = "wasm32-unknown-unknown";
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    // A dedicated target dir under OUT_DIR avoids lock contention with the outer build.
    let guest_target_dir = out_dir.join("guest-target");

    // --- Step 1: build the guest into a CORE wasm module. ---
    let build = Command::new(&cargo)
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg(target)
        .arg("--manifest-path")
        .arg(&guest_manifest)
        .arg("--target-dir")
        .arg(&guest_target_dir)
        // Don't let the guest build inherit the outer build's RUSTFLAGS / target.
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env("CARGO_TARGET_DIR", &guest_target_dir)
        .status();

    match build {
        Ok(s) if s.success() => {}
        Ok(_) => {
            return skip(
                "building the guest core module failed (is the `wasm32-unknown-unknown` \
                 target installed? `rustup target add wasm32-unknown-unknown`)",
            );
        }
        Err(e) => {
            return skip(&format!("could not spawn `cargo build` for the guest: {e}"));
        }
    }

    // cdylib output name: dashes in the crate name become underscores.
    let core_wasm = guest_target_dir
        .join(target)
        .join("release")
        .join("canopy_component_guest.wasm");
    if !core_wasm.exists() {
        return skip(&format!(
            "guest build reported success but {} is missing",
            core_wasm.display()
        ));
    }

    // --- Step 2: turn the core module into a COMPONENT with `wasm-tools`. ---
    // No WASI adapter: the world imports only `canopy:ui/host`, so the core module has
    // no `wasi_snapshot_preview1` imports to satisfy.
    let component_wasm = out_dir.join("canopy_component_guest.component.wasm");
    let wasm_tools = std::env::var("WASM_TOOLS").unwrap_or_else(|_| "wasm-tools".to_string());

    let new = Command::new(&wasm_tools)
        .arg("component")
        .arg("new")
        .arg(&core_wasm)
        .arg("-o")
        .arg(&component_wasm)
        .status();

    match new {
        Ok(s) if s.success() => {}
        Ok(_) => {
            return skip("`wasm-tools component new` failed to produce the component");
        }
        Err(_) => {
            return skip(
                "`wasm-tools` not found on PATH; cannot package the component \
                 (install with `cargo install --locked wasm-tools`)",
            );
        }
    }

    if !component_wasm.exists() {
        return skip("`wasm-tools component new` reported success but no output file exists");
    }

    // Hand the absolute path to the test, baked in at compile time.
    println!(
        "cargo:rustc-env=CANOPY_COMPONENT_WASM={}",
        path_str(&component_wasm)
    );

    // --- Also assemble the adversarial WASI-importing component (best effort). ---
    // It lets the integration test prove the *negative* direction: a component that
    // imports a capability the host never links fails to instantiate. Assembled from a
    // checked-in `.wat` with `wasm-tools parse`. If this optional step can't run, the
    // test simply skips that one assertion (it is gated on CANOPY_ADVERSARY_WASM).
    let adversary_wat = manifest_dir
        .join("tests")
        .join("fixtures")
        .join("wasi-adversary.wat");
    println!("cargo:rerun-if-changed={}", adversary_wat.display());
    let adversary_wasm = out_dir.join("wasi_adversary.component.wasm");
    let parsed = Command::new(&wasm_tools)
        .arg("parse")
        .arg(&adversary_wat)
        .arg("-o")
        .arg(&adversary_wasm)
        .status();
    if matches!(parsed, Ok(s) if s.success()) && adversary_wasm.exists() {
        println!(
            "cargo:rustc-env=CANOPY_ADVERSARY_WASM={}",
            path_str(&adversary_wasm)
        );
    } else {
        println!(
            "cargo:warning=canopy-transport-component: could not assemble the WASI-adversary \
             component; the no-extra-authority instantiation check will be skipped."
        );
    }
}

/// Print a `cargo:warning` explaining why the component could not be built and return
/// without setting `CANOPY_COMPONENT_WASM`. The integration test then build-and-skips.
fn skip(reason: &str) {
    println!(
        "cargo:warning=canopy-transport-component: skipping guest-component build: {reason}. \
         The crate's own tests still run; the end-to-end component test will build-and-skip."
    );
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("{key} not set by cargo"))
}

fn path_str(p: &Path) -> String {
    p.to_str()
        .unwrap_or_else(|| panic!("non-UTF-8 path: {}", p.display()))
        .to_string()
}
