# `canopy-transport-component`

The **Component Model** peer of `canopy-transport-wasmtime`. It runs an *untrusted*
Canopy guest that is a real [WebAssembly Component](https://component-model.bytecodealliance.org/),
built from `wit/canopy.wit`, and lets it drive the host UI through exactly one
imported capability — `canopy:ui/host.apply` — and nothing else.

This is the end-to-end proof of Canopy's cross-language thesis: guest and host share a
single interface artifact (`wit/canopy.wit`). The host generates its bindings with
`wasmtime::component::bindgen!`; the guest generates its own with `wit-bindgen` (and
any other Component-Model language — JS via `jco`, Python via `componentize-py`, Go
via TinyGo — could generate theirs from the identical `.wit`).

## How it relates to the core-wasm transport

| | `canopy-transport-wasmtime` (core wasm) | `canopy-transport-component` (this) |
|---|---|---|
| guest artifact | bare `wasm32-unknown-unknown` module | wasm **component** (from `wit/canopy.wit`) |
| granted import | `env::canopy_apply(ptr, len)` | `canopy:ui/host.apply(ops: list<u8>)` |
| reading op bytes | manual bounds-check + `Memory::read` | owned `Vec<u8>` via the canonical ABI |
| host `unsafe` | none (safe `Memory::read`) | **none at all** (no pointer to read) |
| bad ops | recorded `HostError` | typed `result::err(apply-error)` *and* recorded |
| extra imports (WASI, …) | rejected at instantiation | rejected at instantiation |
| applied to | `canopy-dom::Dom` | `canopy-dom::Dom` (same validator) |

Both carry the **same** `canopy-protocol` op bytes and validate them through the same
`canopy-dom::Dom`. Only the boundary differs.

## The guest component build pipeline

The guest lives in `examples/full/component-guest` (excluded from the workspace). It
is a `wit-bindgen` guest compiled to a core module and then packaged into a component.
`build.rs` runs this automatically for the integration test, but to do it by hand:

```sh
# 1. Build the guest into a CORE wasm module (wit-bindgen emits the canonical-ABI glue).
cargo +nightly build --release --target wasm32-unknown-unknown \
  --manifest-path examples/full/component-guest/Cargo.toml

# 2. Package the core module into a COMPONENT. No WASI adapter is needed because the
#    world grants no WASI — the core module imports only `canopy:ui/host`.
wasm-tools component new \
  examples/full/component-guest/target/wasm32-unknown-unknown/release/canopy_component_guest.wasm \
  -o canopy_component_guest.component.wasm

# 3. (optional) Confirm the component imports ONLY `canopy:ui/host` and exports `run`:
wasm-tools component wit canopy_component_guest.component.wasm
```

Step 3 prints:

```wit
world root {
  import canopy:ui/types@0.1.0;
  import canopy:ui/host@0.1.0;
  export run: func();
}
```

— no `wasi:*` import anywhere, which is the no-ambient-authority guarantee made
structural. (Building the guest for `wasm32-wasip2` would also emit a component
directly, skipping step 2.)

### Toolchain

* `rustup +nightly target add wasm32-unknown-unknown`
* `cargo install --locked wasm-tools` (provides `wasm-tools component new`)

If either is missing, `build.rs` prints a `cargo:warning` and skips the guest build;
the integration test (`tests/component.rs`) then **build-and-skips** rather than
failing. The crate's own unit tests (in `src/lib.rs`) cover the host validation logic
without any component and always run.

## Tests

* `src/lib.rs` unit tests — host `apply` validation: a forged handle is a handled
  `bad-handle` (not a panic), an oversized batch is rejected as `too-large` *before*
  decoding, a valid batch populates the `Dom`, and the `HostError -> ApplyError`
  mapping is total. No component required.
* `tests/component.rs` integration tests (build-and-skip) — the host instantiates the
  **real** guest component and its `run` populates the `Dom` (asserts `node_count` and
  a text node's contents); the guest runs with only `host` linked (positive capability
  proof); and a component importing `wasi:cli/environment` fails to instantiate
  (negative capability proof).
