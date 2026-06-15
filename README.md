# Canopy

A web-like native UI runtime with **no JavaScript runtime**. You author UI with a
familiar web mental model — a declarative tree, CSS-like styling, events,
components — but app logic is written in native languages (or compiled to WASM),
and the DOM is reached only through a typed, **capability-based** op-stream
protocol where UI nodes are opaque, unforgeable handles.

> Status: **M0 scaffold** — this is the seam, not the renderer. The four crates
> below compile, are tested, and are guaranteed `std`-free so every future target
> (desktop now, a Raspberry Pi appliance later) is a backend swap, not a rewrite.
> Rendering, styling, the `rsx!` macro, and the transports land on top of this.

## The shape

```
Per-language authoring (Rust rsx!, …)         ← thin syntax
        │
canopy-signals  (fine-grained reactivity)     ← shared engine
        │   diff → batched op-stream
canopy-core     (vnode tree + reconciler + encoder)
        │   canopy-protocol bytes
   ┌────┴─────┐
 native      WASM            ← two transports, one op-stream
   └────┬─────┘
canopy-core host side → backends behind canopy-traits:
   Renderer · StyleEngine · TextEngine · LayoutEngine · Platform
        │
      pixels
```

## Crates (this scaffold)

| Crate | `no_std` | What it is |
|---|---|---|
| `canopy-protocol` | yes | Opaque handles, opcodes, and the batched op-stream codec. Zero deps. The contract. |
| `canopy-traits` | yes | The platform-abstraction layer: backend traits + Canopy-owned types that cross them. |
| `canopy-core` | yes | Guest-side vnode tree, string interning, and the reconciler that emits the op-stream. |
| `canopy-signals` | yes | The fine-grained reactive runtime (signals + effects + batched flush). |

Host-side backends (`canopy-render-vello`, `canopy-style-stylo`, `canopy-text-parley`,
`canopy-plat-winit`), the transports (`canopy-transport-native`, `-wasmtime`), the
`canopy-rsx` macro, and the `canopy` CLI are the next crates — each a leaf that may
use `std`.

## The one rule that keeps the future open

The std seam is a **crate boundary, not a `#[cfg]`**. The four crates above never
`use std`, never name a vendor type (Stylo/Taffy/Parley/Vello structs) in a trait,
and never pull `getrandom` (they use `alloc::collections::BTreeMap`, not
`std::HashMap`). CI enforces all three:

- `cargo test --workspace` — unit tests, including op-stream round-trips and the
  reconciler.
- `cargo build --target aarch64-unknown-none -p canopy-protocol -p canopy-traits
  -p canopy-core -p canopy-signals` — the **no_std seam guard**: red the instant the
  core pulls in `std`.
- `cargo deny check` — bans `getrandom` and gates licenses/advisories.

## Build & test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Prove the core is std-free (any installed bare-metal target works):
rustup target add thumbv7em-none-eabi
cargo build --target thumbv7em-none-eabi \
  -p canopy-protocol -p canopy-traits -p canopy-core -p canopy-signals
```

## Roadmap

Milestone **M1** is the `rsx!` counter rendering through the full
Vello/Stylo/Taffy/Parley stack via **both** transports (native + a Wasmtime sandbox
proven against a runaway guest), on macOS/Windows/Linux. Then a Raspberry Pi
appliance via minimal embedded Linux + DRM-KMS (which keeps the GPU). True
bare-metal and microcontrollers are parked research tracks the `no_std` seam keeps
reachable.

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE) at your option.
