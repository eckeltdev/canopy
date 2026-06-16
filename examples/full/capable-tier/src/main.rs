//! **The tiered StyleEngine, proven end to end.**
//!
//! One Canopy UI tree, authored *once* with the [`Ui`] layer ([`build_app`]), is styled
//! two ways — and the difference is the whole thesis of the project:
//!
//! - **Lite tier** ([`Ui::with_css`]): the constrained-tier engine ([`canopy_style_css`])
//!   expands class rules to inline styles *author-side*. Its language is flat — class →
//!   declarations, no combinators, no inheritance. The op-batch it produces carries the
//!   resolved styles, so the [`Dom`] ends up with inline `background`/`color` per node.
//!
//! - **Capable tier** ([`Ui::capable`]): the authored tree carries *real element identity*
//!   (tag-name / class / id) to the host. The host then runs the full **Servo-Stylo**
//!   cascade ([`canopy_style_stylo`]) over the *actual* retained [`Dom`] via
//!   [`StyloEngine::from_dom`] — inheritance, specificity, and **descendant combinators**.
//!
//! Same tree, same authoring code; swap the engine. The headline contrast is a single
//! rule — `.card .title { color: gold }` — that the lite language *cannot represent*: the
//! `.title` nested inside `.card` resolves gold under Stylo and plain under lite. Both
//! tiers are rasterized by the *same* CPU renderer, so the only variable is the cascade.
//!
//! Run: `cargo run` → writes `capable-lite.ppm` + `capable-stylo.ppm` and prints the
//! resolved-color contrast to the terminal.

use std::collections::HashMap;

use canopy_dom::Dom;
use canopy_paint::{BG, FG, PADDING};
use canopy_protocol::NodeId;
use canopy_render_soft::Buffer;
use canopy_style_stylo::StyloEngine;
use canopy_traits::{Color, ComputedStyle, OpSink, Point, Rect, Size, StyleEngine};
use canopy_ui::{Classes, Ui};

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

/// Author the demo tree **once**. Identical for both tiers: the lite tier resolves the
/// classes to inline styles here; the capable tier records `tag`/`class` as identity to
/// carry to the host. Because both start from a fresh `App`, the handle ids match across
/// tiers, so a single contrast table lines up by node.
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
    ui.tag(el, "div"); // capable tier: real CSS local name; no-op on lite
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

    // ---- Lite tier: classes → inline styles author-side. -----------------------------
    let lite = Ui::with_css(LITE_CSS);
    let authored = build_app(&lite);
    let mut ldom = Dom::new();
    ldom.apply(&lite.take_batch(0)).expect("apply lite ops");
    let lite_styles: HashMap<NodeId, ComputedStyle> = authored
        .items
        .iter()
        .map(|it| (it.node, lite_style(&ldom, it.node)))
        .collect();
    let lite_buf = render_tier(&authored, &lite_styles);
    write_ppm(dir, "capable-lite.ppm", &lite_buf);

    // ---- Capable tier: real Stylo cascade over the real Dom. -------------------------
    let cap = Ui::capable(CAPABLE_CSS);
    let authored = build_app(&cap); // identical tree → identical handle ids
    let mut cdom = Dom::new();
    cdom.apply(&cap.take_batch(0)).expect("apply capable ops");
    let mut engine = StyloEngine::from_dom(&cdom, cap.css_source());
    engine.resolve_styles();
    let cap_styles: HashMap<NodeId, ComputedStyle> = authored
        .items
        .iter()
        .map(|it| {
            let s = engine
                .resolve(it.node, None)
                .expect("resolve over the real Dom");
            (it.node, s)
        })
        .collect();
    let cap_buf = render_tier(&authored, &cap_styles);
    write_ppm(dir, "capable-stylo.ppm", &cap_buf);

    print_contrast(&authored, &lite_styles, &cap_styles);
}

/// Synthesize a [`ComputedStyle`] for the **lite** tier by reading the inline styles the
/// class engine resolved onto the [`Dom`] (background / color / padding). This is exactly
/// what the lite render pipeline consumes — here distilled to the fields we paint.
fn lite_style(dom: &Dom, node: NodeId) -> ComputedStyle {
    let mut s = ComputedStyle::default();
    if let Some(bg) = dom.style(node, BG).and_then(parse_hex) {
        s.background = bg;
    }
    if let Some(fg) = dom.style(node, FG).and_then(parse_hex) {
        s.color = fg;
    }
    if let Some(p) = dom
        .style(node, PADDING)
        .and_then(|v| v.trim().parse::<f32>().ok())
    {
        s.padding = p;
    }
    s
}

/// Parse a `#rrggbb` color (the format both tiers carry); `None` on anything else.
fn parse_hex(s: &str) -> Option<Color> {
    let hex = s.trim().strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    Some(Color {
        r: u8::from_str_radix(&hex[0..2], 16).ok()?,
        g: u8::from_str_radix(&hex[2..4], 16).ok()?,
        b: u8::from_str_radix(&hex[4..6], 16).ok()?,
        a: 255,
    })
}

/// Rasterize the authored tree into a fresh [`Buffer`], reading each node's style from
/// `styles` (the only thing that differs between tiers).
fn render_tier(app: &Authored, styles: &HashMap<NodeId, ComputedStyle>) -> Buffer {
    let mut buf = Buffer::new(VIEW_W, VIEW_H);
    buf.clear(CLEAR);
    let lookup: HashMap<NodeId, (&'static str, Vec<NodeId>)> = app
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
type Styles = HashMap<NodeId, ComputedStyle>;

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
    /// resolved foreground color under each.
    fn nested_title_colors() -> (Color, Color) {
        // Lite tier.
        let lite = Ui::with_css(LITE_CSS);
        let a = build_app(&lite);
        let mut dom = Dom::new();
        dom.apply(&lite.take_batch(0)).unwrap();
        let lite_color = lite_style(&dom, a.nested_title).color;

        // Capable tier (Stylo over the real Dom).
        let cap = Ui::capable(CAPABLE_CSS);
        let a2 = build_app(&cap);
        let mut cdom = Dom::new();
        cdom.apply(&cap.take_batch(0)).unwrap();
        let mut engine = StyloEngine::from_dom(&cdom, cap.css_source());
        let cap_color = engine.resolve(a2.nested_title, None).unwrap().color;

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
