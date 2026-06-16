# Canopy examples

Every example is its own **standalone crate** (its own `[workspace]`), excluded from the
core workspace so the `no_std` core never inherits `winit`/`wgpu`/`wasm` deps. Run each
from its own directory.

They are organized around Canopy's two deployment tiers.

## `lite/` — the constrained tier

CPU software rasterizer, the baked bitmap / `parley` text path, the hand-rolled flex
layout, the minimal CSS-lite styler. This is the "embeddable to bare metal" story: the
whole render pipeline is `no_std` + `alloc`.

| Example | What it shows | Run |
| --- | --- | --- |
| [`welcome`](lite/welcome) | The flagship `rsx!` starter — Canopy's answer to the Vite/React scaffold: a live counter + hot-reloading stylesheet. | `cargo run` (window) · `cargo run --no-default-features --bin render` (PPM) |
| [`landing`](lite/landing) | A dark, animated, one-page product landing, CPU-rasterized. Same UI as `full/landing` — only the backend differs. | `cargo run` · `cargo run --no-default-features --bin render` |
| [`embedded`](lite/embedded) | The headline proof: the **entire** build → layout → rasterize pipeline in a `#![no_std]` crate, cross-compiled for bare-metal Cortex-M. A host bin dumps the frame to a PPM. | `cargo run --bin render` · `cargo +nightly build --lib --target thumbv7em-none-eabi` |

## `full/` — the capable tier

GPU rasterization (`wgpu`/`vello`, Metal/Vulkan), a **real Servo-Stylo CSS cascade**
([`canopy-style-stylo`](../crates/canopy-style-stylo)), and **sandboxed untrusted
plugins** (the `wasmtime` / Component-Model transports). Desktop / SBC class.

| Example | What it shows | Run |
| --- | --- | --- |
| [`landing`](full/landing) | The GPU twin of `lite/landing`: the same shared UI, rasterized on the GPU. | `cargo run` · `cargo run --no-default-features --bin render` |
| [`stylo`](full/stylo) | The full-tier **`StyleEngine`**: a real Stylo cascade (inheritance, specificity, descendant combinators — what the lite class-only engine can't do) styles a tree. The window **hot-reloads `styles.css`** live — edit the CSS, press a key, watch the cascade update. | `cargo run` · `cargo run --no-default-features --bin render` |
| [`demo`](full/demo) | The kitchen sink — a counter + text field + list, with an **untrusted wasm plugin** hosted in a side panel. | `cargo run` · `cargo run --no-default-features --bin render` |
| [`plugin-counter`](full/plugin-counter) | A tiny untrusted guest compiled to `wasm32-unknown-unknown`; loaded by `demo` and the `wasmtime` transport's tests. | built automatically by its hosts |
| [`component-guest`](full/component-guest) | The same idea as a real **WebAssembly Component** (wit-bindgen against `wit/canopy.wit`); loaded by the Component-Model transport's tests. | built automatically by its host |

## `common/` — shared support

| Crate | Used by |
| --- | --- |
| [`landing-ui`](common/landing-ui) | The landing UI tree + animation timeline + `styles.css`, authored once and rendered by **both** `lite/landing` (CPU) and `full/landing` (GPU). Edit its `styles.css` and either window hot-reloads. |

## The lite ↔ full demonstration

`lite/landing` and `full/landing` are the same app — identical UI tree, identical
animation timeline, identical stylesheet (all in `common/landing-ui`) — differing only in
which renderer each binary links. That is the whole pitch of the tiered design: author
once, pick the backend that fits the target.
