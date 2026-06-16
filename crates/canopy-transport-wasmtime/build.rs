//! Build the untrusted wasm guest (`examples/full/plugin-counter`) so the tests
//! can load a real sandboxed module.
//!
//! The guest targets `wasm32-unknown-unknown` and is excluded from the workspace, so
//! we invoke a nested `cargo build` for it here. Two things keep that nested build
//! from fighting the outer one:
//!   * a SEPARATE `CARGO_TARGET_DIR` under `OUT_DIR`, so the guest's target dir never
//!     contends for the outer build's lock, and
//!   * `--target wasm32-unknown-unknown`, which the guest's own profile compiles with
//!     `panic = "abort"`.
//!
//! The produced `.wasm` path is exported to the test via
//! `cargo:rustc-env=CANOPY_PLUGIN_WASM`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env("OUT_DIR"));

    // The guest crate lives under the repo's `examples/` (full-tier: sandboxed plugins).
    let guest_dir = manifest_dir
        .join("..")
        .join("..")
        .join("examples")
        .join("full")
        .join("plugin-counter");
    let guest_manifest = guest_dir.join("Cargo.toml");

    // Rebuild the wasm whenever the guest's sources or manifest change.
    println!("cargo:rerun-if-changed={}", guest_manifest.display());
    println!(
        "cargo:rerun-if-changed={}",
        guest_dir.join("src").join("lib.rs").display()
    );

    // A dedicated target dir under OUT_DIR avoids any lock contention with the outer
    // build's target directory.
    let guest_target_dir = out_dir.join("guest-target");

    let target = "wasm32-unknown-unknown";

    // Use the same cargo that is driving this build.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    let status = Command::new(&cargo)
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
        // And give it its own target dir explicitly (belt and suspenders alongside
        // --target-dir, in case an outer CARGO_TARGET_DIR is set in the environment).
        .env("CARGO_TARGET_DIR", &guest_target_dir)
        .status();

    let status = match status {
        Ok(s) => s,
        Err(e) => {
            panic!(
                "failed to spawn `cargo build` for the wasm guest at {}: {e}\n\
                 is the `wasm32-unknown-unknown` target installed? run:\n    \
                 rustup target add wasm32-unknown-unknown",
                guest_manifest.display()
            );
        }
    };

    if !status.success() {
        panic!(
            "building the wasm guest ({}) failed.\n\
             the most common cause is a missing target; install it with:\n    \
             rustup target add wasm32-unknown-unknown",
            guest_manifest.display()
        );
    }

    // cdylib output name: dashes in the crate name become underscores.
    let wasm = guest_target_dir
        .join(target)
        .join("release")
        .join("canopy_plugin_counter.wasm");

    if !wasm.exists() {
        panic!(
            "wasm guest build reported success but {} is missing",
            wasm.display()
        );
    }

    // Hand the absolute path to the test via an env var baked in at compile time.
    println!("cargo:rustc-env=CANOPY_PLUGIN_WASM={}", path_str(&wasm));
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("{key} not set by cargo"))
}

fn path_str(p: &Path) -> String {
    p.to_str()
        .unwrap_or_else(|| panic!("non-UTF-8 path: {}", p.display()))
        .to_string()
}
