//! Canopy demo — UI logic only (no windowing).
//!
//! [`build`] assembles a small click-driven app on the Canopy crates: a reactive
//! counter, a `Memo`-derived parity line, and a removable list with a live count.
//! Both the windowed binary (`src/bin/window.rs`) and the headless renderer
//! (`src/bin/render.rs`) drive this same UI, so the example proves the whole stack
//! — signals + ops + retained tree + flex layout + baked-font text — without a GPU.
//!
//! Nothing here references `winit`; the windowing lives entirely in the windowed
//! binary, so this module (and the headless renderer) build with zero UI deps.

use canopy_dom::{Dom, ROOT};
use canopy_paint::{hit_test, layout, BG, DIRECTION, FG, GAP, HEIGHT, PADDING, WIDTH};
use canopy_protocol::{HandlerId, NodeId};
use canopy_signals::Signal;
use canopy_traits::{Point, Size};
use canopy_view::{App, CLICK, COLUMN, ROW};

/// Logical viewport the demo is designed for.
pub const VIEW_W: f32 = 360.0;
/// Logical viewport height.
pub const VIEW_H: f32 = 300.0;

// Catppuccin-ish palette.
const BASE: &str = "#1e1e2e";
const TEXT: &str = "#cdd6f4";
const GREEN: &str = "#a6e3a1";
const YELLOW: &str = "#f9e2af";
const SURFACE: &str = "#313244";
const SUBTLE: &str = "#6c7086";

/// The built demo: the reactive [`App`] plus the signals a host might poke directly.
pub struct Demo {
    /// The reactive app (produces op batches, receives dispatched events).
    pub app: App,
    /// The counter value (exposed so the headless renderer can set a lively start).
    pub count: Signal<i32>,
}

fn style_button(app: &App, node: NodeId) {
    app.style(node, BG, SURFACE);
    app.style(node, PADDING, "5");
    app.style(node, HEIGHT, "16");
}

/// Assemble the demo UI and return the live [`App`].
pub fn build() -> Demo {
    let app = App::new();
    let rt = app.runtime();

    // Root column filling the viewport.
    let root = app.el(COLUMN);
    app.style(root, DIRECTION, "column");
    app.style(root, BG, BASE);
    app.style(root, PADDING, "16");
    app.style(root, GAP, "12");
    app.style(root, WIDTH, "360");
    app.mount(ROOT, root);

    let header = app.label("Canopy demo");
    app.style(header, FG, TEXT);
    app.style(header, HEIGHT, "22");
    app.mount(root, header);

    // --- Counter: [-]  Count: N  [+] ---
    let count = rt.signal(0i32);

    let crow = app.el(ROW);
    app.style(crow, DIRECTION, "row");
    app.style(crow, GAP, "10");
    app.mount(root, crow);

    let dec = app.button("-");
    style_button(&app, dec);
    {
        let count = count.clone();
        app.on_click(dec, move |_| count.update(|n| *n -= 1));
    }
    app.mount(crow, dec);

    let display = app.label("");
    app.style(display, FG, GREEN);
    app.style(display, HEIGHT, "20");
    {
        let count = count.clone();
        app.bind_text(display, move || format!("Count: {}", count.get()));
    }
    app.mount(crow, display);

    let inc = app.button("+");
    style_button(&app, inc);
    {
        let count = count.clone();
        app.on_click(inc, move |_| count.update(|n| *n += 1));
    }
    app.mount(crow, inc);

    // --- Memo-derived parity line (updates only when parity flips) ---
    let parity = app.label("");
    app.style(parity, FG, YELLOW);
    app.style(parity, HEIGHT, "18");
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
    app.style(list, DIRECTION, "column");
    app.style(list, GAP, "6");
    app.mount(root, list);

    let remaining = rt.signal(3i32);
    let emitter = app.emitter();
    for i in 1..=3 {
        let item_row = app.el(ROW);
        app.style(item_row, DIRECTION, "row");
        app.style(item_row, GAP, "8");
        app.mount(list, item_row);

        let item_label = app.label(&format!("item {i}"));
        app.style(item_label, FG, TEXT);
        app.style(item_label, HEIGHT, "18");
        app.mount(item_row, item_label);

        let remove = app.button("x");
        style_button(&app, remove);
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
    app.style(footer, FG, SUBTLE);
    app.style(footer, HEIGHT, "16");
    {
        let remaining = remaining.clone();
        app.bind_text(footer, move || format!("{} items remaining", remaining.get()));
    }
    app.mount(root, footer);

    Demo { app, count }
}

/// Resolve a pointer position to the click handler that should fire, by laying the
/// `dom` out for `viewport`, hit-testing the topmost node, and walking up to the
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
