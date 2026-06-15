//! Compile the untrusted wasm plugin (`examples/canopy-plugin-counter`) so the demo
//! can load and host it in a panel. Mirrors the transport crate's build step: a
//! nested `cargo build --target wasm32-unknown-unknown` with its own target dir
//! under `OUT_DIR` (no lock contention), exposing the `.wasm` path to the demo via
//! `cargo:rustc-env=CANOPY_PLUGIN_WASM`.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let guest_dir = manifest_dir.join("..").join("canopy-plugin-counter");
    let guest_manifest = guest_dir.join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", guest_manifest.display());
    println!(
        "cargo:rerun-if-changed={}",
        guest_dir.join("src").join("lib.rs").display()
    );

    let guest_target = out_dir.join("guest-target");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    let status = Command::new(&cargo)
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .arg("--manifest-path")
        .arg(&guest_manifest)
        .arg("--target-dir")
        .arg(&guest_target)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET")
        .env("CARGO_TARGET_DIR", &guest_target)
        .status()
        .expect("spawn cargo for the wasm guest (need: rustup target add wasm32-unknown-unknown)");
    assert!(
        status.success(),
        "building the wasm guest failed (try: rustup target add wasm32-unknown-unknown)"
    );

    let wasm = guest_target
        .join("wasm32-unknown-unknown")
        .join("release")
        .join("canopy_plugin_counter.wasm");
    assert!(wasm.exists(), "wasm guest missing at {}", wasm.display());
    println!("cargo:rustc-env=CANOPY_PLUGIN_WASM={}", wasm.display());
}
