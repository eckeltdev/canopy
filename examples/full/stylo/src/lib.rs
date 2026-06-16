//! Full-tier (**Stylo**) demo — shared scene: build a styled tree, resolve it through
//! the real Servo-Stylo cascade ([`canopy_style_stylo`]), and lay it out + paint it with
//! the lite CPU software renderer.
//!
//! Two binaries drive this: `src/bin/render.rs` (a headless still → PPM) and
//! `src/bin/window.rs` (a live winit window that **hot-reloads `styles.css`** — edit the
//! stylesheet, press a key, and the whole tree is re-cascaded through Stylo and redrawn).
//!
//! The stylesheet (`styles.css`, read at runtime) exercises things the constrained-tier
//! `canopy-style-css` engine cannot: **inheritance** through the tree, **specificity**,
//! and **descendant combinators**. The output makes them visible — the two `.title`
//! elements resolve differently because only one is under a `.card`.

use std::collections::HashMap;

use canopy_protocol::NodeId;
use canopy_render_soft::Buffer;
use canopy_style_stylo::StyloEngine;
use canopy_traits::{Color, ComputedStyle, Point, Rect, Size, StyleEngine};

/// Logical window size — the content lays out within this and reflows on resize.
pub const VIEW_W: usize = 620;
/// Logical window height.
pub const VIEW_H: usize = 460;

/// The page color behind the `.app` panel (outside its background).
pub const CLEAR: Color = Color {
    r: 0x0c,
    g: 0x0d,
    b: 0x10,
    a: 255,
};

/// The editable stylesheet, read at runtime so editing + saving hot-reloads the window.
pub const STYLES_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/styles.css");

/// Read `styles.css` from disk (empty string on failure, so the app still runs).
#[must_use]
pub fn load_styles() -> String {
    std::fs::read_to_string(STYLES_PATH).unwrap_or_default()
}

/// One node we lay out + paint: its slab id, a label to draw, and element children.
struct Item {
    id: usize,
    label: &'static str,
    children: Vec<usize>,
}

/// A built scene: the Stylo engine (DOM + parsed CSS, cascade resolved) plus the item
/// tree we paint. Rebuilt from scratch on each CSS hot-reload.
pub struct Scene {
    engine: StyloEngine,
    items: Vec<Item>,
    root: usize,
}

/// Build the demo scene from author `css`, run the real Stylo cascade, and return it
/// ready to render. Rebuilt wholesale on hot-reload (cheap; the tree is tiny).
#[must_use]
pub fn build(css: &str) -> Scene {
    let mut engine = StyloEngine::new(css);
    let doc = engine.document_mut();

    // The styled tree (HTML-like: tag + optional id + classes).
    let app = doc.add_element(0, "div", None, &["app"]);
    let heading = doc.add_element(app, "div", None, &["title"]);
    let card = doc.add_element(app, "div", None, &["card"]);
    let card_title = doc.add_element(card, "div", None, &["title"]);
    let row1 = doc.add_element(card, "div", None, &["row"]);
    let row2 = doc.add_element(card, "div", None, &["row"]);
    let cta = doc.add_element(card, "div", Some("cta"), &["row"]);
    let footer = doc.add_element(app, "div", None, &["muted"]);

    let items = vec![
        Item {
            id: app,
            label: "",
            children: vec![heading, card, footer],
        },
        Item {
            id: heading,
            label: "Canopy  x  Stylo",
            children: vec![],
        },
        Item {
            id: card,
            label: "",
            children: vec![card_title, row1, row2, cta],
        },
        Item {
            id: card_title,
            label: "Settings",
            children: vec![],
        },
        Item {
            id: row1,
            label: "Theme:  dark",
            children: vec![],
        },
        Item {
            id: row2,
            label: "Reduce motion:  off",
            children: vec![],
        },
        Item {
            id: cta,
            label: "Apply",
            children: vec![],
        },
        Item {
            id: footer,
            label: "stylo 0.18  -  inheritance + specificity + combinators",
            children: vec![],
        },
    ];

    // Run the whole-tree cascade once (idempotent; cached for every later resolve).
    engine.resolve_styles();

    Scene {
        engine,
        items,
        root: app,
    }
}

impl Scene {
    /// Resolve one node's flat [`ComputedStyle`] via the real Stylo cascade.
    fn style_of(&mut self, id: usize) -> ComputedStyle {
        self.engine
            .resolve(NodeId::new(id as u64), None)
            .expect("resolve")
    }

    /// Print the resolved cascade (label → color / background / font-size / display) so
    /// the effect is legible in the terminal too.
    pub fn print_cascade(&mut self) {
        let ids: Vec<(usize, &'static str)> =
            self.items.iter().map(|it| (it.id, it.label)).collect();
        println!("resolved cascade (label -> color / background / font-size / display):");
        for (id, label) in ids {
            let s = self.style_of(id);
            let name = if label.is_empty() {
                "<container>"
            } else {
                label
            };
            println!(
                "  {:<46} fg #{:02x}{:02x}{:02x}  bg #{:02x}{:02x}{:02x}{}  {:>4.0}px  {:?}",
                name,
                s.color.r,
                s.color.g,
                s.color.b,
                s.background.r,
                s.background.g,
                s.background.b,
                if s.background.a == 0 {
                    " (transparent)"
                } else {
                    "             "
                },
                s.font_size,
                s.display,
            );
        }
    }

    /// Render the scene into a fresh [`Buffer`] sized `w` × `h`. The content lays out at
    /// the top-left within a margin and reflows to the given width.
    #[must_use]
    pub fn render(&mut self, w: usize, h: usize) -> Buffer {
        let mut buf = Buffer::new(w.max(1), h.max(1));
        buf.clear(CLEAR);
        let lookup: HashMap<usize, (usize, &'static str, Vec<usize>)> = self
            .items
            .iter()
            .map(|it| (it.id, (it.id, it.label, it.children.clone())))
            .collect();
        let margin = 16.0;
        paint(
            self,
            &lookup,
            self.root,
            margin,
            margin,
            (w as f32) - margin * 2.0,
            &mut buf,
        );
        buf
    }
}

type Lookup = HashMap<usize, (usize, &'static str, Vec<usize>)>;

/// Lay out + paint `id` within `[x, y]` at `width`, returning the height it consumed.
/// A tiny block flow: a container stacks its children (inset by its padding) and paints
/// its background behind them; a leaf paints its background band and draws its label in
/// its resolved color at its resolved font size.
fn paint(
    scene: &mut Scene,
    items: &Lookup,
    id: usize,
    x: f32,
    y: f32,
    width: f32,
    buf: &mut Buffer,
) -> f32 {
    let (_, label, children) = items[&id].clone();
    let s = scene.style_of(id);
    let pad = s.padding;

    if children.is_empty() {
        let h = s.font_size + pad * 2.0;
        fill_bg(buf, x, y, width, h, &s);
        buf.blit_text(
            Point {
                x: x + pad,
                y: y + pad,
            },
            label,
            s.color,
            s.font_size,
        );
        return h;
    }

    let gap = 10.0;
    let inner_x = x + pad;
    let inner_w = width - pad * 2.0;

    // Measure children first so the container background can be drawn behind them.
    let mut total = pad;
    for (i, ch) in children.iter().enumerate() {
        total += measure(scene, items, *ch, inner_w);
        if i + 1 < children.len() {
            total += gap;
        }
    }
    total += pad;

    fill_bg(buf, x, y, width, total, &s);

    let mut cur_y = y + pad;
    for ch in &children {
        let ch_h = paint(scene, items, *ch, inner_x, cur_y, inner_w, buf);
        cur_y += ch_h + gap;
    }
    total
}

/// Height `id` would consume at `width` (mirrors [`paint`]'s flow, no drawing).
fn measure(scene: &mut Scene, items: &Lookup, id: usize, width: f32) -> f32 {
    let (_, _, children) = items[&id].clone();
    let s = scene.style_of(id);
    let pad = s.padding;
    if children.is_empty() {
        return s.font_size + pad * 2.0;
    }
    let gap = 10.0;
    let inner_w = width - pad * 2.0;
    let mut total = pad;
    for (i, ch) in children.iter().enumerate() {
        total += measure(scene, items, *ch, inner_w);
        if i + 1 < children.len() {
            total += gap;
        }
    }
    total + pad
}

/// Fill the node's background (a rounded rect), skipping fully-transparent backgrounds.
fn fill_bg(buf: &mut Buffer, x: f32, y: f32, w: f32, h: f32, s: &ComputedStyle) {
    if s.background.a == 0 {
        return;
    }
    buf.fill_round_rect(
        Rect {
            origin: Point { x, y },
            size: Size { w, h },
        },
        s.background,
        8.0,
    );
}
