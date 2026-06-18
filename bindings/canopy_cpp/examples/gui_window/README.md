# canopy_cpp_window — a live, clickable Canopy window from C++ on frt

A real native window whose UI is authored entirely with the **canopy_cpp DSL** (normal
`std::string`/`vector`/`unique_ptr` routed through the freestanding **frt** runtime), applied to
the real Canopy engine, and rasterized by the engine's lite layout + software renderer. Clicks are
hit-tested by the engine, the matching **C++ `on_click` closure** fires through `host::pump`, the
reactive runtime flushes a surgical `SetText`, and the window redraws — so the buttons count.

```
C++ DSL (signal<int> + reactive text)  ── op-stream ──▶  canopy::host (real engine)
        ▲                                                         │ render_rgba
        │ host::pump → C++ closure → signal.set → flush           ▼
   mouse click ◀── host::pointer (hit-test) ◀──  AppKit window (blit RGBA each frame)
```

## Build & run (macOS)

```sh
./build.sh                 # cargo build -p canopy-abi, then compile main.mm + the binding + Cocoa
./canopy_cpp_window        # opens the window — click + / - to count
```

- `./canopy_cpp_window --selftest` — headless: renders, clicks the `+` button, renders again, and
  writes `frame_before.ppm` / `frame_after.ppm` (the counter moves 0 → 1). Exit 0 = the click loop
  works.
- `./canopy_cpp_window --shot out.png` — renders the real AppKit view offscreen to a PNG (verifies
  the blit path without opening a window).

## Why AppKit (not winit/softbuffer)

winit/softbuffer are Rust crates and can't invoke the C++ handler closures parked in the
`build_context`. The click→handler path here is the existing **`host::pump(ctx)`**, which is C++, so
the loop is owned by C++. AppKit (Objective-C++, `-framework Cocoa`) is the zero-dependency native
window for that — `host::render_rgba` blits straight into an `NSView`. This file is intentionally
**not** a cpp-doctor/CMake target (it is macOS-only ObjC++ glue); it is built by `build.sh`.

The same `render_rgba` + `pointer` + `pump` C ABI drives any windowing layer — a Linux/SDL or a
bare-metal framebuffer would swap only this ~50 lines of platform glue.
