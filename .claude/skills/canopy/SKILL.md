---
name: canopy
description: >-
  Build and ship a UI with Canopy — the JavaScript-runtime-free, web-like native UI
  runtime in this repo. Use when authoring a Canopy app (Rust rsx!/Ui or the C++ DSL),
  styling it with the lite CSS engine, wiring it to a window or a real device framebuffer,
  or deploying to hardware (desktop, bare-metal aarch64 on the frt runtime, or Cortex-M
  no_std). Triggers on: Canopy, rsx!, canopy-ui, canopy_cpp, canopy-abi, op-stream,
  DisplayList, render_rgba, frt runtime, "style with CSS", "run on hardware/framebuffer".
---

# Canopy

Canopy is a **typed, capability-based, web-like native UI runtime with no JavaScript
runtime**. You author a UI tree + CSS-like styles; the app reaches the UI only through a
validated **op-stream** of opaque node handles. The core is `no_std` + `alloc`, so the
same UI code runs on the desktop (GPU) and all the way down to bare metal (software
rasterizer). Repo root: `the repository root`.

## The one mental model

```
author a tree + CSS  ──▶  op-stream bytes  ──▶  host applies + cascades + lays out + rasterizes  ──▶  RGBA8 framebuffer
   (rsx! or C++ DSL)      (canopy-protocol)      (canopy-abi / the no_std crates)                    (blit to a window or a device)
```

Everything reduces to: **emit a batch of ops, hand it to a host, get back an `width*height*4`
RGBA8 buffer, blit it.** Pointer/hover/click events flow back the other way. There is no
virtual DOM and no second runtime — `rsx!` and a hand-written tree emit byte-identical ops.

## Pick your path (this decides everything downstream)

| You are… | Author with | Host / runtime | Target | Start from |
|---|---|---|---|---|
| A Rust app on desktop | `rsx!` + `canopy-ui` `Ui` | `canopy new` (winit + softbuffer/wgpu) | macOS/Linux/Windows | `examples/lite/welcome` |
| A Rust app on a microcontroller | `canopy-ui` `Ui` (it's `no_std`) or `canopy-core` `Emitter` | the `no_std` crates: `canopy-dom` → `canopy-paint` → `canopy-render-soft` | Cortex-M (`thumbv7em-none-eabi`), any `no_std` target | `examples/lite/embedded` |
| A C++ app on bare metal | the `canopy_cpp` DSL | link `libcanopy_abi.a` + the **frt** freestanding runtime | bare-metal aarch64 (Orange Pi 5), freestanding | `bindings/canopy_cpp/examples/gui_css` |
| Any host in another language | hand-roll the op-stream | the C ABI (`canopy.h`) | anything that can link a staticlib | `crates/canopy-abi/include/canopy.h` |

Read the matching reference file before writing code:
- **`reference/architecture.md`** — the op-stream, the capability boundary, the two style tiers, the render path, the crate map.
- **`reference/authoring.md`** — the Rust (`rsx!`/`Ui`) and C++ (DSL) authoring APIs, side by side, with every factory + method.
- **`reference/styling.md`** — the lite CSS engine (selectors, properties, values). Full coverage table lives in `crates/canopy-style-css/FEATURES.md`.
- **`reference/hardware.md`** — deploying to a real framebuffer: the three target paths, exact build commands, the frt platform seam, the `no_std` build-proof.
- **`reference/api.md`** — the C ABI (`canopy.h`), the C++ `host` wrapper, the Rust `CanopyHost`/`Ui`, and the op-protocol quick reference.

## The universal render → display step (memorize this)

Every host produces the same thing — a row-major, straight-alpha RGBA8 buffer — and you
blit it to whatever surface your platform has (a window, a Linux `/dev/fb0`, an SPI TFT, a
bare-metal MMIO framebuffer):

- **C++:** `std::vector<uint8_t> rgba = host.render_rgba(w, h);` → copy to your framebuffer.
- **C ABI:** `canopy_host_render_rgba(host, w, h, out, cap, &out_len);` (call once with `cap=0`/`out=NULL` to get the needed `out_len = w*h*4`, then again with the buffer).
- **Rust no_std:** lay out with `canopy-paint` + rasterize with `canopy-render-soft` into your `Buffer`, then blit (see `examples/lite/embedded/src/lib.rs`).

There is no GPU dependency on this path — it is pure CPU, `no_std`-safe.

## Canonical minimal app (Rust desktop)

```rust
use canopy_ui::prelude::*;

let ui = Ui::with_css(".card{background:#313244;radius:12;padding:16}\
                       button{background:#89b4fa;color:#11111b;radius:8;padding:10}");
let count = ui.signal(0i32);
let root = rsx!(ui =>
    <div class="card">
        <button on:click={ let c = count.clone(); move |_| c.update(|n| *n += 1) }>
            { let c = count.clone(); move || format!("count is {}", c.get()) }
        </button>
    </div>
);
ui.mount_root(root);
// `canopy new` wires the window loop; or drive the bytes yourself:
//   host.apply_bytes(&ui.take_batch(0)); let px = host.render_rgba(w, h);
```

## Canonical minimal app (C++ DSL, links libcanopy_abi.a)

```cpp
#include "canopy_cpp/dsl.hpp"
#include "canopy_cpp/host.hpp"
using namespace canopy;

build_context ctx;
mount(ctx, div(id("card"),
               button(cls("primary"), on_click([]{ /* ... */ }), "Open")));

host engine;
engine.set_stylesheet("#card{background:#313244;radius:12;padding:16}"
                      "button.primary{background:#89b4fa;color:#11111b;radius:8;padding:10}");
engine.apply(ctx.take_batch(0));
engine.resize(view_w, view_h);
std::vector<std::uint8_t> rgba = engine.render_rgba(view_w, view_h); // -> blit
```

## Build + verify (always green; this repo pins a STABLE toolchain)

- **Engine staticlib (for C/C++):** `cargo build -p canopy-abi` → `target/debug/libcanopy_abi.a`.
- **C++ binding + examples:** CMake under `bindings/canopy_cpp/` (link `-lcanopy_abi`). Run `cpp-doctor check` on C++ (see the `cpp-doctor-style-guide` skill; types are `snake_case`).
- **no_std bare-metal proof:** `cargo +nightly build -p canopy-style-css -p canopy-layout-taffy -p canopy-render-soft --target thumbv7em-none-eabi` (nightly only for the cross-target; the main workspace is stable).
- **Test a specific crate:** `cargo test -p canopy-style-css` etc. **Never** a bare-workspace `cargo test`/`cargo build` (the GPU crates need a newer rustc than the pin) and **never** `cargo update`.
- **Protocol parity:** if you touch a PropId/op/widget id, mirror it in `crates/canopy-abi/include/canopy_protocol.h` + `bindings/canopy_cpp/include/canopy_cpp/protocol.hpp` and run `cargo test -p canopy-abi --test protocol_header`.

## Rules of the road

- **Identity, not inline styles.** Author the tree with `class`/`id`/tag only; put all styling in the stylesheet string. The host runs the real cascade (selectors, specificity, inheritance, `var()`, `@media`) and folds results in non-destructively — the retained tree stays parity-stable.
- **Two style tiers, one API.** LITE (`canopy-style-css`, `no_std`, the device path) vs CAPABLE (`canopy-style-stylo` = real Servo Stylo, std/desktop). Same authoring; the host picks the tier. On hardware you are on LITE.
- **Handles are unforgeable.** A `NodeId` is host-minted; you can only touch nodes you were handed. The op boundary *is* the permission boundary.
- **Reactivity is a signal, not a diff.** A changed `Signal` re-emits one targeted `SetText`/`SetInlineStyle`, never a tree diff.
