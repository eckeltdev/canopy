//! Canopy layout via the **real** [Taffy] flexbox engine.
//!
//! This crate is a drop-in alternative to [`canopy_paint::layout`]: same
//! signature, same output types ([`DisplayList`] + [`LayoutResult`]). Where
//! `canopy-paint` hand-rolls a single-pass flex, this crate maps each element's
//! inline styles onto a Taffy [`Style`], builds a [`TaffyTree`] mirroring the
//! [`Dom`] subtree under every [`ROOT`] child, runs `compute_layout` against the
//! viewport, and converts Taffy's per-node *relative* boxes into the **absolute**
//! [`Rect`]s Canopy expects.
//!
//! The display list is built by Canopy, not Taffy: backgrounds paint behind their
//! children (one [`DisplayItem::Rect`] per element with a [`canopy_paint::BG`]
//! color) and text leaves paint as [`DisplayItem::Text`] runs sized to the node's
//! requested pixel height. Geometry — and only geometry — comes from Taffy.
//!
//! A single tree walk ([`collect_rects`]) also resolves the **cascade's inherited
//! properties** — `color` ([`FG`]) and `text-align` ([`TEXT_ALIGN`]) flow from a node to
//! its descendants unless the descendant sets its own — alongside the compounding paint
//! accumulators (`translate`, `opacity`). `background` ([`BG`]) is deliberately *not*
//! inherited. This is a minimal, no-specificity inheritance pass (the constrained-tier
//! answer); the `StyleEngine` trait reserves a full Stylo cascade for capable tiers.
//!
//! Style mapping (all inline, reusing the `canopy-paint` [`PropId`] consts):
//! - [`DIRECTION`] `"row"`/`"column"` -> [`FlexDirection::Row`]/`Column` (default `Column`).
//! - [`GAP`] -> Taffy `gap` on both axes (length px).
//! - [`PADDING`] -> uniform Taffy `padding` (length px) on all four sides.
//! - [`WIDTH`]/[`HEIGHT`] -> `size` of [`Dimension::length`] when set, else `auto`.
//!
//! Text leaves get a Taffy size from the requested pixel height: `height` is [`HEIGHT`]
//! (or `16`) exactly, and `width` is [`WIDTH`] when set, else `chars * (8 * scale)` — the
//! baked renderer's exact glyph advance (an 8px cell at integer `scale = floor(height/8)`),
//! so the layout box is as wide as the drawn glyphs and the renderer's clip never truncates.
//!
//! [Taffy]: https://docs.rs/taffy
//! Stays `no_std` + `alloc`: Taffy is pulled with `default-features = false` and
//! only `flexbox`/`alloc`/`taffy_tree`, and every pixel value is parsed as an
//! integer (no `f32::floor`/`parse` from `std`).
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::ToString;
use alloc::vec::Vec;

use canopy_dom::{Dom, ROOT};
use canopy_paint::{
    ALIGN, BG, DIRECTION, FG, GAP, HEIGHT, JUSTIFY, OPACITY, PADDING, RADIUS, TEXT_ALIGN,
    TRANSLATE_X, TRANSLATE_Y, WIDTH,
};
use canopy_protocol::{NodeId, PropId};
use canopy_traits::{Color, DisplayItem, DisplayList, LayoutResult, Point, Rect, Size};

use taffy::prelude::length;
use taffy::{
    AlignItems, AvailableSpace, Dimension, FlexDirection, JustifyContent, LengthPercentage,
    Rect as TaffyRect, Size as TaffySize, Style, TaffyTree,
};

/// Default text size (px) when no [`HEIGHT`] is set.
const TEXT_HEIGHT: u32 = 16;
/// The baked font's square cell size in px (`canopy_text_baked::CELL_W == CELL_H == 8`). A
/// text run's width estimate steps by this so the layout box matches the renderer's advance.
const BAKED_CELL_PX: f32 = 8.0;
/// Default foreground ink (light gray) when no [`FG`] is set.
const DEFAULT_FG: Color = Color {
    r: 0xe6,
    g: 0xe6,
    b: 0xe6,
    a: 255,
};

fn parse_color(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#')?;
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

/// Raw integer-pixel value of `prop` on `node`, if set and parseable. Pixel styles
/// are whole-number strings, so integer parsing keeps this crate honestly `no_std`
/// (float parsing was historically `std`-only).
fn style_px(dom: &Dom, node: NodeId, prop: PropId) -> Option<u32> {
    dom.style(node, prop).and_then(|s| s.parse::<u32>().ok())
}

fn style_color(dom: &Dom, node: NodeId, prop: PropId) -> Option<Color> {
    dom.style(node, prop).and_then(parse_color)
}

/// The node's corner [`RADIUS`] in logical px (default `0.0` = square). Geometry
/// comes from Taffy, but the corner radius is a *paint* property, so it is read
/// straight off the Dom here and threaded onto the emitted background rect.
fn style_radius(dom: &Dom, node: NodeId) -> f32 {
    style_px(dom, node, RADIUS).unwrap_or(0) as f32
}

/// A node's OWN [`TEXT_ALIGN`] as a `0.0`/`0.5`/`1.0` fraction (`"center"` => `0.5`,
/// `"right"` => `1.0`, anything else => `0.0` = left/start), or `None` if it sets none.
/// The fraction ultimately rides onto the emitted [`DisplayItem::Text`]'s `align`, where
/// the renderer applies it against its own measured run width — so a centered text node
/// renders its glyphs centered within the box Taffy laid out for it.
///
/// `text-align` is a CSS **inherited** property; the inheritance itself happens once in
/// the [`collect_rects`] tree walk (a node with no value takes its parent's resolved
/// one), not as a per-read ancestor walk — so this only reports the node's local value.
fn style_text_align_own(dom: &Dom, node: NodeId) -> Option<f32> {
    dom.style(node, TEXT_ALIGN).map(|v| match v {
        "center" => 0.5,
        "right" => 1.0,
        _ => 0.0,
    })
}

/// The inherited (cascaded-down) style a node passes to its children: the CSS
/// **inherited** properties (`color`/text-align here) plus the paint accumulators
/// (translate offset, effective opacity) that also compound down the subtree.
#[derive(Clone, Copy)]
struct Inherited {
    /// Accumulated translate offset (paint + hit-test).
    translate: Point,
    /// Effective opacity (multiplied down).
    opacity: f32,
    /// Resolved text color (`FG`); inherits when a node sets none.
    fg: Color,
    /// Resolved text alignment fraction; inherits when a node sets none.
    align: f32,
}

/// The per-node resolved paint values [`collect_rects`] records alongside each rect,
/// for [`build_display_list`] to read by index (parallel to the `rects` vec).
#[derive(Clone, Copy)]
struct NodePaint {
    /// Effective opacity scaling every primitive this node emits.
    opacity: f32,
    /// Resolved (inherited) text color.
    fg: Color,
    /// Resolved (inherited) text-align fraction.
    align: f32,
}

/// Read a sizing property (`prop` is [`WIDTH`] or [`HEIGHT`]) into a Taffy
/// [`Dimension`], accepting three forms:
///
/// - a **percentage** — `"100%"`, `"50%"` -> [`Dimension::percent`] of the fraction
///   (`"50%"` -> `percent(0.5)`), resolved by Taffy against the available space;
/// - a **length** — `"Npx"` or bare `"N"` -> [`Dimension::length`] of `N` px;
/// - **absent / unparseable** -> [`Dimension::auto`] (content-sized), the default.
///
/// Percentages parse the integer part before `%` (the box-model sizes authors use
/// are whole percents), and lengths reuse the integer-px reader, so no `std`-only
/// float parsing creeps into this `no_std` crate.
fn style_dimension(dom: &Dom, node: NodeId, prop: PropId) -> Dimension {
    if let Some(s) = dom.style(node, prop) {
        if let Some(pct) = s.strip_suffix('%') {
            if let Ok(n) = pct.trim().parse::<u32>() {
                // "50%" -> 0.5; integer-only division keeps this `no_std`-clean.
                return Dimension::percent(n as f32 / 100.0);
            }
        }
        // Tolerate a trailing "px" (the CSS path strips it, but be robust).
        let num = s.strip_suffix("px").map(str::trim).unwrap_or(s);
        if let Ok(n) = num.parse::<u32>() {
            return Dimension::length(n as f32);
        }
    }
    Dimension::auto()
}

/// Signed, fractional float value of `prop` on `node`, defaulting to `default` when
/// unset or unparseable.
///
/// Unlike [`style_px`], this accepts a leading `-` and a decimal point, which the
/// translate offsets need (`-24`, `12.5`). Float string parsing is available in
/// `core` on the targets Canopy builds for (verified against `thumbv7em-none-eabi`),
/// so reading these here keeps the crate honestly `no_std`.
fn style_f32(dom: &Dom, node: NodeId, prop: PropId, default: f32) -> f32 {
    dom.style(node, prop)
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(default)
}

/// The node's own [`TRANSLATE_X`]/[`TRANSLATE_Y`] paint offset in logical px
/// (default `(0, 0)`). This is the node's *local* shift; the tree walk accumulates
/// it onto the parent's offset so the translate applies to the whole subtree, like a
/// CSS `transform: translate`.
fn style_translate(dom: &Dom, node: NodeId) -> Point {
    Point {
        x: style_f32(dom, node, TRANSLATE_X, 0.0),
        y: style_f32(dom, node, TRANSLATE_Y, 0.0),
    }
}

/// The node's own [`OPACITY`] in `[0, 1]` (default `1.0`), clamped so a stray
/// out-of-range authoring value can't push an alpha past the byte it scales. The
/// tree walk multiplies it into the parent's effective opacity, so opacity composes
/// down the subtree.
fn style_opacity(dom: &Dom, node: NodeId) -> f32 {
    // `.clamp(0.0, 1.0)` is `core`-safe (just compares/selects, no transcendentals),
    // matching the no_std float discipline the rest of the crate keeps.
    style_f32(dom, node, OPACITY, 1.0).clamp(0.0, 1.0)
}

/// Scale `alpha` (a straight-alpha byte) by `opacity` in `[0, 1]`, rounding to the
/// nearest byte. `opacity == 1.0` returns `alpha` unchanged (the common, fully-
/// opaque case); a fractional opacity fades the channel toward transparent.
///
/// Integer-friendly round-to-nearest via `+ 0.5` before the truncating `as u8`,
/// which is `core`-safe — no `f32::round` (a `std`-only intrinsic). The product is
/// in `[0, 255]` because `alpha <= 255` and `opacity <= 1.0`, so the cast never
/// saturates unexpectedly.
fn scale_alpha(alpha: u8, opacity: f32) -> u8 {
    if opacity >= 1.0 {
        return alpha;
    }
    (alpha as f32 * opacity + 0.5) as u8
}

/// `color` with its alpha scaled by `opacity` (see [`scale_alpha`]); RGB is left
/// untouched (straight-alpha compositing fades via the alpha channel alone).
fn fade(color: Color, opacity: f32) -> Color {
    Color {
        r: color.r,
        g: color.g,
        b: color.b,
        a: scale_alpha(color.a, opacity),
    }
}

/// The container's cross-axis alignment ([`ALIGN`] / CSS `align-items`), or `None`
/// (Taffy's default = stretch/start) when unset or unrecognized.
fn style_align(dom: &Dom, id: NodeId) -> Option<AlignItems> {
    // taffy 0.11 models alignment as a struct with associated-const keywords.
    match dom.style(id, ALIGN)? {
        "start" | "flex-start" => Some(AlignItems::FLEX_START),
        "center" => Some(AlignItems::CENTER),
        "end" | "flex-end" => Some(AlignItems::FLEX_END),
        "stretch" => Some(AlignItems::STRETCH),
        _ => None,
    }
}

/// The container's main-axis distribution ([`JUSTIFY`] / CSS `justify-content`), or
/// `None` (Taffy's default = start) when unset or unrecognized.
fn style_justify(dom: &Dom, id: NodeId) -> Option<JustifyContent> {
    // `JustifyContent` is taffy's `AlignContent`; same associated-const keyword style.
    match dom.style(id, JUSTIFY)? {
        "start" | "flex-start" => Some(JustifyContent::FLEX_START),
        "center" => Some(JustifyContent::CENTER),
        "end" | "flex-end" => Some(JustifyContent::FLEX_END),
        "space-between" => Some(JustifyContent::SPACE_BETWEEN),
        "space-around" => Some(JustifyContent::SPACE_AROUND),
        "space-evenly" => Some(JustifyContent::SPACE_EVENLY),
        _ => None,
    }
}

/// Layout size of a text leaf: its **height is the requested font size in px**
/// (`height` style, default [`TEXT_HEIGHT`]) — NOT snapped to the 8px baked cell, so a
/// `height: 15` run renders at a real 15px on the capable (parley) tier instead of
/// collapsing to 8px. The capable renderer rasterizes at exactly this size; the
/// constrained baked renderer floors it to its nearest 8px scale internally.
///
/// Width matches the constrained baked renderer's glyph advance EXACTLY, so a text run's
/// layout box is as wide as the glyphs drawn into it. `canopy-render-soft` now clips text
/// to this box, so an under-estimate (the old `0.6 em` proportional guess) would truncate
/// the run; the baked font is a fixed `CELL_W = CELL_H = 8` cell drawn at integer
/// `scale = max(1, floor(h / 8))`, advancing `8 * scale` px per glyph — mirror that here.
/// `text-align` still absorbs any slack within the box.
fn text_size(dom: &Dom, id: NodeId, text: &str) -> Size {
    let h = style_px(dom, id, HEIGHT).unwrap_or(TEXT_HEIGHT) as f32;
    // The baked-font cell (canopy_text_baked CELL_W == CELL_H == 8) and the renderer's
    // integer scale; keep these in lockstep with canopy_render_soft::blit_text's advance.
    // Cast-truncation (not f32::floor, which needs std/libm) mirrors the renderer exactly.
    let scale = ((h / BAKED_CELL_PX) as u32).max(1) as f32;
    let advance = BAKED_CELL_PX * scale;
    let w = style_px(dom, id, WIDTH)
        .map(|w| w as f32)
        .unwrap_or(text.chars().count() as f32 * advance);
    Size { w, h }
}

/// Whether `rect` contains `point` (top/left inclusive, bottom/right exclusive).
fn rect_contains(rect: &Rect, point: Point) -> bool {
    point.x >= rect.origin.x
        && point.y >= rect.origin.y
        && point.x < rect.origin.x + rect.size.w
        && point.y < rect.origin.y + rect.size.h
}

/// Resolve a **root** node's sizing property (`prop` is [`WIDTH`] or [`HEIGHT`])
/// into a definite pixel size against the viewport `extent` (the viewport's width
/// for [`WIDTH`], its height for [`HEIGHT`]), or `None` to leave the axis as Taffy
/// laid it out (content/auto).
///
/// Taffy does not resolve a *root* node's percentage against the available space
/// passed to `compute_layout`, so a `width: 100%` root would otherwise collapse to
/// its content. We resolve it ourselves here:
///
/// - a **percentage** — `"100%"` -> `extent`, `"50%"` -> `extent * 0.5`;
/// - a **length** — `"Npx"`/`"N"` -> `N` px (already definite, but we pin it so a
///   root length is honored exactly even at the tree root);
/// - **absent / auto** -> `None`, so a top-level node with no explicit size keeps
///   its content height and the existing top-level *stacking* down the viewport is
///   preserved (forcing auto to fill would make every top-level sibling
///   viewport-tall).
fn resolve_root_dimension(dom: &Dom, node: NodeId, prop: PropId, extent: f32) -> Option<f32> {
    let s = dom.style(node, prop)?;
    if let Some(pct) = s.strip_suffix('%') {
        if let Ok(n) = pct.trim().parse::<u32>() {
            // Integer-percent of the viewport extent; `n as f32 / 100.0` is
            // `core`-safe (no `std`-only float intrinsics).
            return Some(extent * (n as f32 / 100.0));
        }
        return None;
    }
    let num = s.strip_suffix("px").map(str::trim).unwrap_or(s);
    num.parse::<u32>().ok().map(|n| n as f32)
}

/// Build the Taffy [`Style`] for one element from its inline styles.
fn element_style(dom: &Dom, id: NodeId) -> Style {
    let dir = match dom.style(id, DIRECTION) {
        Some("row") => FlexDirection::Row,
        _ => FlexDirection::Column,
    };
    let gap = style_px(dom, id, GAP).unwrap_or(0) as f32;
    let pad = style_px(dom, id, PADDING).unwrap_or(0) as f32;
    // Width/height accept a percentage ("100%"/"50%"), a length ("Npx"/"N"), or are
    // auto (content-sized) when absent — see `style_dimension`.
    let width = style_dimension(dom, id, WIDTH);
    let height = style_dimension(dom, id, HEIGHT);
    Style {
        flex_direction: dir,
        align_items: style_align(dom, id),
        justify_content: style_justify(dom, id),
        gap: TaffySize {
            width: length(gap),
            height: length(gap),
        },
        padding: TaffyRect {
            left: LengthPercentage::length(pad),
            right: LengthPercentage::length(pad),
            top: LengthPercentage::length(pad),
            bottom: LengthPercentage::length(pad),
        },
        size: TaffySize { width, height },
        ..Default::default()
    }
}

/// Hard cap on tree depth the layout builder will descend. `canopy_dom` rejects cycles at
/// the mutation boundary (the primary defense — see its `InsertBefore` cycle check), so a
/// real tree never approaches this; this is a defense-in-depth backstop that keeps a
/// pathological (or future-buggy, e.g. a cycle that ever slipped the Dom) tree from
/// overflowing the stack. Past the cap a node becomes a childless leaf rather than being
/// recursed into. The cap is applied in `build_node`, but because the *built* Taffy tree is
/// then walked to its full depth by `compute_layout` and [`collect_rects`], the value also
/// bounds those recursions — so it is kept well under the depth at which the whole pipeline
/// overflows a normal stack (empirically ~512), with generous headroom over any real UI.
pub const MAX_TREE_DEPTH: usize = 256;

/// Recursively build a Taffy node mirroring `id`, returning its Taffy key. Text
/// leaves become fixed-size leaves; elements recurse over their children. `depth` is the
/// distance from the root; descent stops at [`MAX_TREE_DEPTH`] (see its docs).
fn build_node(dom: &Dom, id: NodeId, tree: &mut TaffyTree<NodeId>, depth: usize) -> taffy::NodeId {
    if let Some(text) = dom.node(id).and_then(|n| n.text.as_deref()) {
        let size = text_size(dom, id, text);
        let style = Style {
            size: TaffySize {
                width: Dimension::length(size.w),
                height: Dimension::length(size.h),
            },
            ..Default::default()
        };
        // `new_leaf_with_context` stashes the Canopy NodeId so the post-layout walk
        // can map Taffy keys back to Dom nodes without a side table.
        return tree.new_leaf_with_context(style, id).unwrap();
    }

    let children: Vec<taffy::NodeId> = if depth >= MAX_TREE_DEPTH {
        Vec::new() // backstop: cannot descend past the cap (see MAX_TREE_DEPTH)
    } else {
        dom.children(id)
            .iter()
            .map(|&c| build_node(dom, c, tree, depth + 1))
            .collect()
    };
    let key = tree
        .new_leaf_with_context(element_style(dom, id), id)
        .unwrap();
    if !children.is_empty() {
        tree.set_children(key, &children).unwrap();
    }
    key
}

/// Walk the computed Taffy tree, accumulating parent offsets into absolute rects
/// and recording `(NodeId, Rect)` in back-to-front tree order (parents before
/// children), alongside a parallel `paints` vec (one [`NodePaint`] per pushed rect,
/// same index) holding each node's resolved opacity, color, and text-align.
///
/// The [`Inherited`] state threads down the subtree, mirroring the CSS cascade. Two of
/// its fields **compound** (paint accumulators) and two **inherit** (cascaded properties):
///
/// - **`translate`** *(compounds)* — the running paint offset. A node's own
///   [`style_translate`] is *added* to it; the sum shifts the node's absolute rect
///   **and** is passed to its children, so the whole subtree slides together with no
///   reflow. Because the shifted rect is what we record, both the display list (which
///   reads these rects) and [`hit_test`] (which scans them) see the node where it is
///   drawn — a translated node is hit at its painted position.
/// - **`opacity`** *(compounds)* — the running effective opacity. A node's own
///   [`style_opacity`] *multiplies* it; the product is stored for this rect and
///   passed down, so setting opacity on a container fades its whole subtree. Opacity
///   is paint-only: it never touches the rect geometry, so hit-testing ignores it
///   (a faded node is still clickable).
/// - **`fg` (`color`)** and **`align` (`text-align`)** *(inherit)* — a node's own value
///   wins, else it takes the parent's resolved value. This is the cascade's inheritance
///   step, performed once here so [`build_display_list`] can read the resolved value by
///   index instead of walking ancestors per text node. `background` is intentionally NOT
///   inherited (it stays a per-element read in `build_display_list`).
fn collect_rects(
    dom: &Dom,
    tree: &TaffyTree<NodeId>,
    key: taffy::NodeId,
    parent_origin: Point,
    inherited: Inherited,
    rects: &mut Vec<(NodeId, Rect)>,
    paints: &mut Vec<NodePaint>,
) {
    let layout = tree.layout(key).unwrap();
    // Taffy's relative box, made absolute by the parent's accumulated origin.
    let origin = Point {
        x: parent_origin.x + layout.location.x,
        y: parent_origin.y + layout.location.y,
    };

    // Fold this node's own values into the inherited state. The context maps the Taffy
    // key back to a Dom node; a key with no context (none in practice) contributes no
    // local style and just forwards the parent's values.
    //
    // - translate/opacity COMPOUND (a CSS transform/opacity accumulates down a subtree);
    // - fg (`color`) and align (`text-align`) INHERIT (a node's own value wins, else the
    //   parent's resolved value is taken). This is the cascade's inheritance step, done
    //   once here instead of as a per-read ancestor walk in `build_display_list`.
    let id = tree.get_node_context(key).copied();
    let local_translate = id
        .map(|id| style_translate(dom, id))
        .unwrap_or(Point { x: 0.0, y: 0.0 });
    let translate = Point {
        x: inherited.translate.x + local_translate.x,
        y: inherited.translate.y + local_translate.y,
    };
    let opacity = inherited.opacity * id.map(|id| style_opacity(dom, id)).unwrap_or(1.0);
    let fg = id
        .and_then(|id| style_color(dom, id, FG))
        .unwrap_or(inherited.fg);
    let align = id
        .and_then(|id| style_text_align_own(dom, id))
        .unwrap_or(inherited.align);

    // The recorded rect is the translated one, so paint and hit-test agree.
    let rect = Rect {
        origin: Point {
            x: origin.x + translate.x,
            y: origin.y + translate.y,
        },
        size: Size {
            w: layout.size.width,
            h: layout.size.height,
        },
    };
    if let Some(id) = id {
        rects.push((id, rect));
        paints.push(NodePaint { opacity, fg, align });
    }
    // Children inherit the *untranslated* absolute origin (Taffy locations are relative
    // to it) plus the accumulated/inherited style.
    let child_inherited = Inherited {
        translate,
        opacity,
        fg,
        align,
    };
    for child in tree.children(key).unwrap() {
        collect_rects(dom, tree, child, origin, child_inherited, rects, paints);
    }
}

/// Lay the whole tree out within `viewport` using Taffy, producing both the
/// back-to-front [`DisplayList`] and a [`LayoutResult`] with an **absolute**
/// [`Rect`] for every node (elements *and* text), in tree order.
///
/// Same signature and output contract as [`canopy_paint::layout`]: each top-level
/// node is laid out against the viewport as available space and stacked down the
/// y axis. Geometry comes from Taffy; the display list (backgrounds behind
/// children, baked-font text runs) is built here from the Dom + the absolute rects.
pub fn layout(dom: &Dom, viewport: Size) -> (DisplayList, LayoutResult) {
    let mut rects: Vec<(NodeId, Rect)> = Vec::new();
    // Resolved paint per rect, same index as `rects` (paint-only; not part of the
    // returned `LayoutResult`, which hit-tests on geometry alone). Holds the inherited
    // `color`/`text-align` and the accumulated opacity for each node.
    let mut paints: Vec<NodePaint> = Vec::new();
    let mut y = 0.0_f32;
    for &root in dom.children(ROOT) {
        let mut tree: TaffyTree<NodeId> = TaffyTree::new();
        let key = build_node(dom, root, &mut tree, 0);

        // Taffy does not resolve a *root* node's percentage against the available
        // space, so a `width: 100%` / `height: 100%` root would collapse to its
        // content. Resolve the root's own width/height against the viewport here and
        // pin it as a definite length, so a root sized `100%` fills the window (and a
        // `50%` root takes half). Auto/absent axes are left untouched, preserving the
        // existing "top-level nodes stack down the viewport at content height".
        if let Ok(style) = tree.style(key) {
            let mut style = style.clone();
            let mut changed = false;
            if let Some(w) = resolve_root_dimension(dom, root, WIDTH, viewport.w) {
                style.size.width = Dimension::length(w);
                changed = true;
            }
            if let Some(h) = resolve_root_dimension(dom, root, HEIGHT, viewport.h) {
                style.size.height = Dimension::length(h);
                changed = true;
            }
            if changed {
                tree.set_style(key, style).unwrap();
            }
        }

        tree.compute_layout(
            key,
            TaffySize {
                width: AvailableSpace::Definite(viewport.w),
                height: AvailableSpace::Definite(viewport.h),
            },
        )
        .unwrap();
        // Each top-level subtree starts with no inherited translate, full opacity, the
        // default foreground, and left text-align — the cascade's initial values.
        collect_rects(
            dom,
            &tree,
            key,
            Point { x: 0.0, y },
            Inherited {
                translate: Point { x: 0.0, y: 0.0 },
                opacity: 1.0,
                fg: DEFAULT_FG,
                align: 0.0,
            },
            &mut rects,
            &mut paints,
        );
        // Stack top-level siblings down the viewport, mirroring `canopy-paint`.
        let used_h = tree.layout(key).unwrap().size.height;
        y += used_h;
    }

    let items = build_display_list(dom, &rects, &paints);
    (DisplayList { items }, LayoutResult { rects })
}

/// Build a display list for the whole tree within `viewport`.
///
/// A thin wrapper over [`layout`] that discards the [`LayoutResult`]; kept at this
/// exact signature so renderer hosts continue to compile unchanged.
pub fn build_scene(dom: &Dom, viewport: Size) -> DisplayList {
    layout(dom, viewport).0
}

/// Build the [`DisplayList`] from the Dom, the absolute rects, and the per-rect
/// resolved paint ([`NodePaint`]; `paints[i]` belongs to `rects[i]`).
///
/// `rects` is in back-to-front tree order (parents before children), so iterating
/// it forward naturally paints each element's background *behind* its descendants.
/// Each element with a [`BG`] color emits a filled [`DisplayItem::Rect`] (background is
/// read off the node — it does not inherit); each text node emits a [`DisplayItem::Text`]
/// run colored by its already-resolved (inherited) [`FG`], aligned by its resolved
/// `text-align`, with a cell height equal to its rect height.
///
/// Every emitted color is [`fade`]d by that node's effective opacity, scaling the
/// fill's / ink's alpha so a reduced-opacity subtree paints translucent and blends
/// over whatever sits behind it. At full opacity (the overwhelmingly common case)
/// [`scale_alpha`] returns the byte unchanged, so opaque scenes are byte-for-byte
/// what they were before.
fn build_display_list(
    dom: &Dom,
    rects: &[(NodeId, Rect)],
    paints: &[NodePaint],
) -> Vec<DisplayItem> {
    let mut items = Vec::new();
    for (i, &(id, rect)) in rects.iter().enumerate() {
        let Some(node) = dom.node(id) else { continue };
        // Parallel vecs are built together in `collect_rects`, so the index is always
        // valid; default to opaque/inherited-light if a caller ever passes a short slice.
        let paint = paints.get(i).copied().unwrap_or(NodePaint {
            opacity: 1.0,
            fg: DEFAULT_FG,
            align: 0.0,
        });
        if let Some(text) = node.text.as_deref() {
            // `background` is NOT inherited (per-element), so it is read off this node;
            // the text `color` IS inherited (resolved in the tree walk -> `paint.fg`).
            if let Some(bg) = style_color(dom, id, BG) {
                items.push(DisplayItem::Rect {
                    rect,
                    color: fade(bg, paint.opacity),
                    radius: style_radius(dom, id),
                });
            }
            items.push(DisplayItem::Text {
                origin: rect.origin,
                text: text.to_string(),
                color: fade(paint.fg, paint.opacity),
                size: rect.size.h,
                // Align the glyphs within the node's laid-out box width using the
                // resolved (inherited) `text-align`; the renderer offsets by
                // `(box_w - run_w) * align` against its own measured run width, so a
                // centered text node (in a box centered by `align-items: center`)
                // renders centered ink.
                box_w: rect.size.w,
                align: paint.align,
            });
        } else if let Some(bg) = style_color(dom, id, BG) {
            items.push(DisplayItem::Rect {
                rect,
                color: fade(bg, paint.opacity),
                radius: style_radius(dom, id),
            });
        }
    }
    items
}

/// Return the topmost node whose absolute rect contains `point`, or `None`.
///
/// `layout.rects` is in back-to-front tree order (parents before children, earlier
/// siblings before later), so scanning from the end yields the visually topmost hit.
pub fn hit_test(layout: &LayoutResult, point: Point) -> Option<NodeId> {
    layout
        .rects
        .iter()
        .rev()
        .find(|(_, rect)| rect_contains(rect, point))
        .map(|(id, _)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_core::Emitter;
    use canopy_dom::Dom;
    use canopy_protocol::ElementTag;
    use canopy_traits::OpSink;

    fn dom_from(e: Emitter) -> Dom {
        let mut e = e;
        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();
        dom
    }

    #[test]
    fn deep_tree_is_depth_capped_not_stack_overflowed() {
        // Defense-in-depth: `canopy_dom` rejects cycles, but even a pathologically DEEP acyclic
        // tree (or a cycle that ever slipped the Dom's guard) must not recurse without bound.
        // Build a chain deeper than MAX_TREE_DEPTH and assert layout TERMINATES and truncates at
        // the cap (far fewer rects than the input depth) rather than overflowing the stack.
        let depth = MAX_TREE_DEPTH * 2;
        let mut e = Emitter::new();
        let mut prev = e.create_element(ElementTag::new(1));
        e.append(ROOT, prev);
        for _ in 1..depth {
            let next = e.create_element(ElementTag::new(1));
            e.append(prev, next);
            prev = next;
        }
        let dom = dom_from(e);

        let (_scene, lay) = layout(&dom, Size { w: 100.0, h: 100.0 });
        assert!(
            lay.rects.len() <= MAX_TREE_DEPTH + 1,
            "descent stops at the cap: {} rects for a {}-deep chain",
            lay.rects.len(),
            depth
        );
        assert!(
            lay.rects.len() < depth,
            "the deep tail was truncated, not laid out"
        );
    }

    #[test]
    fn align_items_center_centers_a_child_on_the_cross_axis() {
        // A 200-wide column with `align-items: center` and a 40-wide child: the child's
        // x should be (200 - 40) / 2 = 80, not 0 (the default cross-start).
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "200");
        e.set_inline_style(col, HEIGHT, "100");
        e.set_inline_style(col, ALIGN, "center");
        let child = e.create_element(ElementTag::new(2));
        e.set_inline_style(child, WIDTH, "40");
        e.set_inline_style(child, HEIGHT, "20");
        e.append(col, child);
        let dom = dom_from(e);

        let (_scene, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let child_rect = lay.rects.iter().find(|(id, _)| *id == child).unwrap().1;
        assert_eq!(
            child_rect.origin.x, 80.0,
            "child centered on the cross axis"
        );
    }

    #[test]
    fn percent_root_fills_the_viewport_and_percent_child_is_resolved() {
        // THE fill-the-viewport proof: a root sized `100% x 100%` must fill the whole
        // 800x600 viewport (Taffy alone would collapse a root percentage to content),
        // and a `50%`-wide child resolves against the filled root -> 400 wide.
        let mut e = Emitter::new();
        let root = e.create_element(ElementTag::new(1));
        e.append(ROOT, root);
        e.set_inline_style(root, WIDTH, "100%");
        e.set_inline_style(root, HEIGHT, "100%");
        let child = e.create_element(ElementTag::new(2));
        e.append(root, child);
        e.set_inline_style(child, WIDTH, "50%");
        e.set_inline_style(child, HEIGHT, "100%");
        let dom = dom_from(e);

        let (_scene, lay) = layout(&dom, Size { w: 800.0, h: 600.0 });
        let root_rect = lay.rects.iter().find(|(id, _)| *id == root).unwrap().1;
        assert_eq!(
            root_rect.size,
            Size { w: 800.0, h: 600.0 },
            "100% x 100% root fills the viewport"
        );
        let child_rect = lay.rects.iter().find(|(id, _)| *id == child).unwrap().1;
        assert_eq!(
            child_rect.size.w, 400.0,
            "50% child is half the filled root"
        );
        assert_eq!(
            child_rect.size.h, 600.0,
            "100% child matches the root height"
        );
    }

    #[test]
    fn half_percent_root_takes_half_the_viewport() {
        // A `50% x 50%` root resolves against the viewport to a 400x300 box.
        let mut e = Emitter::new();
        let root = e.create_element(ElementTag::new(1));
        e.append(ROOT, root);
        e.set_inline_style(root, WIDTH, "50%");
        e.set_inline_style(root, HEIGHT, "50%");
        let dom = dom_from(e);

        let (_scene, lay) = layout(&dom, Size { w: 800.0, h: 600.0 });
        let root_rect = lay.rects.iter().find(|(id, _)| *id == root).unwrap().1;
        assert_eq!(root_rect.size, Size { w: 400.0, h: 300.0 });
    }

    #[test]
    fn auto_root_still_content_sizes_and_stacks() {
        // Resolving percentages must NOT change the auto path: a root with no explicit
        // size still content-sizes (here, to its 30x20 child), preserving the existing
        // top-level behavior rather than ballooning to the viewport.
        let mut e = Emitter::new();
        let root = e.create_element(ElementTag::new(1));
        e.append(ROOT, root);
        let child = e.create_element(ElementTag::new(2));
        e.append(root, child);
        e.set_inline_style(child, WIDTH, "30");
        e.set_inline_style(child, HEIGHT, "20");
        let dom = dom_from(e);

        let (_scene, lay) = layout(&dom, Size { w: 800.0, h: 600.0 });
        let root_rect = lay.rects.iter().find(|(id, _)| *id == root).unwrap().1;
        assert_eq!(
            root_rect.size,
            Size { w: 30.0, h: 20.0 },
            "auto root content-sizes to its child, not the viewport"
        );
    }

    #[test]
    fn text_align_center_rides_onto_the_display_item() {
        // A text node with `text-align: center` emits a Text run whose `align` is 0.5
        // and whose `box_w` is the node's laid-out box width — the renderer does the
        // actual centering against its own measured run width.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "200");
        let t = e.create_text("ab"); // 2 chars
        e.append(col, t);
        e.set_inline_style(t, WIDTH, "160");
        e.set_inline_style(t, HEIGHT, "16");
        e.set_inline_style(t, TEXT_ALIGN, "center");
        let dom = dom_from(e);

        let (scene, _lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let (box_w, align) = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Text { box_w, align, .. } => Some((*box_w, *align)),
                _ => None,
            })
            .expect("text run");
        assert_eq!(align, 0.5, "text-align: center -> 0.5");
        assert_eq!(box_w, 160.0, "box_w is the text node's laid-out width");
    }

    #[test]
    fn no_text_align_is_left_on_the_display_item() {
        // The default (no text-align) emits align 0.0 — legacy left-aligned.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let t = e.create_text("ab");
        e.append(col, t);
        e.set_inline_style(t, HEIGHT, "16");
        let dom = dom_from(e);

        let (scene, _lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let align = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Text { align, .. } => Some(*align),
                _ => None,
            })
            .expect("text run");
        assert_eq!(align, 0.0, "no text-align -> left (0.0)");
    }

    #[test]
    fn color_inherits_from_an_ancestor() {
        // `color` is a CSS *inherited* property: a text node with no `color` of its own
        // takes its ancestor's. Here the column sets a yellow `color` and the nested
        // text leaf sets none — the emitted Text run must be yellow, not the default.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, FG, "#ffd040");
        // An intermediate wrapper with no color, to prove inheritance crosses depth.
        let wrap = e.create_element(ElementTag::new(1));
        e.append(col, wrap);
        let t = e.create_text("hi");
        e.append(wrap, t);
        e.set_inline_style(t, HEIGHT, "16");
        let dom = dom_from(e);

        let (scene, _lay) = layout(&dom, Size { w: 100.0, h: 50.0 });
        let color = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Text { color, .. } => Some(*color),
                _ => None,
            })
            .expect("text run");
        assert_eq!(
            color,
            Color {
                r: 0xff,
                g: 0xd0,
                b: 0x40,
                a: 255
            },
            "text with no color of its own inherits the ancestor's yellow"
        );
    }

    #[test]
    fn own_color_wins_over_inherited() {
        // A node's OWN `color` overrides the inherited one. The column is yellow; the
        // text node sets its own blue — the run must be blue.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, FG, "#ffd040");
        let t = e.create_text("hi");
        e.append(col, t);
        e.set_inline_style(t, HEIGHT, "16");
        e.set_inline_style(t, FG, "#89b4fa");
        let dom = dom_from(e);

        let (scene, _lay) = layout(&dom, Size { w: 100.0, h: 50.0 });
        let color = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Text { color, .. } => Some(*color),
                _ => None,
            })
            .expect("text run");
        assert_eq!(
            color,
            Color {
                r: 0x89,
                g: 0xb4,
                b: 0xfa,
                a: 255
            },
            "the node's own color overrides the inherited one"
        );
    }

    #[test]
    fn text_align_inherits_from_an_ancestor() {
        // `text-align` is inherited too: a centered container makes a descendant text
        // node with no `text-align` of its own emit align 0.5.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "200");
        e.set_inline_style(col, TEXT_ALIGN, "center");
        let t = e.create_text("ab");
        e.append(col, t);
        e.set_inline_style(t, WIDTH, "160");
        e.set_inline_style(t, HEIGHT, "16");
        let dom = dom_from(e);

        let (scene, _lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let align = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Text { align, .. } => Some(*align),
                _ => None,
            })
            .expect("text run");
        assert_eq!(
            align, 0.5,
            "text-align: center inherits onto the descendant"
        );
    }

    #[test]
    fn background_does_not_inherit() {
        // `background` is a per-element property: setting it on a parent must NOT paint
        // a background behind a child that sets none. The parent emits exactly one Rect
        // (its own); the unstyled child contributes no fill.
        let mut e = Emitter::new();
        let parent = e.create_element(ElementTag::new(1));
        e.append(ROOT, parent);
        e.set_inline_style(parent, WIDTH, "100");
        e.set_inline_style(parent, HEIGHT, "100");
        e.set_inline_style(parent, BG, "#202020");
        let child = e.create_element(ElementTag::new(2));
        e.append(parent, child);
        e.set_inline_style(child, WIDTH, "40");
        e.set_inline_style(child, HEIGHT, "20");
        let dom = dom_from(e);

        let (scene, _lay) = layout(&dom, Size { w: 100.0, h: 100.0 });
        let rects = scene
            .items
            .iter()
            .filter(|i| matches!(i, DisplayItem::Rect { .. }))
            .count();
        assert_eq!(
            rects, 1,
            "only the parent's background paints; bg never inherits"
        );
    }

    #[test]
    fn row_places_second_child_at_first_width_plus_gap() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let row = e.create_element(ElementTag::new(1));
        e.append(col, row);
        e.set_inline_style(row, DIRECTION, "row");
        e.set_inline_style(row, GAP, "10");
        // Two explicitly-sized child boxes so the geometry is exact.
        let a = e.create_element(ElementTag::new(2));
        e.append(row, a);
        e.set_inline_style(a, WIDTH, "30");
        e.set_inline_style(a, HEIGHT, "20");
        let b = e.create_element(ElementTag::new(2));
        e.append(row, b);
        e.set_inline_style(b, WIDTH, "40");
        e.set_inline_style(b, HEIGHT, "20");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });

        let a_rect = lay.rects.iter().find(|(id, _)| *id == a).unwrap().1;
        let b_rect = lay.rects.iter().find(|(id, _)| *id == b).unwrap().1;
        assert_eq!(a_rect.size.w, 30.0);
        // Second child starts at first child's width + the gap.
        assert_eq!(b_rect.origin.x, a_rect.size.w + 10.0);
        assert_eq!(b_rect.origin.x, 40.0);
        // Start-aligned on the cross axis: no vertical offset.
        assert_eq!(a_rect.origin.y, 0.0);
        assert_eq!(b_rect.origin.y, 0.0);
    }

    #[test]
    fn padding_insets_a_child() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, PADDING, "5");
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, WIDTH, "20");
        e.set_inline_style(child, HEIGHT, "10");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 100.0, h: 100.0 });

        let child_rect = lay.rects.iter().find(|(id, _)| *id == child).unwrap().1;
        // Child is inset by the padding on both axes.
        assert_eq!(child_rect.origin, Point { x: 5.0, y: 5.0 });
    }

    #[test]
    fn text_node_gets_baked_font_size() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let t = e.create_text("ab"); // 2 chars
        e.append(col, t);
        e.set_inline_style(t, HEIGHT, "20"); // a real 20px run (no 8px snapping)
        e.set_inline_style(t, FG, "#ffd040");

        let dom = dom_from(e);
        let (scene, lay) = layout(&dom, Size { w: 100.0, h: 50.0 });

        // Height is the requested 20px exactly; width matches the renderer's baked advance:
        // scale = floor(20 / 8) = 2, advance = 8 * 2 = 16 px/glyph, 2 chars -> 32.
        let t_rect = lay.rects.iter().find(|(id, _)| *id == t).unwrap().1;
        assert_eq!(t_rect.size, Size { w: 32.0, h: 20.0 });

        // The text leaf emits a Text run carrying the content, foreground, and the
        // snapped cell height.
        let text_item = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Text {
                    text, color, size, ..
                } => Some((text.clone(), *color, *size)),
                _ => None,
            })
            .expect("text run");
        assert_eq!(text_item.0, "ab");
        assert_eq!(
            text_item.1,
            Color {
                r: 0xff,
                g: 0xd0,
                b: 0x40,
                a: 255
            }
        );
        assert_eq!(text_item.2, 20.0, "the run's size is the requested 20px");
    }

    #[test]
    fn background_paints_behind_children() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, BG, "#202830");
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, WIDTH, "30");
        e.set_inline_style(child, HEIGHT, "20");
        e.set_inline_style(child, BG, "#ffffff");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });

        // The column background is emitted before the child's background.
        let col_bg = Color {
            r: 0x20,
            g: 0x28,
            b: 0x30,
            a: 255,
        };
        let child_bg = Color {
            r: 0xff,
            g: 0xff,
            b: 0xff,
            a: 255,
        };
        let col_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Rect { color, .. } if *color == col_bg))
            .unwrap();
        let child_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Rect { color, .. } if *color == child_bg))
            .unwrap();
        assert!(col_idx < child_idx, "parent background must paint first");
    }

    #[test]
    fn radius_style_flows_onto_the_emitted_rect() {
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, BG, "#313244");
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, RADIUS, "8");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });

        let radius = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Rect { radius, .. } => Some(*radius),
                _ => None,
            })
            .expect("background rect");
        assert_eq!(radius, 8.0);
    }

    #[test]
    fn hit_test_finds_deepest_node_and_misses_outside() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "100");
        e.set_inline_style(col, HEIGHT, "100");
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, WIDTH, "30");
        e.set_inline_style(child, HEIGHT, "20");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 100.0, h: 100.0 });

        // A point inside the child resolves to the child (topmost), not the column.
        assert_eq!(hit_test(&lay, Point { x: 10.0, y: 10.0 }), Some(child));
        // Inside the column but outside the child resolves to the column.
        assert_eq!(hit_test(&lay, Point { x: 80.0, y: 80.0 }), Some(col));
        // Past everything resolves to nothing.
        assert_eq!(hit_test(&lay, Point { x: 500.0, y: 500.0 }), None);
    }

    #[test]
    fn translate_y_shifts_node_and_child_rects_and_hit_test() {
        // A parent at the origin with translate-y: 10 and a padded child. The
        // translate must shift the parent's rect, flow down to the child's rect, and
        // — because the recorded rects are the translated ones — move the hit-test
        // target with the paint (no reflow: sizes are unchanged).
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "100");
        e.set_inline_style(col, HEIGHT, "100");
        e.set_inline_style(col, PADDING, "5");
        e.set_inline_style(col, TRANSLATE_Y, "10");
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, WIDTH, "30");
        e.set_inline_style(child, HEIGHT, "20");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 200.0 });

        // Baseline (without the translate) the column sits at y=0 and the child,
        // inset by padding 5, at y=5. With translate-y: 10 both shift down by 10.
        let col_rect = lay.rects.iter().find(|(id, _)| *id == col).unwrap().1;
        let child_rect = lay.rects.iter().find(|(id, _)| *id == child).unwrap().1;
        assert_eq!(
            col_rect.origin.y, 10.0,
            "parent rect shifted by translate-y"
        );
        assert_eq!(
            child_rect.origin.y, 15.0,
            "child inherits the parent's translate (5 padding + 10 translate)"
        );
        // Sizes are unchanged — translate shifts paint position, it does not reflow.
        assert_eq!(child_rect.size, Size { w: 30.0, h: 20.0 });

        // Hit-testing follows the paint: the child is hit at its shifted position
        // (x=5, y=15), and a point at the *un-shifted* spot (y=5) is no longer the
        // child.
        assert_eq!(hit_test(&lay, Point { x: 10.0, y: 16.0 }), Some(child));
        assert_ne!(hit_test(&lay, Point { x: 10.0, y: 6.0 }), Some(child));
    }

    #[test]
    fn translate_x_is_signed_and_subtree_relative() {
        // A negative translate-x on a parent slides its whole subtree left.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "100");
        e.set_inline_style(col, HEIGHT, "40");
        e.set_inline_style(col, TRANSLATE_X, "-24");
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, WIDTH, "20");
        e.set_inline_style(child, HEIGHT, "20");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 200.0 });

        let col_rect = lay.rects.iter().find(|(id, _)| *id == col).unwrap().1;
        let child_rect = lay.rects.iter().find(|(id, _)| *id == child).unwrap().1;
        // Both moved left by 24px; the child (laid out at the parent's left edge)
        // lands at -24 too.
        assert_eq!(col_rect.origin.x, -24.0);
        assert_eq!(child_rect.origin.x, -24.0);
    }

    #[test]
    fn opacity_half_fades_the_emitted_rect_alpha() {
        // A container with opacity 0.5 must emit its background rect with ~half the
        // alpha, so the renderer blends it over the background instead of overwriting.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, BG, "#313244");
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, OPACITY, "0.5");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });

        let color = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Rect { color, .. } => Some(*color),
                _ => None,
            })
            .expect("background rect");
        // 255 * 0.5 rounded = 128; RGB is untouched (straight-alpha fade).
        assert_eq!(color.a, 128, "alpha is scaled to ~half");
        assert_eq!((color.r, color.g, color.b), (0x31, 0x32, 0x44));
    }

    #[test]
    fn opacity_multiplies_down_the_subtree() {
        // A parent at 0.5 over a child at 0.5 yields an effective 0.25 on the child's
        // emitted ink — opacity composes multiplicatively, like nested CSS opacity.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, OPACITY, "0.5");
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, BG, "#ffffff");
        e.set_inline_style(child, WIDTH, "20");
        e.set_inline_style(child, HEIGHT, "20");
        e.set_inline_style(child, OPACITY, "0.5");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });

        // The only emitted rect is the white child; its alpha is 255 * 0.5 * 0.5.
        let color = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Rect { color, .. } => Some(*color),
                _ => None,
            })
            .expect("child background rect");
        // 255 * 0.25 = 63.75, rounds to 64.
        assert_eq!(color.a, 64, "nested opacity multiplies to ~quarter alpha");
    }

    #[test]
    fn full_opacity_leaves_alpha_untouched() {
        // The no-opacity (default 1.0) path must be byte-identical to before: a fully
        // opaque fill stays alpha 255.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, BG, "#313244");
        e.set_inline_style(card, WIDTH, "10");
        e.set_inline_style(card, HEIGHT, "10");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 50.0, h: 50.0 });
        let color = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Rect { color, .. } => Some(*color),
                _ => None,
            })
            .expect("background rect");
        assert_eq!(color.a, 255);
    }
}
