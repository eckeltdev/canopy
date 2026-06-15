# `wit/` — Canopy's Component Model contract

`canopy.wit` is the language-neutral description of how an untrusted Canopy guest
talks to a host, expressed in the [WebAssembly Component Model](https://component-model.bytecodealliance.org/)
type system (`package canopy:ui@0.1.0`). It is an **interface artifact**: today it
is a spec, not a build input. No `wit-bindgen` codegen is wired into the workspace —
that is a deliberate follow-up (see below).

## What the world says

The world a guest plugin targets is `canopy-guest`. Its shape captures the whole
capability contract:

- **One imported capability: `host`.** The guest's only way to change the UI is
  `host.apply(ops: list<u8>) -> result<_, apply-error>` — hand the host a batch of
  `canopy-protocol` op bytes to validate and apply. A second import,
  `host.poll-events() -> list<event>`, lets the host deliver input back. That is the
  *entire* host surface.
- **No ambient authority.** Because `canopy-guest` imports nothing but `host` — no
  WASI, no clock, no filesystem, no network — a conforming guest *structurally*
  cannot reach the OS. This is the same threat model `canopy-transport-wasmtime`
  enforces at runtime over core wasm modules, lifted into the component's type.
- **One exported entry point: `run: func()`.** The host calls it once to build and
  start driving the UI, matching the real guest's exported `run` function.
- **Opaque, host-minted handles.** `node-id`, `str-id`, `handler-id`, and
  `event-kind` (in `interface types`) are plain integers. Naming a handle does not
  grant access — the host arena validates ownership on every mutating op, so a guest
  can only ever touch a node it was handed. A forged handle comes back as
  `apply-error::bad-handle`.

### Why op bytes instead of typed ops in WIT?

`apply` takes an opaque `list<u8>`, not a `variant` of every op. The op bytes are the
`canopy-protocol` wire format — the *same* bytes the C ABI (`canopy-abi`) and the
wasmtime transport already carry. Keeping the Component Model boundary oblivious to
their internal structure means the wire format can evolve (new ops, version bumps)
without a WIT change or a regenerated binding. The `list<u8>` carries its own length,
so unlike the raw core-wasm `canopy_apply(ptr, len)` import there is no untrusted size
for the host to bounds-check by hand — the component boundary does it.

## Relationship to the rest of the runtime

| Surface | Crate / file | Boundary |
|---|---|---|
| Compiled-in native guest | `canopy-transport-native` | trust at **compile time** |
| Untrusted core-wasm guest | `canopy-transport-wasmtime` | one import `env::canopy_apply(ptr,len)`, enforced at **runtime** |
| Foreign-language host (C/C++/Python/Swift/…) | `canopy-abi` + `include/canopy.h` | stable **C ABI** |
| **Untrusted component guest** | **`wit/canopy.wit`** (this file) | one import `host.apply`, enforced by the **Component Model type** |

`canopy.wit` is the component-model expression of the same single-capability contract
the other three already implement; it is what lets a guest authored in any
component-model language link against Canopy.

## Validating

The file is kept syntactically valid. If [`wasm-tools`](https://github.com/bytecodealliance/wasm-tools)
is installed:

```sh
wasm-tools component wit wit/canopy.wit
```

This parses the package and prints the resolved, canonical form (exit 0, no
diagnostics) — it is the check this directory is held to. `wasm-tools` is **not** a
workspace build dependency; it is only an optional local validator.

## Future: wiring up `wit-bindgen` (not done here)

When the component-model transport lands, this `.wit` becomes a real codegen input:

- **Guest side** — a Rust guest crate adds `wit-bindgen` and generates trait stubs
  for the `canopy-guest` world:

  ```rust
  wit_bindgen::generate!({ world: "canopy-guest", path: "../../wit" });
  ```

  It then implements `Guest::run` and calls the generated `host::apply(&bytes)`,
  reusing the existing `canopy-core` `Emitter` to build the op batch.

- **Host side** — a `canopy-transport-component` crate uses
  [`wasmtime::component`](https://docs.rs/wasmtime) (`bindgen!`) to generate a typed
  `Linker` from the same world, implementing `apply`/`poll-events` by forwarding to
  `canopy-dom::Dom` exactly as the C ABI and the core-wasm transport do. The guest is
  then loaded as a *component* (e.g. via `wasm-tools component new` over a core
  module) rather than a bare core module.

Other languages (JS via `jco`, Python via `componentize-py`, Go via TinyGo, …) can
generate their own guest bindings from the identical `.wit`, which is exactly the
"each language builds its own React-like wrapper over the core" goal.
