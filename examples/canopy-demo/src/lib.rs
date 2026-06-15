//! Canopy demo — UI logic only (no windowing).
//!
//! [`build`] assembles a small click-driven app on the Canopy crates: a reactive
//! counter, a `Memo`-derived parity line, and a removable list with a live count.
//! Styling is authored as a real **CSS stylesheet** ([`canopy_style_css`]) with
//! class rules, and geometry is computed by the real **Taffy** flexbox engine
//! ([`canopy_layout_taffy`]). Both the windowed binary and the headless renderer
//! drive this same UI, so the example exercises the whole stack — signals + ops +
//! retained tree + CSS + Taffy layout + baked-font text — without a GPU.
//!
//! Nothing here references `winit`; windowing lives entirely in the windowed binary.

use canopy_dom::{Dom, ROOT};
use canopy_layout_taffy::{hit_test, layout};
use canopy_protocol::HandlerId;
use canopy_signals::Signal;
use canopy_traits::{Point, Size};
use canopy_view::{App, CLICK, COLUMN, ROW};

/// Logical viewport the demo is designed for.
pub const VIEW_W: f32 = 360.0;
/// Logical viewport height.
pub const VIEW_H: f32 = 300.0;

/// The demo's stylesheet — authored as CSS class rules, parsed at build time and
/// expanded onto nodes via [`canopy_style_css`]. Catppuccin-ish palette.
const STYLES: &str = "
.root   { background: #1e1e2e; padding: 16px; gap: 12px; direction: column; width: 360px }
.header { color: #cdd6f4; height: 22px }
.row    { direction: row; gap: 10px }
.btn    { background: #313244; padding: 5px }
.count  { color: #a6e3a1; height: 20px }
.parity { color: #f9e2af; height: 18px }
.list   { direction: column; gap: 6px }
.itemrow { direction: row; gap: 8px }
.item   { color: #cdd6f4; height: 18px }
.footer { color: #6c7086; height: 16px }
";

/// The built demo: the reactive [`App`] plus the counter signal.
pub struct Demo {
    /// The reactive app (produces op batches, receives dispatched events).
    pub app: App,
    /// The counter value (exposed so the headless renderer can set a lively start).
    pub count: Signal<i32>,
}

/// Assemble the demo UI and return the live [`App`].
pub fn build() -> Demo {
    let app = App::new();
    let rt = app.runtime();
    let css = canopy_style_css::parse(STYLES);

    let root = app.el(COLUMN);
    css.apply(&app, root, &["root"]);
    app.mount(ROOT, root);

    let header = app.label("Canopy demo");
    css.apply(&app, header, &["header"]);
    app.mount(root, header);

    // --- Counter: [-]  Count: N  [+] ---
    let count = rt.signal(0i32);

    let crow = app.el(ROW);
    css.apply(&app, crow, &["row"]);
    app.mount(root, crow);

    let dec = app.button("-");
    css.apply(&app, dec, &["btn"]);
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
    css.apply(&app, inc, &["btn"]);
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
        css.apply(&app, remove, &["btn"]);
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
        app.bind_text(footer, move || format!("{} items remaining", remaining.get()));
    }
    app.mount(root, footer);

    Demo { app, count }
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
