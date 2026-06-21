# Canopy architecture

## The pipeline

```
  GUEST (your app)                          HOST (the engine)
  ───────────────                           ─────────────────
  author tree + CSS                         decode + validate every handle  ──▶ retained tree (canopy-dom)
        │                                            │
        ▼  emit ops                                  ▼  style cascade (lite or capable tier)
  canopy-protocol bytes  ──[transport]──▶     fold matched rules onto nodes (non-destructive)
  (a BeginBatch..EndBatch frame)                     │
        ▲                                            ▼  layout (Taffy) → DisplayList (renderer-agnostic)
        │  events (DispatchEvent)                    ▼  rasterize
  poll/pump back to handlers                  RGBA8 framebuffer  /  GPU scene
```

The **op-stream** is the only interface. The guest never holds a real DOM pointer; it
holds opaque `NodeId`s the host minted and emits a validated batch of ops. The host
re-validates ownership on every op, so a forged or unowned handle is rejected at the
boundary. **The DOM-access boundary IS the plugin-permission boundary** — a guest can
mutate exactly the nodes it was handed and nothing else.

## The op-stream (canopy-protocol)

A batch is `BeginBatch(version, seq) op* EndBatch`. Every op is one tag byte then its
fields in declaration order, little-endian. Handle widths: `NodeId` u64, `HandlerId` u32,
`StrId` u32, `ElementTag`/`PropId`/`EventKind` u16, `AttrId` u16. Strings are UTF-8,
interned once. The wire contract is documented in `crates/canopy-abi/include/canopy_protocol.h`
(machine-checked against the Rust constants by `crates/canopy-abi/tests/protocol_header.rs`).

Op tags: `CreateElement`, `CreateText`, `RemoveNode`, `InsertBefore`, `SetText`,
`SetAttribute`, `SetInlineStyle`, `SetClass`, `RemoveClass`, `AddListener`,
`RemoveListener`, `InternString`, `SetTagName`. The one inbound op (host→guest) is
`DispatchEvent` (events), which you drain with `poll_events`/`pump`.

You almost never write ops by hand — the authoring front-ends emit them (see
`authoring.md`). The `Emitter` (`canopy-core`) is the low-level recorder both front-ends
build on; the intern table + node counter **persist across batches**.

## The two style tiers

Same authoring, two cascades behind one seam — the host picks based on the target:

| | LITE | CAPABLE |
|---|---|---|
| crate | `canopy-style-css` | `canopy-style-stylo` (real Servo Stylo) |
| std? | `#![no_std]` + alloc | std only |
| target | embedded / bare metal / the device path | desktop |
| coverage | a large, real CSS subset (see `styling.md` + `crates/canopy-style-css/FEATURES.md`) | full browser-grade CSS |

On hardware you are always on **LITE**. The lite engine is genuinely a real engine now:
type/id/class/compound selectors + combinators + attribute + structural/functional/state
pseudo-classes with correct specificity, inheritance, custom properties + `var()`/`calc()`,
`@media`, flexbox + CSS grid, gradients, shadows, anti-aliased rounded corners, and
`overflow` clipping.

## The render path

The host lays the tree out and builds a **`DisplayList`** (`canopy-traits`) — a flat,
back-to-front `Vec<DisplayItem>` (`Rect`, `Text`, `Glyphs`, `Border`, `Gradient`, `Shadow`,
`PushClip`/`PopClip`). `DisplayItem` is `#[non_exhaustive]` and every renderer dispatch has
a catch-all, so new primitives never break a renderer. Renderers:

- **`canopy-render-soft`** — the `no_std` software rasterizer. CPU-only, the device path. AA corners/borders, gradient ramps, soft shadows, clip stack.
- **`canopy-render-vello`** — GPU via wgpu (desktop, capable tier).
- **`canopy-render-text`** — text-focused CPU path.

The host that turns ops → pixels for C/C++ is **`canopy-abi`** (the `canopy_host_*` C ABI +
`CanopyHost`). For `no_std` Rust you compose the no_std crates directly (`canopy-dom` →
`canopy-paint` layout/scene → `canopy-render-soft`), as `examples/lite/embedded` does.

## Crate map (the ones you'll touch)

| Crate | Role | std? |
|---|---|---|
| `canopy-protocol` | the op-stream wire format + handle types | no_std |
| `canopy-core` | the `Emitter` (op recorder) | no_std |
| `canopy-dom` | the retained tree ops apply to (`Dom`, `Node`, `ROOT`) | no_std |
| `canopy-ui` | the Rust DX layer (`Ui` context) + hit-test + hot-reload | no_std |
| `canopy-rsx` | the `rsx!` JSX-shaped proc macro | (macro) |
| `canopy-signals` | fine-grained reactivity (`Signal`, `Memo`) | no_std |
| `canopy-style-css` | the LITE CSS engine | no_std |
| `canopy-style-stylo` | the CAPABLE (Stylo) engine | std |
| `canopy-layout-taffy` | Taffy flex/grid layout → DisplayList | no_std |
| `canopy-paint` | PropId registry + the lite scene builder | no_std |
| `canopy-render-soft` | software rasterizer | no_std |
| `canopy-traits` | `DisplayItem`/`DisplayList`/`Color`/`Rect` seam | no_std |
| `canopy-abi` | the C ABI host (`canopy.h`, `CanopyHost`) → staticlib | std |
| `canopy-host` | the windowed desktop host | std |
| `canopy-cli` | `canopy new` / `canopy build` scaffolder | std |
| `bindings/canopy_cpp` | the freestanding C++ DSL + `host` wrapper | C++ (frt) |
| `runtime/frt` | the freestanding C++ runtime (platform seam for bare metal) | C++ |

See `docs/ARCHITECTURE.md` for the deeper treatment of lowering, the capability boundary,
and the rendering tiers.
