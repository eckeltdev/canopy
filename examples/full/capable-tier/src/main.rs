//! **The tiered StyleEngine, proven end to end.**
//!
//! One Canopy UI tree, authored *once* with the [`Ui`] layer ([`build_app`]) in
//! identity-carrying mode ([`Ui::capable`]), so the retained [`Dom`] carries real element
//! identity (tag-name / class / id). That single Dom is then cascaded **two ways**, by two
//! different host engines bound behind the *same* [`StyleEngine`](canopy_traits::StyleEngine)
//! trait:
//!
//! - **Lite tier** — [`canopy_style_css::LiteEngine`]: the constrained-tier resolver. Its
//!   language is the flat class subset — class → declarations, no combinators, no
//!   selector-driven inheritance. `no_std`, embeddable.
//! - **Capable tier** — `StyloEngine` ([`canopy_style_stylo`]): the full **Servo-Stylo**
//!   cascade — inheritance, specificity, and **descendant combinators**.
//!
//! The unification is literal: both tiers resolve through the one
//! [`resolve_tree`]`(&mut dyn StyleEngine, &dom)` call — that shared call site *is* the
//! tiered seam, and the only thing that varies is which engine is plugged in. The headline
//! contrast is a single rule — `.card .title { color: gold }` — that the lite language
//! *cannot represent*: the `.title` nested inside `.card` resolves gold under Stylo and
//! plain under lite. Both tiers are rasterized by the *same* CPU renderer, so the only
//! variable is the cascade.
//!
//! Run: `cargo run` → writes `capable-lite.ppm` + `capable-stylo.ppm` and prints the
//! resolved-color contrast to the terminal.

use std::collections::{BTreeMap, HashMap};

use canopy_dom::Dom;
use canopy_protocol::NodeId;
use canopy_render_soft::Buffer;
use canopy_style_stylo::StyloEngine;
use canopy_traits::{Color, ComputedStyle, OpSink, Point, Rect, Size};
use canopy_ui::prelude::{resolve_tree, Classes, LiteEngine, Ui};

/// Logical canvas size for each tier's PPM.
const VIEW_W: usize = 380;
/// Logical canvas height.
const VIEW_H: usize = 300;

/// The page color behind the `.app` panel.
const CLEAR: Color = Color {
    r: 0x0c,
    g: 0x0d,
    b: 0x10,
    a: 255,
};

/// The flat class subset the **lite** engine can express: class → declarations, with no
/// combinators and no inheritance. This is the whole language available author-side.
const LITE_CSS: &str = "\
.app   { background: #14161c; padding: 18 }
.title { color: #e8eaf0 }
.card  { background: #1c2030; padding: 14 }
.row   { color: #aeb4c2 }
.muted { color: #5b6172 }
";

/// The full CSS the **capable** (Stylo) engine resolves over the real tree — a strict
/// superset of [`LITE_CSS`] that adds a **descendant combinator** (`.card .title`) the
/// lite language has no way to represent.
const CAPABLE_CSS: &str = "\
.app   { background: #14161c; padding: 18px; color: #e8eaf0 }
.title { color: #e8eaf0 }
.card  { background: #1c2030; padding: 14px }
.card .title { color: #f5b301 }
.row   { color: #aeb4c2 }
.muted { color: #5b6172 }
";

/// One node we lay out + paint: its real [`NodeId`], the text to draw (empty for a pure
/// container), and its element children.
struct Item {
    node: NodeId,
    label: &'static str,
    kids: Vec<NodeId>,
}

/// The authored tree: the root, every paintable item, and the one node that proves the
/// point — the `.title` nested inside `.card`.
struct Authored {
    root: NodeId,
    items: Vec<Item>,
    nested_title: NodeId,
}

/// Author the demo tree **once**. Identical for both tiers: both author in
/// identity-carrying mode (`Ui::capable`), so the Dom records each node's `tag`/`class`
/// identity for the host engine to cascade — only the engine and CSS differ. Because
/// both start from a fresh `App`, the handle ids match across tiers, so a single contrast
/// table lines up by node.
fn build_app(ui: &Ui) -> Authored {
    let mut items = Vec::new();

    let app = container(ui, &mut items, &["app"]);
    let heading = label_band(ui, &mut items, &["title"], "Canopy  —  capable tier");
    let card = container(ui, &mut items, &["card"]);
    let nested_title = label_band(ui, &mut items, &["title"], "Settings");
    let row = label_band(ui, &mut items, &["row"], "Theme:  dark");
    let footer = label_band(
        ui,
        &mut items,
        &["muted"],
        "tiered StyleEngine: lite vs Stylo",
    );

    // Wire the tree: app › [heading, card › [nested_title, row], footer].
    ui.mount(card, nested_title);
    ui.mount(card, row);
    ui.mount(app, heading);
    ui.mount(app, card);
    ui.mount(app, footer);
    ui.mount_root(app);

    // Containers are recorded after their children so the kid handles already exist.
    set_kids(&mut items, app, &[heading, card, footer]);
    set_kids(&mut items, card, &[nested_title, row]);

    Authored {
        root: app,
        items,
        nested_title,
    }
}

/// Create a `div` container with `classes` (no text); record it as a paintable item with
/// no children yet (filled in by [`set_kids`] once the tree is wired).
fn container(ui: &Ui, items: &mut Vec<Item>, classes: Classes) -> NodeId {
    let el = ui.column();
    ui.tag(el, "div"); // CSS local name carried to the host engine (both tiers author in capable mode)
    ui.class(el, classes);
    items.push(Item {
        node: el,
        label: "",
        kids: Vec::new(),
    });
    el
}

/// Create a `div` carrying `classes` with a single text child — a label band we paint as
/// background + `text` in the element's resolved color.
fn label_band(ui: &Ui, items: &mut Vec<Item>, classes: Classes, text: &'static str) -> NodeId {
    let el = ui.column();
    ui.tag(el, "div");
    ui.class(el, classes);
    ui.mount(el, ui.label(text));
    items.push(Item {
        node: el,
        label: text,
        kids: Vec::new(),
    });
    el
}

/// Fill in a container's child list once the tree is wired.
fn set_kids(items: &mut [Item], node: NodeId, kids: &[NodeId]) {
    if let Some(it) = items.iter_mut().find(|it| it.node == node) {
        it.kids = kids.to_vec();
    }
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_default();
    let dir = if out.is_empty() { "." } else { out.as_str() };

    // Both tiers resolve through the SAME `resolve_tree(&mut dyn StyleEngine, &dom)`
    // helper — that single call site is the unified seam; only the engine differs.

    // ---- Lite tier: the constrained-tier StyleEngine over the class subset. -----------
    // Author in identity-carrying mode so the Dom carries class names for the host
    // engine to cascade — the *same* shape the capable tier uses; only the engine and
    // CSS differ.
    let lite = Ui::capable(LITE_CSS);
    let authored = build_app(&lite);
    let mut ldom = Dom::new();
    ldom.apply(&lite.take_batch(0)).expect("apply lite ops");
    let mut lite_engine = LiteEngine::from_dom(&ldom, LITE_CSS);
    let lite_styles = resolve_tree(&mut lite_engine, &ldom);
    let lite_buf = render_tier(&authored, &lite_styles);
    write_ppm(dir, "capable-lite.ppm", &lite_buf);

    // ---- Capable tier: the real Stylo cascade over the same real Dom. -----------------
    let cap = Ui::capable(CAPABLE_CSS);
    let authored = build_app(&cap); // identical tree → identical handle ids
    let mut cdom = Dom::new();
    cdom.apply(&cap.take_batch(0)).expect("apply capable ops");
    let mut stylo = StyloEngine::from_dom(&cdom, cap.css_source());
    let cap_styles = resolve_tree(&mut stylo, &cdom);
    let cap_buf = render_tier(&authored, &cap_styles);
    write_ppm(dir, "capable-stylo.ppm", &cap_buf);

    print_contrast(&authored, &lite_styles, &cap_styles);
}

/// Rasterize the authored tree into a fresh [`Buffer`], reading each node's style from
/// `styles` (the only thing that differs between tiers — both are produced by the same
/// [`resolve_tree`] call over a `&mut dyn StyleEngine`).
fn render_tier(app: &Authored, styles: &Styles) -> Buffer {
    let mut buf = Buffer::new(VIEW_W, VIEW_H);
    buf.clear(CLEAR);
    let lookup: Lookup = app
        .items
        .iter()
        .map(|it| (it.node, (it.label, it.kids.clone())))
        .collect();
    let margin = 16.0;
    paint(
        &lookup,
        styles,
        app.root,
        margin,
        margin,
        VIEW_W as f32 - margin * 2.0,
        &mut buf,
    );
    buf
}

type Lookup = HashMap<NodeId, (&'static str, Vec<NodeId>)>;
/// Resolved styles keyed by node — a `BTreeMap` because that is what [`resolve_tree`]
/// returns.
type Styles = BTreeMap<NodeId, ComputedStyle>;

/// A tiny block flow: a container stacks its children (inset by its padding) and paints
/// its background behind them; a leaf paints its background band and draws its label in
/// its resolved color. Returns the height consumed.
fn paint(
    items: &Lookup,
    styles: &Styles,
    node: NodeId,
    x: f32,
    y: f32,
    width: f32,
    buf: &mut Buffer,
) -> f32 {
    let (label, kids) = items[&node].clone();
    let s = styles[&node];
    let pad = s.padding;

    if kids.is_empty() {
        let fs = font_size(&s);
        let h = fs + pad * 2.0;
        fill_bg(buf, x, y, width, h, &s);
        buf.blit_text(
            Point {
                x: x + pad,
                y: y + pad,
            },
            label,
            s.color,
            fs,
        );
        return h;
    }

    let gap = 10.0;
    let inner_x = x + pad;
    let inner_w = width - pad * 2.0;

    let mut total = pad;
    for (i, ch) in kids.iter().enumerate() {
        total += measure(items, styles, *ch, inner_w);
        if i + 1 < kids.len() {
            total += gap;
        }
    }
    total += pad;

    fill_bg(buf, x, y, width, total, &s);

    let mut cur_y = y + pad;
    for ch in &kids {
        let ch_h = paint(items, styles, *ch, inner_x, cur_y, inner_w, buf);
        cur_y += ch_h + gap;
    }
    total
}

/// Height `node` would consume at `width` (mirrors [`paint`]'s flow, no drawing).
fn measure(items: &Lookup, styles: &Styles, node: NodeId, width: f32) -> f32 {
    let (_, kids) = items[&node].clone();
    let s = styles[&node];
    let pad = s.padding;
    if kids.is_empty() {
        return font_size(&s) + pad * 2.0;
    }
    let gap = 10.0;
    let inner_w = width - pad * 2.0;
    let mut total = pad;
    for (i, ch) in kids.iter().enumerate() {
        total += measure(items, styles, *ch, inner_w);
        if i + 1 < kids.len() {
            total += gap;
        }
    }
    total + pad
}

/// The font size to draw at: the resolved `font_size` when set (the capable tier carries
/// Stylo's px), falling back to a legible default (the lite `ComputedStyle` leaves it 0).
fn font_size(s: &ComputedStyle) -> f32 {
    if s.font_size > 1.0 {
        s.font_size
    } else {
        16.0
    }
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

/// Print the resolved foreground color for each text band under both tiers, marking the
/// node where they diverge — the `.title` inside `.card`, gold only under Stylo.
fn print_contrast(app: &Authored, lite: &Styles, cap: &Styles) {
    println!("\nresolved foreground color  (lite class engine  vs  Stylo over the real Dom):\n");
    for it in &app.items {
        if it.label.is_empty() {
            continue; // pure containers draw no text
        }
        let l = lite[&it.node].color;
        let c = cap[&it.node].color;
        let mark = if it.node == app.nested_title {
            "   <- `.card .title` descendant combinator: gold under Stylo, plain under lite"
        } else if l != c {
            "   <- differs"
        } else {
            ""
        };
        println!(
            "  {:<34}  lite #{:02x}{:02x}{:02x}   stylo #{:02x}{:02x}{:02x}{}",
            it.label, l.r, l.g, l.b, c.r, c.g, c.b, mark
        );
    }
    println!("\nwrote capable-lite.ppm + capable-stylo.ppm — same tree, two engines.");
}

/// Write `buf` as a binary PPM under `dir`.
fn write_ppm(dir: &str, name: &str, buf: &Buffer) {
    let path = format!("{dir}/{name}");
    std::fs::write(&path, buf.to_ppm()).expect("write ppm");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build both tiers over the same authored tree and return the nested `.title`'s
    /// resolved foreground color under each — both resolved through the *same*
    /// `resolve_tree(&mut dyn StyleEngine, ..)` seam, so the test also exercises the
    /// unified consume path.
    fn nested_title_colors() -> (Color, Color) {
        // Lite tier.
        let lite = Ui::capable(LITE_CSS);
        let a = build_app(&lite);
        let mut dom = Dom::new();
        dom.apply(&lite.take_batch(0)).unwrap();
        let mut lite_engine = LiteEngine::from_dom(&dom, LITE_CSS);
        let lite_color = resolve_tree(&mut lite_engine, &dom)[&a.nested_title].color;

        // Capable tier (Stylo over the real Dom).
        let cap = Ui::capable(CAPABLE_CSS);
        let a2 = build_app(&cap);
        let mut cdom = Dom::new();
        cdom.apply(&cap.take_batch(0)).unwrap();
        let mut stylo = StyloEngine::from_dom(&cdom, cap.css_source());
        let cap_color = resolve_tree(&mut stylo, &cdom)[&a2.nested_title].color;

        (lite_color, cap_color)
    }

    #[test]
    fn the_descendant_combinator_is_capable_only() {
        let (lite, cap) = nested_title_colors();

        // Lite can only express the flat `.title` rule → the plain light color.
        assert_eq!(
            lite,
            Color {
                r: 0xe8,
                g: 0xea,
                b: 0xf0,
                a: 255
            },
            "lite resolves the nested title to the flat `.title` color"
        );
        // Stylo resolves `.card .title` over the real Dom → gold.
        assert_eq!(
            cap,
            Color {
                r: 0xf5,
                g: 0xb3,
                b: 0x01,
                a: 255
            },
            "capable tier applies the `.card .title` descendant combinator"
        );
        assert_ne!(lite, cap, "the two tiers must diverge on the nested title");
    }
}
