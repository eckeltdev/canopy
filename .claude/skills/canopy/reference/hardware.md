# Running Canopy on real hardware

The universal device contract is a **row-major, straight-alpha RGBA8 framebuffer**
(`width*height*4` bytes). Produce it from the tree, then blit it to whatever surface the
platform exposes — a window, a Linux `/dev/fb0`, an SPI/parallel TFT, or a bare-metal MMIO
framebuffer. No GPU is required on this path; it is pure CPU and `no_std`-safe.

```
your tree + CSS  ──▶  ops  ──▶  host: cascade + Taffy layout + software rasterize  ──▶  RGBA8  ──▶  blit
```

Per-frame loop on any target:
1. apply the latest op batch (only when the UI changed — reactivity emits tiny batches),
2. `render_rgba(w, h)` → RGBA8,
3. blit to the display (convert pixel order if the panel wants RGB565/BGRA),
4. feed input back (pointer/touch → `hover`/`pointer`; then `pump`/`poll_events` → handlers),
5. re-render only if step 4 reported a change.

---

## Path A — Rust on desktop (fastest to see something)

```sh
cargo build -p canopy-abi          # or: canopy new myapp && cd myapp && cargo run
```
`canopy new` scaffolds a winit + softbuffer/wgpu window driving a `Ui`. Use this to
develop and preview the exact same tree + CSS you will ship to a device.

## Path B — Rust on a microcontroller (`no_std`, e.g. Cortex-M)

The lite pipeline is `#![no_std] + alloc` end to end. Build + layout + rasterize with the
no_std crates and blit into your panel's framebuffer. The build-proof + canonical skeleton
is **`examples/lite/embedded`** (a `#![no_std]` lib that does the whole
build→layout→rasterize pipeline; a host `render` bin writes a PPM so you can see it):

```sh
rustup target add thumbv7em-none-eabi
cargo +nightly build --lib --target thumbv7em-none-eabi   # the no_std lib (bare metal)
cargo run --bin render frame.ppm                          # host preview of the same code
```

Wiring (see `examples/lite/embedded/src/lib.rs`): build the op-stream with `canopy-core`'s
`Emitter` (the example's choice; `canopy-ui` works too — it's also no_std); apply to a
`canopy_dom::Dom`; lay out + build the `DisplayList` with `canopy-paint`; rasterize into a
`canopy-render-soft` `Buffer`; copy the buffer to your framebuffer. Bring your own
`#[global_allocator]` (e.g. `embedded-alloc`) and `#[panic_handler]`; the crates need only
`alloc`.

> Nightly is used **only** for the cross-target build (the bare-metal target needs
> `-Z build-std`-style support / a nightly to build the `alloc` seam cleanly). The main
> workspace is pinned STABLE — never `cargo update`, never a bare-workspace build.

## Path C — C++ on bare metal (aarch64, e.g. Orange Pi 5) via the frt runtime

Author with the `canopy_cpp` DSL, link the `canopy-abi` engine staticlib, and run it
freestanding on the **frt** runtime (`runtime/frt/`), which supplies the C++ platform
primitives an OS normally would.

```sh
# 1. Build the engine staticlib for your target (host shown; cross-compile for aarch64 as needed)
cargo build -p canopy-abi          # -> target/debug/libcanopy_abi.a   (link with -lcanopy_abi)
# 2. Build the C++ app + binding + frt, linking the staticlib (see bindings/canopy_cpp/CMakeLists.txt)
```

**The frt platform seam (`runtime/frt/include/frt/platform.hpp`) is your hardware hook.**
frt routes all of `std`/global `new`/`delete` through an installable backend — a
`frt::platform_ops` struct of function pointers (allocate, log, clock, abort). The default
`host_ops()` is POSIX; for your board you install a backend that implements those over your
hardware:

```cpp
#include "frt/platform.hpp"
// Exact field set from runtime/frt/include/frt/platform.hpp:
const frt::platform_ops board_ops = {
    .alloc            = &board_alloc,      // void*(size, align) over the board's heap region
    .free             = &board_free,       // void(ptr, size, align)
    .panic            = &board_halt,       // void(const char* msg) -> never returns
    .log              = &board_uart_write, // void(const char* msg, size_t len)
    .ticks            = &board_ticks,      // uint64_t() monotonic counter
    .ticks_per_second = &board_tick_hz,    // uint64_t()
};
frt::install_platform(board_ops);   // install BEFORE creating the host
```

Then it's the normal flow — `host engine; engine.set_stylesheet(css);
engine.apply(ctx.take_batch(0)); auto rgba = engine.render_rgba(w, h);` — and you DMA/copy
`rgba` to the display controller's framebuffer.

> Why this works freestanding: `canopy-abi` uses a small slice of `std` (vector/map/string),
> and the frt M5 audit (`runtime/frt/docs/m5-nm-audit.md`,
> `runtime/frt/tools/nm_audit.sh`) proves that slice drags **no exception unwinder, no RTTI,
> and no static-initializer machinery** on bare-metal aarch64 — so it links into a
> freestanding image. Run that audit when you change what `std` features the host uses.

## Path D — any other language / OS

Link the staticlib and call the C ABI in `crates/canopy-abi/include/canopy.h` directly
(see `api.md`). The op-stream wire format is `crates/canopy-abi/include/canopy_protocol.h`.
The pixel contract is identical: `canopy_host_render_rgba` → blit.

---

## Practical notes

- **Pixel format:** the engine emits RGBA8 straight-alpha, row-major top-to-bottom. Most
  panels want RGB888/RGB565/BGRA — convert in your blit. Alpha is usually dropped (the
  example PPM writers drop it).
- **Sizing:** `render_rgba(w, h)` lays out to that exact logical size; pass your panel's
  pixel dimensions. `resize(w, h)` sets the viewport used for hit-testing — keep them equal.
  The C ABI caps a dimension at 8192 and a batch at 1 MiB.
- **Redraw discipline:** only re-render when something changed (a reactive batch, a hover
  change reported by `hover()`/`set_hover`, or an event). On a slow MCU this matters.
- **Input:** map your touch/pointer hardware to `pointer(x, y, button, event)` for clicks
  and `hover(x, y)` for `:hover`; then `pump(ctx)` (C++) or `poll_events` (C) to fire
  handlers. Keyboard/focus events feed the same queue (the C-ABI focus/active extern surface
  is a documented follow-up; the Rust host API already has `set_focus`/`set_active`).
- **Memory:** the whole lite path is `alloc`-only — provide a heap. RGBA8 at the panel size
  dominates RAM (e.g. 480×360×4 ≈ 691 KB); reuse one framebuffer.
