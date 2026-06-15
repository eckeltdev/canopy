# Canopy architecture

This is the deeper dive behind the [README](../README.md): how a UI written in `rsx!`
becomes pixels, why the boundary between app logic and the UI is a security boundary, and
how one `no_std` core reaches from a desktop GPU down toward bare metal. Read the README
first for the elevator pitch and the crate map; this document explains the *why*.

## The shape, top to bottom

```
authoring        rsx!  +  canopy-ui::Ui            (Rust's React-shaped wrapper)
                   │  lowers to Ui builder calls
reactivity       canopy-signals  +  canopy-view    (one targeted op per change)
                   │  emits the batched op-stream
wire             canopy-protocol                   (opaque handles + opcodes + codec)
                   │  bytes
transport        native │ wasmtime │ component │ C ABI   (compiled-in or sandboxed)
                   │  validated apply
retained tree    canopy-dom                        (arena + handle-ownership checks)
                   │
layout           canopy-layout-taffy               (real Taffy flexbox)
                   │  DisplayList
paint / text     canopy-render-{vello,text,soft} + canopy-text-{parley,baked}
                   │
                 pixels
```

Every arrow that crosses the transport line is **bytes plus a capability check**. Nothing
above the line ever holds a pointer into the tree below it; nothing below it ever runs guest
code. That is the whole design.

## The op-stream and the capability model

App logic never mutates a tree. It *describes* mutations as a sequence of typed ops and
hands the host a batch of them. The op vocabulary ([`canopy-protocol`](../crates/canopy-protocol/src/lib.rs))
is small and explicit — `CreateElement`, `CreateText`, `SetText`, mount/child ops,
`SetInlineStyle`, `AddListener`, and `DispatchEvent` (the *only* host→guest op, carrying a
delivered input event back to the guest). Each op is a fixed-shape record encoded into a
compact little-endian batch with a sequence number, so a transport is just "move these
bytes."

Three properties turn this indirection into a real boundary:

1. **Handles are opaque and host-minted.** A `NodeId` is an integer the *host* allocated
   and handed back to the guest. It carries no pointer, no offset, no structure a guest
   could fabricate. Naming a number you weren't given does not name a node you can touch.

2. **Ownership is re-validated on every mutating op.** As the host decodes a batch into its
   arena ([`canopy-dom`](../crates/canopy-dom/src/lib.rs)), every op that references a node
   is checked against what *this guest* created. A forged or stolen handle is rejected at
   decode time — the batch is refused rather than partially applied, so a malformed or
   malicious stream cannot corrupt the tree.

3. **`apply` is the entire surface.** A guest's one and only capability is "hand the host
   these op bytes." There is no ambient authority hiding behind it — no clock, no
   filesystem, no network — unless the host explicitly wires one in. This is why a
   trusted, compiled-in module and a fully untrusted plugin can run the *same* guest code:
   they differ only in which transport carries the bytes and how hard the sandbox leans on
   the guest.

This is the sentence to remember: **the DOM-access boundary is the plugin-permission
boundary.** You don't bolt a permission system onto the side; the only thing a guest can
*do* is the thing the host validates.

### The transports

All four transports carry the identical op bytes; they differ in trust and mechanism.

- **[`canopy-transport-native`](../crates/canopy-transport-native/src/lib.rs)** — a
  same-address-space channel. Op/event batches move with no serialization. This is for a
  guest you compiled in and trust; the handle validation in `canopy-dom` still runs (it is
  a correctness check, not only a security one), but there is no sandbox.

- **[`canopy-transport-wasmtime`](../crates/canopy-transport-wasmtime/src/lib.rs)** — a
  `wasm32` core-module guest in a Wasmtime sandbox. The host grants exactly one import,
  `canopy_apply(ptr, len)`, and nothing else, then caps the guest with a memory limit,
  fuel, and epoch interruption so a runaway or hostile guest cannot wedge the host. The
  host reads the op bytes out of the guest's linear memory, bounds-checking the
  guest-supplied length, and runs them through the same validated decode path.

- **[`canopy-transport-component`](../crates/canopy-transport-component/src/lib.rs)** — the
  Component Model peer. It instantiates a real WebAssembly *component* built from the
  [WIT world](../wit/canopy.wit) and grants it exactly the `host.apply` import. Because the
  Component Model carries the op batch as an owned `list<u8>`, there is no raw pointer and
  no host-side bounds math: the boundary is type-checked. The [`world canopy-guest`](../wit/canopy.wit)
  imports *only* `host`, so a conforming guest **structurally** has no WASI, no clock, no
  filesystem, no network — the threat model expressed in the type system rather than
  enforced at runtime.

- **[`canopy-abi`](../crates/canopy-abi/src/lib.rs)** — the stable C ABI. An opaque host
  handle plus `extern "C"` functions to create it, feed it validated op-batch bytes, read
  facts back, and free it. This is the cross-language embedding seam — the door for a C,
  Swift, or Python host — and the **one** crate in the workspace allowed `unsafe`, because
  it *is* the FFI boundary. (Everywhere else, the workspace inherits `unsafe_code = "deny"`.)

### Plugin isolation in practice

[`canopy-plugin-panel`](../crates/canopy-plugin-panel/src/lib.rs) shows the boundary paying
off visually: a plugin owns its *own* retained tree and coordinate space; the host lays it
out for a panel viewport, rasterizes it standalone, and blits the result into a sub-region
of the host frame. The plugin's nodes and the host's nodes never share an arena, so a
plugin's UI can be dropped into a larger app and *cannot* reference a host node — the same
unforgeable-handle guarantee, now also a compositing boundary.

## The platform-abstraction layer (`canopy-traits`)

Everything host-specific lives behind traits in
[`canopy-traits`](../crates/canopy-traits/src/lib.rs): `StyleEngine`, `LayoutEngine`,
`TextEngine`, `Renderer`, and `Platform`. The traits speak only **Canopy-owned types**
(points, sizes, colors, a `DisplayList`) — never a vendor struct. Taffy, cosmic-text,
swash, wgpu, and AccessKit are named *only inside the leaf crates that wrap them*, never in
a signature that the core depends on.

That discipline is what makes the portability story real rather than aspirational. Swapping
the GPU renderer for the software rasterizer, or the cosmic-text engine for the baked 8×8
font, is changing which leaf crate a host links — not editing the core. The trait is the
seam; the `#[cfg]` is not.

## Reactivity: one op per change

Reactivity is fine-grained, not diff-based. [`canopy-signals`](../crates/canopy-signals/src/lib.rs)
is a signals + effects + batched-flush runtime; [`canopy-view`](../crates/canopy-view/src/lib.rs)
ties a signal to the op-emitting `App` so that when you bind a text node to a closure, the
closure runs once now and again on each change of a signal it read.

The key consequence: a changed signal does **not** re-render a subtree and diff it. It
flushes exactly the bindings that depend on that signal, and each flush emits **one
targeted `SetText`** (or one targeted style op) for the node that changed. The counter in
the welcome app, on each click, emits a single `SetText` for the button label — nothing
else moves. There is no virtual DOM and no reconciliation pass on the hot path; the
reconciler in [`canopy-core`](../crates/canopy-core/src/lib.rs) exists for static/keyed
*structural* changes, not for value updates.

## How `rsx!` lowers to `Ui`

[`rsx!`](../crates/canopy-rsx/src/lib.rs) is a `proc_macro` that parses a JSX/HTML-shaped
tree and emits method calls on a single `canopy-ui` [`Ui`](../crates/canopy-ui/src/lib.rs)
receiver — the expression `rsx!(ui => …)`. The emitted code names *no* crate paths, only
methods on `ui`, so a crate using `rsx!` needs nothing in scope but `canopy-ui`.

The lowering is mechanical and one-to-one:

| `rsx!` syntax | Lowers to |
|---|---|
| `<div>…</div>` | `ui.column()` / `ui.row()` (direction from CSS), children mounted via `ui.mount(parent, child)` |
| `<span>"text"</span>` | `ui.label("text")` |
| `<button>…</button>` | `ui.button(..)` |
| `<input value="…"/>` | `ui.input("…")` |
| `<el tag={K}>` | `ui.el(K)` — the host-element escape hatch |
| `class="a b"` | `ui.class(node, &["a", "b"])` — resolves through the stylesheet *and* records the node |
| `on:click={ closure }` | `ui.on_click(node, closure)` — closure passed verbatim |
| `{ move || String }` child | `ui.bind_text(node, closure)` — the reactive text path |
| `{ expr }` child (a `NodeId`) | spliced as a child — this is how components compose |

Because `ui.class` is the *only* styling path the macro emits, the set of styled nodes and
the hot-reload registry are the same set by construction — a node cannot be styled without
also being reloadable, so styles never silently stop updating. Hover is *derived* from that
registry crossed with the stylesheet's `:hover` rules, not hand-maintained. The upshot is
the central DX claim: **`rsx!` and the same tree written by hand emit a byte-identical
op-stream.** The macro is sugar over the core's op-emitting API, not a second runtime
layered on top of it.

`canopy-ui` itself is `no_std` + `alloc` and does no I/O. A host reads `styles.css` from
disk and passes the string into `Ui::with_css` / `Ui::reload_css`; the authoring layer
stays pure, which is what lets the same `Ui` run on a desktop host or a constrained target.

## Rendering tiers

Canopy has three rendering paths behind the one `Renderer` trait, chosen by target
capability:

- **GPU (Tier 0).** [`canopy-render-vello`](../crates/canopy-render-vello/src/lib.rs) is a
  wgpu-backed `Renderer` that rasterizes a `DisplayList` offscreen and reads it back to
  RGBA8 — the Metal path on macOS, validated headless on desktop.

- **Capable software.** [`canopy-render-text`](../crates/canopy-render-text/src/lib.rs)
  lays a `Dom` out with Taffy and rasterizes its `DisplayList` into a software RGBA buffer
  with **real antialiased glyphs** from [`canopy-text-parley`](../crates/canopy-text-parley/src/lib.rs)
  (cosmic-text shaping + swash rasterization against a bundled DejaVu Sans Mono),
  alpha-over compositing the coverage masks so AA edges look right. This is the path a
  desktop run without a usable GPU, or an SBC, falls back to.

- **Constrained / bare-metal.** [`canopy-render-soft`](../crates/canopy-render-soft/src/lib.rs)
  is a pure `no_std` CPU rasterizer, and [`canopy-text-baked`](../crates/canopy-text-baked/src/lib.rs)
  is an 8×8 monospace bitmap glyph atlas for printable ASCII with zero dependencies. This
  is the microcontroller path: no shaping engine, no GPU, a reduced style model — but the
  *same* `DisplayList` and the *same* guest code above it.

Text quality therefore degrades gracefully: real shaped antialiased glyphs where the host
can afford cosmic-text + swash, a crisp baked bitmap font where it cannot.

## The `no_std` seam, and why it is a hard rule

Fifteen crates form the guest-side core and **must stay `no_std`** (they carry
`#![cfg_attr(not(test), no_std)]` + `extern crate alloc;`):

> `canopy-protocol`, `canopy-traits`, `canopy-core`, `canopy-signals`, `canopy-view`,
> `canopy-dom`, `canopy-paint`, `canopy-style-css`, `canopy-layout-taffy`,
> `canopy-render-soft`, `canopy-text-baked`, `canopy-input`, `canopy-ui`, `canopy-host`,
> `canopy-transport-native`.

These crates never `use std`, never name a vendor type in a public signature, and use
`alloc` collections rather than `std` ones (so they don't drag in `getrandom`/`HashMap`).
The discipline is a **crate boundary, not a `#[cfg]`** — there is no `#[cfg(feature =
"std")]` fork inside a shared crate, because that is exactly how a `std` dependency sneaks
into the core unnoticed.

CI enforces it by compiling the core for a bare-metal target:

```sh
cargo +nightly build -p canopy-core --target thumbv7em-none-eabi
```

The instant a core crate pulls in `std`, that build goes red. This is the lever that keeps
the bare-metal track *reachable* (the core literally compiles for `thumbv7em-none-eabi`
today) without pretending it is *done* — Vello/wgpu, cosmic-text, and Wasmtime are
desktop/SBC-only, and a microcontroller composes the core with the software rasterizer and
the baked font instead.

## Where the standard backends sit

The eleven `std` leaf crates — the GPU and capable-software renderers, the parley text
engine, the three trust-tiered transports, the C ABI, the plugin-panel compositor, the
AccessKit a11y bridge ([`canopy-a11y`](../crates/canopy-a11y/src/lib.rs)), the hot-reload
watcher ([`canopy-hotreload`](../crates/canopy-hotreload/src/lib.rs)), and the
[`canopy`](../crates/canopy-cli/src/main.rs) CLI — are all leaves. Each may pull in real
engines and `std`, but **nothing in the core depends on them**. That is the property that
keeps every future target a backend swap rather than a rewrite, and it is the single
invariant worth protecting as Canopy grows.
