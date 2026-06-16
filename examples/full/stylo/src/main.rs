//! Full-tier demo: a **real Servo-Stylo cascade** drives Canopy rendering.
//!
//! This builds a small styled UI tree, resolves every node's style through
//! [`canopy_style_stylo::StyloEngine`] (the genuine Stylo cascade behind Canopy's
//! [`StyleEngine`] trait), and rasterizes the result to a PPM with the lite CPU
//! software renderer. The stylesheet deliberately exercises things the constrained-tier
//! `canopy-style-css` engine *cannot* do, and the output makes them visible:
//!
//! - **inheritance** — `.app { color }` flows to descendants that set no color of their
//!   own (the heading, the rows, the footer all take the page ink);
//! - **specificity** — the card's heading is matched by both `.title` and
//!   `.card .title`; the more specific rule wins (white + larger), while the *top-level*
//!   `.title` (not under a card) keeps only the inherited ink and the base size;
//! - **descendant combinators** — `.card .title` applies only inside a `.card`;
//! - **id selectors** — `#cta` paints the call-to-action blue.
//!
//! Run: `cargo +nightly run [out.ppm]`.

use canopy_protocol::NodeId;
use canopy_render_soft::Buffer;
use canopy_style_stylo::StyloEngine;
use canopy_traits::{Color, ComputedStyle, Point, Rect, Size, StyleEngine};

/// The author stylesheet — ordinary CSS, cascaded for real by Stylo.
const CSS: &str = "
.app   { color: #c8ccd4; background: #15171c; font-size: 16px; padding: 22px }
.title { font-size: 19px }
.card  { background: #21242b; padding: 16px; border-radius: 10px }
.card .title { color: #ffffff; font-size: 26px }
.row   { font-size: 15px; padding: 7px }
#cta   { background: #4c8bf5; color: #ffffff; padding: 12px; border-radius: 8px }
.muted { color: #6b7080; font-size: 13px }
";

const VIEW_W: usize = 620;
const VIEW_H: usize = 460;

/// One node we lay out + paint: its slab id, a label to draw, and element children.
struct Item {
    id: usize,
    label: &'static str,
    children: Vec<usize>,
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "stylo.ppm".to_string());

    // --- Build the styled tree in the Stylo engine's DOM. ---
    let mut engine = StyloEngine::new(CSS);
    let mut items: Vec<Item> = Vec::new();
    let doc = engine.document_mut();

    // Helper closures can't borrow `doc` repeatedly cleanly, so inline the adds.
    let app = doc.add_element(0, "div", None, &["app"]);
    let heading = doc.add_element(app, "div", None, &["title"]);
    let card = doc.add_element(app, "div", None, &["card"]);
    let card_title = doc.add_element(card, "div", None, &["title"]);
    let row1 = doc.add_element(card, "div", None, &["row"]);
    let row2 = doc.add_element(card, "div", None, &["row"]);
    let cta = doc.add_element(card, "div", Some("cta"), &["row"]);
    let footer = doc.add_element(app, "div", None, &["muted"]);

    // Mirror the structure for layout/paint, with the labels we draw.
    items.push(Item {
        id: app,
        label: "",
        children: vec![heading, card, footer],
    });
    items.push(Item {
        id: heading,
        label: "Canopy  x  Stylo",
        children: vec![],
    });
    items.push(Item {
        id: card,
        label: "",
        children: vec![card_title, row1, row2, cta],
    });
    items.push(Item {
        id: card_title,
        label: "Settings",
        children: vec![],
    });
    items.push(Item {
        id: row1,
        label: "Theme:  dark",
        children: vec![],
    });
    items.push(Item {
        id: row2,
        label: "Reduce motion:  off",
        children: vec![],
    });
    items.push(Item {
        id: cta,
        label: "Apply",
        children: vec![],
    });
    items.push(Item {
        id: footer,
        label: "stylo 0.18  -  inheritance + specificity + combinators",
        children: vec![],
    });

    // --- Resolve the whole tree once (the real cascade). ---
    engine.resolve_styles();

    // Print the resolved cascade so it's legible in the terminal too.
    println!("resolved cascade (label -> color / background / font-size / display):");
    for it in &items {
        let s = style_of(&mut engine, it.id);
        let name = if it.label.is_empty() {
            "<container>"
        } else {
            it.label
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

    // --- Paint the resolved styles. ---
    let mut buf = Buffer::new(VIEW_W, VIEW_H);
    buf.clear(Color {
        r: 0x0c,
        g: 0x0d,
        b: 0x10,
        a: 255,
    });

    let lookup: std::collections::HashMap<usize, &Item> =
        items.iter().map(|it| (it.id, it)).collect();
    paint(
        &mut engine,
        &lookup,
        app,
        16.0,
        16.0,
        (VIEW_W as f32) - 32.0,
        &mut buf,
    );

    std::fs::write(&path, buf.to_ppm()).expect("write ppm");
    println!("\nwrote {path} ({VIEW_W}x{VIEW_H}) — styled by Stylo, rasterized on the CPU");
}

/// Resolve one node's flat [`ComputedStyle`] via the real Stylo cascade.
fn style_of(engine: &mut StyloEngine, id: usize) -> ComputedStyle {
    engine
        .resolve(NodeId::new(id as u64), None)
        .expect("resolve")
}

/// Lay out + paint `id` within `[x, y]` at `width`, returning the height it consumed.
/// A tiny block flow: a container stacks its children (inset by its padding) and paints
/// its background behind them; a leaf paints its background band and draws its label in
/// its resolved color at its resolved font size.
fn paint(
    engine: &mut StyloEngine,
    items: &std::collections::HashMap<usize, &Item>,
    id: usize,
    x: f32,
    y: f32,
    width: f32,
    buf: &mut Buffer,
) -> f32 {
    let item = items[&id];
    let s = style_of(engine, id);
    let pad = s.padding;

    if item.children.is_empty() {
        // Leaf: one band of height = font-size + 2*padding.
        let h = s.font_size + pad * 2.0;
        fill_bg(buf, x, y, width, h, &s);
        // Text baseline-ish: draw the label inset by padding, in the resolved color.
        buf.blit_text(
            Point {
                x: x + pad,
                y: y + pad,
            },
            item.label,
            s.color,
            s.font_size,
        );
        return h;
    }

    // Container: measure children top-down, painting our background first (behind).
    // Two-pass: first compute total height, draw bg, then redraw children over it.
    let gap = 10.0;
    let inner_x = x + pad;
    let inner_w = width - pad * 2.0;

    // Pass 1: measure children heights (paint into a throwaway is wasteful, so compute).
    let mut total = pad;
    for (i, &ch) in item.children.iter().enumerate() {
        let ch_h = measure(engine, items, ch, inner_w);
        total += ch_h;
        if i + 1 < item.children.len() {
            total += gap;
        }
    }
    total += pad;

    // Paint our background panel.
    fill_bg(buf, x, y, width, total, &s);

    // Pass 2: paint children over it.
    let mut cur_y = y + pad;
    for &ch in &item.children {
        let ch_h = paint(engine, items, ch, inner_x, cur_y, inner_w, buf);
        cur_y += ch_h + gap;
    }
    total
}

/// Height `id` would consume at `width` (mirrors [`paint`]'s flow, no drawing).
fn measure(
    engine: &mut StyloEngine,
    items: &std::collections::HashMap<usize, &Item>,
    id: usize,
    width: f32,
) -> f32 {
    let item = items[&id];
    let s = style_of(engine, id);
    let pad = s.padding;
    if item.children.is_empty() {
        return s.font_size + pad * 2.0;
    }
    let gap = 10.0;
    let inner_w = width - pad * 2.0;
    let mut total = pad;
    for (i, &ch) in item.children.iter().enumerate() {
        total += measure(engine, items, ch, inner_w);
        if i + 1 < item.children.len() {
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
