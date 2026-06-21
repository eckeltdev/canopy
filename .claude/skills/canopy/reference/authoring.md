# Authoring a Canopy UI

Two front-ends, one op-stream. Both emit the same `canopy-protocol` bytes; the host can't
tell which produced a batch (the cross-producer parity test pins this). **Author identity
only — `class`/`id`/tag — and put styling in the stylesheet** (see `styling.md`).

Element vocabulary (same in both): a container (`div`/`row`/`column` — a flex box whose
row/column direction comes from CSS), a `button`, an `input`, and text leaves. The
well-known `ElementTag` ids are `COLUMN=1`, `ROW=2`, `BUTTON=3`, `INPUT=4`; the host maps
them to CSS type names `div`/`row`/`button`/`input` for type/`#id`/compound selectors.

---

## Rust — `rsx!` + the `Ui` context

`use canopy_ui::prelude::*;` brings in `Ui`, `rsx!`, `Signal`, `NodeId`, etc.

### `rsx!` — JSX/HTML-shaped macro (`canopy-rsx`)

```rust
let root = rsx!(ui =>
    <div class="card" id="hero">
        <span class="title">"Canopy"</span>
        <button class="primary" on:click={ let c = count.clone(); move |_| c.update(|n| *n += 1) }>
            { let c = count.clone(); move || format!("count is {}", c.get()) }
        </button>
        <input value="type here" />
        { logo(&ui) }            // splice an already-built NodeId (this is how components compose)
    </div>
);
ui.mount_root(root);
```

- Tags: `<div>` (flex container), `<span>`/`<label>`/`<p>` (text leaves), `<button>`, `<input/>`, `<el tag={K}>` (escape hatch for any tag id).
- Attributes: `class="a b"`, `id="x"`, `on:click={ closure }`, `value="…"`.
- Children: a string literal (`"Canopy"`), a reactive `{ move || … }` closure (re-emits one `SetText` per change), a `{ expr }` splice of a `NodeId` (component composition), or a nested element.
- A **component** is a `fn(&Ui, …) -> NodeId` that builds a subtree on the shared `Ui` and returns its root. Splice it with `{ comp(&ui) }`. Drop out of `rsx!` to imperative `Ui` calls whenever you need a node handle (e.g. to animate it) — `examples/lite/welcome` does this for its logo.

### The `Ui` context (`canopy-ui`, `no_std` + alloc, does no I/O)

| Method | Purpose |
|---|---|
| `Ui::with_css(src)` / `Ui::capable(src)` | new context, LITE / CAPABLE tier, with a stylesheet string |
| `ui.signal(v)` / `ui.memo(f)` | fine-grained reactive state |
| `ui.column()` / `row()` / `el(tag)` / `label(s)` / `label_bound(f)` / `button(s)` / `button_bound(f)` / `input(s)` | build nodes imperatively |
| `ui.mount(parent, child)` / `ui.mount_root(child)` | attach to the tree |
| `ui.class(node, &["a","b"])` / `ui.set_id(node, "x")` / `ui.tag(node, "section")` | identity |
| `ui.on_click(node, |payload| …)` | register a click handler |
| `ui.bind_text(node, f)` / `ui.bind_style(node, prop, f)` | bind a node to a reactive closure |
| `ui.take_batch(seq)` → `Vec<u8>` | drain the pending ops as a batch to apply to a host |
| `ui.set_hover/set_focus/set_active(node, bool)` | drive `:hover`/`:focus`/`:active` restyle |
| `ui.hover_target/click_handler(dom, viewport, point)` | hit-test a pointer to a node/handler |
| `ui.reload_css(src, hovered)` | swap the stylesheet (hot reload); re-styles every styled node |
| `ui.runtime()` / `ui.dispatch(handler, payload)` | flush reactive effects / fire a handler |

The host loop pattern (desktop is wired by `canopy new`; on a device you drive it):
`host.apply_bytes(&ui.take_batch(seq))` → `host.render_rgba(w, h)` → blit → on a pointer
move call `ui.set_hover(...)` (or the host's `hover`) and re-render; on a tick call
`ui.runtime().flush()` then apply the next `take_batch`.

---

## C++ — the `canopy_cpp` DSL

`#include "canopy_cpp/dsl.hpp"` (the value-building factories) and
`#include "canopy_cpp/host.hpp"` (the engine wrapper). `using namespace canopy;`. Note:
cpp-doctor enforces `snake_case` types — see the `cpp-doctor-style-guide` skill before
writing C++.

### Factories (all build lightweight value descriptions; nothing touches a context until `mount`)

```cpp
build_context ctx;
mount(ctx, div(id("hero"), cls("card"),
               div(cls("title"), text("Canopy")),
               div(cls("actions"),
                   button(cls("primary"), on_click([]{ /* handler */ }), "Open"),
                   button(cls("danger"),  on_click([]{ /* handler */ }), "Close")),
               input(cls("field"))));
```

| Factory | Emits |
|---|---|
| `div(...)` / `row(...)` / `button(...)` / `input(...)` | `CreateElement(COLUMN/ROW/BUTTON/INPUT)` |
| `el(tag)(...)` | escape hatch for any host tag id |
| `text("…")` | `CreateText` |
| `cls("name")` | `SetClass` |
| `id("x")` | `SetAttribute(ATTR_ID, "x")` |
| `attr(kind, "v")` / `style(prop, "v")` / `tag("name")` | `SetAttribute` / `SetInlineStyle` / `SetTagName` |
| `on_click([]{ … })` | `AddListener(CLICK)` + parks the closure in `ctx`'s handler table |
| `mount(ctx, node…)` | appends to the root |
| `ctx.take_batch(seq)` | drains the recorded ops as a batch |

Because these are freestanding value-builders that allocate via the frt runtime, the DSL
compiles `-fno-exceptions -fno-rtti` on bare metal.

### Driving the engine (`canopy::host`, wraps `libcanopy_abi.a`)

```cpp
host engine;                                   // = canopy_host_new()
engine.set_stylesheet(css);                    // install the lite stylesheet
engine.apply(ctx.take_batch(0));               // apply the op-stream
engine.resize(w, h);                           // viewport for hit-testing
auto rgba = engine.render_rgba(w, h);          // w*h*4 RGBA8 -> blit to a framebuffer
// input loop:
bool changed = engine.hover(px, py);           // update :hover; re-render if true
int n = engine.pointer(px, py, button, wire::event_click); // hit-test + queue an event
int fired = engine.pump(ctx);                  // drain events -> invoke the parked closures
```

See `bindings/canopy_cpp/examples/gui_css/main.cpp` for a complete, styled, rendered
example, and `bindings/canopy_cpp/examples/gui_render/` + `counter_card/` for more.
