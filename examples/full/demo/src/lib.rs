//! Canopy demo — UI logic only (no windowing).
//!
//! [`build`] assembles a small click-driven app on the Canopy crates: a reactive
//! counter, a `Memo`-derived parity line, and a removable list with a live count.
//! Styling is authored as a real **CSS stylesheet** ([`canopy_style_css`]) with
//! class rules (including `:hover`), and geometry is computed by the real **Taffy**
//! flexbox engine ([`canopy_layout_taffy`]). Both the windowed binary and the
//! headless renderer drive this same UI, so the example exercises the whole stack —
//! signals + ops + retained tree + CSS + Taffy layout + **real antialiased text**
//! ([`canopy_render_text`]) — and the windowed binary additionally drives the
//! `:hover` cascade off the live cursor.
//!
//! Nothing here references `winit`; windowing lives entirely in the windowed binary.
//! The pieces the windowed host needs to drive hover — the parsed [`Stylesheet`] and
//! the list of hoverable `(NodeId, classes)` — are returned on [`Demo`] so the glue
//! code can re-resolve a node's style when the cursor enters or leaves it.

use canopy_dom::{Dom, ROOT};
use canopy_layout_taffy::{hit_test, layout};
use canopy_protocol::{HandlerId, NodeId};
use canopy_signals::Signal;
use canopy_style_css::Stylesheet;
use canopy_traits::{Point, Size};
use canopy_transport_wasmtime::PluginHost;
use canopy_view::{App, CLICK, COLUMN, ROW};

/// Logical viewport: the app UI on the left, the untrusted-plugin panel on the right.
pub const VIEW_W: f32 = 720.0;
/// Logical viewport height.
pub const VIEW_H: f32 = 320.0;

/// The demo's stylesheet — authored as CSS class rules, parsed at build time and
/// expanded onto nodes via [`canopy_style_css`]. Catppuccin-ish palette.
///
/// The `:hover` rules are the interactive layer: the windowed host hit-tests the
/// cursor each move and re-resolves the hovered node's classes with `hovered = true`
/// (see [`Demo::hoverables`] and the windowed binary), so the buttons lighten to
/// `#585b70` (Catppuccin *overlay0*) under the pointer and the field's border-ish
/// background warms a step. Only the windowed binary drives this; the headless shot
/// renders the base (un-hovered) state.
const STYLES: &str = "
.root   { background: #1e1e2e; padding: 16px; gap: 12px; direction: column; width: 360px }
.header { color: #cdd6f4; height: 22px }
.row    { direction: row; gap: 10px }
.btn    { background: #313244; padding: 5px }
.btn:hover { background: #585b70 }
.count  { color: #a6e3a1; height: 20px }
.parity { color: #f9e2af; height: 18px }
.list   { direction: column; gap: 6px }
.itemrow { direction: row; gap: 8px }
.item   { color: #cdd6f4; height: 18px }
.footer { color: #6c7086; height: 16px }
.field  { background: #313244; padding: 5px }
.field:hover { background: #45475a }
.fieldtext { color: #cdd6f4; height: 18px }
";

/// The class list shared by every clickable button (`-`, `+`, and each `x`). It is a
/// `'static` slice so the windowed host can retain it in [`Demo::hoverables`] and
/// replay it through [`Stylesheet::apply_state`] without re-allocating per frame.
const BTN_CLASSES: &[&str] = &["btn"];

/// The built demo: the reactive [`App`] plus everything a host needs to drive it.
pub struct Demo {
    /// The reactive app (produces op batches, receives dispatched events).
    pub app: App,
    /// The counter value (exposed so the headless renderer can set a lively start).
    pub count: Signal<i32>,
    /// The text-input field node, so the host can route typed keys to it.
    pub input: NodeId,
    /// The parsed stylesheet, retained so the windowed host can re-resolve a node's
    /// classes (with `:hover`) when the cursor enters or leaves it via
    /// [`Stylesheet::apply_state`]. The headless renderer ignores it.
    pub css: Stylesheet,
    /// Every node that reacts to `:hover`, paired with the classes to re-resolve it
    /// with. The windowed host hit-tests the cursor, finds the matching entry, and
    /// flips that node's hover state (see the windowed binary). Empty entries here
    /// would simply never light up; today it holds the three buttons.
    pub hoverables: Vec<(NodeId, &'static [&'static str])>,
}

/// Assemble the demo UI and return the live [`App`].
pub fn build() -> Demo {
    let app = App::new();
    let rt = app.runtime();
    let css = canopy_style_css::parse(STYLES);
    // Collect the nodes that should react to `:hover` (the buttons) as we build them,
    // so the windowed host can re-resolve their style when the cursor crosses them.
    let mut hoverables: Vec<(NodeId, &'static [&'static str])> = Vec::new();

    let root = app.el(COLUMN);
    css.apply(&app, root, &["root"]);
    app.mount(ROOT, root);

    let header = app.label("Canopy demo");
    css.apply(&app, header, &["header"]);
    app.mount(root, header);

    // A focused text-input field — the windowed host routes typed keys here.
    let field = app.text_input("todo: ");
    css.apply(&app, field, &["field"]);
    if let Some(text_node) = app.input_text_node(field) {
        css.apply(&app, text_node, &["fieldtext"]);
    }
    app.focus(field);
    app.mount(root, field);

    // --- Counter: [-]  Count: N  [+] ---
    let count = rt.signal(0i32);

    let crow = app.el(ROW);
    css.apply(&app, crow, &["row"]);
    app.mount(root, crow);

    let dec = app.button("-");
    css.apply(&app, dec, BTN_CLASSES);
    hoverables.push((dec, BTN_CLASSES));
    {
        let count = count.clone();
        app.on_click(dec, move |_| count.update(|n| *n -= 1));
    }
    app.mount(crow, dec);

    let display = app.label("");
    css.apply(&app, display, &["count"]);
    {
        let count = count.clone();
        app.bind_text(display, move || format!("Count: {}", count.get()));
    }
    app.mount(crow, display);

    let inc = app.button("+");
    css.apply(&app, inc, BTN_CLASSES);
    hoverables.push((inc, BTN_CLASSES));
    {
        let count = count.clone();
        app.on_click(inc, move |_| count.update(|n| *n += 1));
    }
    app.mount(crow, inc);

    // --- Memo-derived parity line (updates only when parity flips) ---
    let parity = app.label("");
    css.apply(&app, parity, &["parity"]);
    {
        let count = count.clone();
        let even = rt.memo(move || count.get() % 2 == 0);
        app.bind_text(parity, move || {
            if even.get() {
                "parity: even".to_string()
            } else {
                "parity: odd".to_string()
            }
        });
    }
    app.mount(root, parity);

    // --- Removable list with a live remaining-count footer ---
    let list = app.el(COLUMN);
    css.apply(&app, list, &["list"]);
    app.mount(root, list);

    let remaining = rt.signal(3i32);
    let emitter = app.emitter();
    for i in 1..=3 {
        let item_row = app.el(ROW);
        css.apply(&app, item_row, &["itemrow"]);
        app.mount(list, item_row);

        let item_label = app.label(&format!("item {i}"));
        css.apply(&app, item_label, &["item"]);
        app.mount(item_row, item_label);

        let remove = app.button("x");
        css.apply(&app, remove, BTN_CLASSES);
        hoverables.push((remove, BTN_CLASSES));
        {
            let emitter = emitter.clone();
            let remaining = remaining.clone();
            app.on_click(remove, move |_| {
                emitter.borrow_mut().remove(item_row);
                remaining.update(|n| *n -= 1);
            });
        }
        app.mount(item_row, remove);
    }

    let footer = app.label("");
    css.apply(&app, footer, &["footer"]);
    {
        let remaining = remaining.clone();
        app.bind_text(footer, move || {
            format!("{} items remaining", remaining.get())
        });
    }
    app.mount(root, footer);

    Demo {
        app,
        count,
        input: field,
        css,
        hoverables,
    }
}

/// Load and run the untrusted wasm plugin in a Wasmtime sandbox, returning the host
/// (whose `dom()` holds the UI the plugin built) — or `None` if the wasm is missing
/// or the sandbox rejects it. The plugin is granted exactly one import and cannot
/// reach the filesystem, network, or this app's tree; the host composites its result
/// into a panel. The wasm path is baked in by `build.rs`.
pub fn run_plugin() -> Option<PluginHost> {
    let wasm = std::fs::read(env!("CANOPY_PLUGIN_WASM")).ok()?;
    let mut host = PluginHost::new().ok()?;
    host.run(&wasm).ok()?;
    Some(host)
}

/// Resolve a pointer position to the click handler that should fire, using the real
/// Taffy layout for `viewport`, hit-testing the topmost node, and walking up to the
/// nearest ancestor with a `click` listener. Returns `None` if nothing is hit.
pub fn click_handler(dom: &Dom, viewport: Size, point: Point) -> Option<HandlerId> {
    let (_scene, lay) = layout(dom, viewport);
    let mut node = hit_test(&lay, point)?;
    loop {
        let n = dom.node(node)?;
        if let Some((_, handler)) = n.listeners.iter().find(|(ev, _)| *ev == CLICK) {
            return Some(*handler);
        }
        node = n.parent?;
    }
}

/// Resolve `point` to the [`Demo::hoverables`] entry under the cursor, if any.
///
/// This mirrors [`click_handler`]: it lays out `dom` at `viewport`, hit-tests the
/// topmost node, then walks up the parent chain to the nearest ancestor that appears
/// in `hoverables` (a button's text label is a child of the button element, so the
/// raw hit is usually one level below the hoverable node). Returns the matching
/// `(NodeId, classes)` so the host can flip exactly that node's `:hover` state.
///
/// Returns `None` when nothing is hit or no ancestor is hoverable — e.g. the cursor
/// is over the background or inside the untrusted-plugin panel.
pub fn hover_target(
    dom: &Dom,
    viewport: Size,
    point: Point,
    hoverables: &[(NodeId, &'static [&'static str])],
) -> Option<(NodeId, &'static [&'static str])> {
    let (_scene, lay) = layout(dom, viewport);
    let mut node = hit_test(&lay, point)?;
    loop {
        if let Some(entry) = hoverables.iter().find(|(id, _)| *id == node) {
            return Some(*entry);
        }
        node = dom.node(node)?.parent?;
    }
}
