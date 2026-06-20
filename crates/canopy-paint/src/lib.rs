//! Canopy scene builder: turns the host [`Dom`] into a renderer-agnostic
//! [`DisplayList`] that any [`canopy_traits::Renderer`] can paint.
//!
//! This is the host-side "style + layout + paint-tree-walk" stage. The M1
//! implementation here is deliberately small — a **flexbox-style** layout that
//! reads a handful of inline style properties — so the whole pipeline (op-stream →
//! `Dom` → `DisplayList` → pixels) is exercised and testable *before* the real
//! engines are wired. `Stylo` (style) and `Taffy` (layout) drop in behind the
//! `StyleEngine` / `LayoutEngine` traits without changing this crate's output type.
//!
//! Each element lays its children out along a **main axis** chosen by the
//! [`DIRECTION`] inline style (`"row"` or `"column"`; default `"column"`),
//! separated by [`GAP`] px and inset by [`PADDING`] px on every side. The cross
//! axis is start-aligned. An element's size is its explicit [`WIDTH`]/[`HEIGHT`]
//! if set, otherwise its content size.
//!
//! [`layout`] is the single source of truth: it produces both the
//! [`DisplayList`] (back-to-front primitives) and a [`canopy_traits::LayoutResult`]
//! holding an **absolute** [`Rect`] for *every* node — elements and text alike.
//! That `LayoutResult` is what [`hit_test`] walks to map a [`Point`] back to the
//! topmost node beneath it. [`build_scene`] is a thin wrapper that returns only the
//! display list, preserving the renderer-facing contract.
//!
//! Text nodes are emitted as [`canopy_traits::DisplayItem::Text`] runs, which the
//! software renderer paints with a baked 8x8 bitmap font; the optional text
//! background still emits a [`canopy_traits::DisplayItem::Rect`] behind the run.
//! Shaped runs ([`canopy_traits::DisplayItem::Glyphs`]) arrive with the capable-tier
//! Parley text backend.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::ToString;
use alloc::vec::Vec;

use canopy_dom::{Dom, ROOT};
use canopy_protocol::{NodeId, PropId};
use canopy_traits::{Color, DisplayItem, DisplayList, LayoutResult, Point, Rect, Size};

/// Background color, e.g. `#202830`.
pub const BG: PropId = PropId::new(1);
/// Foreground / text color, e.g. `#ffd040`.
pub const FG: PropId = PropId::new(2);
/// Explicit width in integer pixels.
pub const WIDTH: PropId = PropId::new(3);
/// Explicit height in integer pixels.
pub const HEIGHT: PropId = PropId::new(4);
/// Gap between children along the main axis, in integer pixels.
pub const GAP: PropId = PropId::new(5);
/// Uniform padding inset on all four sides, in integer pixels.
pub const PADDING: PropId = PropId::new(6);
/// Main-axis direction: `"row"` (horizontal) or `"column"` (vertical, the default).
pub const DIRECTION: PropId = PropId::new(7);
/// Corner radius for an element's background rect, in integer pixels (`0` = square).
pub const RADIUS: PropId = PropId::new(8);
/// Subtree opacity as a unitless float in `[0, 1]` (`1.0` = fully opaque, the
/// default). Multiplies down the tree — like a CSS `opacity` — so setting it on a
/// container fades the whole subtree. This is a **paint-only** property: it scales
/// the alpha of every primitive a node and its descendants emit without touching
/// layout or hit-testing.
pub const OPACITY: PropId = PropId::new(9);
/// Horizontal paint translation in logical px, signed and fractional (e.g.
/// `-24px`, `12.5px`). Like a CSS `transform: translateX`, it shifts a node's
/// painted position **and** its hit-test rect by this amount and accumulates down
/// the subtree, with **no reflow** — siblings keep their original boxes.
pub const TRANSLATE_X: PropId = PropId::new(10);
/// Vertical paint translation in logical px, signed and fractional (e.g. `-24px`,
/// `12.5px`). The Y-axis counterpart of [`TRANSLATE_X`]; see it for the semantics.
pub const TRANSLATE_Y: PropId = PropId::new(11);
/// Flex **cross-axis** alignment of a container's children (CSS `align-items`):
/// `"start"`/`"center"`/`"end"`/`"stretch"`. On a column this centers children
/// horizontally; on a row, vertically. The value is a keyword string the layout
/// engine maps to its alignment enum; an unrecognized/absent value is the engine
/// default (start).
pub const ALIGN: PropId = PropId::new(12);
/// Flex **main-axis** distribution of a container's children (CSS `justify-content`):
/// `"start"`/`"center"`/`"end"`/`"space-between"`/`"space-around"`/`"space-evenly"`.
/// On a column this distributes children vertically; on a row, horizontally — the
/// honest way to center a hero or push a nav's ends apart without spacer hacks.
pub const JUSTIFY: PropId = PropId::new(13);
/// Horizontal **text alignment** of a node's text run within its own box (CSS
/// `text-align`): `"left"`/`"center"`/`"right"`. Unlike [`JUSTIFY`] (which
/// distributes child *boxes*), this aligns the *glyphs* inside the text node's box —
/// applied by the renderer using its own measured run width, the honest way to
/// center proportional text whose drawn width differs from the baked layout box. The
/// value is a keyword string the layout engine maps to a `0.0`/`0.5`/`1.0` fraction
/// on [`canopy_traits::DisplayItem::Text`]'s `align` field; an unrecognized or absent
/// value is left-aligned (`0.0`).
pub const TEXT_ALIGN: PropId = PropId::new(14);
/// Uniform outer **margin** in logical px (CSS `margin`): space *outside* the border box,
/// between this node and its flex siblings. Like [`PADDING`] but on the outside; the layout
/// engine maps it to Taffy's margin on all four sides.
pub const MARGIN: PropId = PropId::new(15);
/// **Minimum** width / height in logical px (CSS `min-width` / `min-height`): a floor the
/// layout engine clamps the box up to, even when content or a percentage would be smaller.
pub const MIN_WIDTH: PropId = PropId::new(16);
pub const MIN_HEIGHT: PropId = PropId::new(17);
/// **Maximum** width / height in logical px (CSS `max-width` / `max-height`): a ceiling the
/// layout engine clamps the box down to.
pub const MAX_WIDTH: PropId = PropId::new(18);
pub const MAX_HEIGHT: PropId = PropId::new(19);
/// Flex **grow** factor (CSS `flex-grow`), a unitless non-negative number: how much free space
/// on the container's main axis this child absorbs relative to its siblings. `0` (default) = do
/// not grow; `1` = take an equal share of the leftover space.
pub const FLEX_GROW: PropId = PropId::new(20);
/// **Border width** in logical px (CSS `border-width`): a uniform frame drawn inside the box edge
/// in [`BORDER_COLOR`]. `0` (default) = no frame. Paint-only — it does not change layout geometry.
pub const BORDER_WIDTH: PropId = PropId::new(21);
/// **Border color** (CSS `border-color`), a `#rrggbb` value the renderer strokes the frame in when
/// [`BORDER_WIDTH`] is positive.
pub const BORDER_COLOR: PropId = PropId::new(22);

// Box model — per-side margin/padding (px lengths; `auto` allowed for margins).
/// Top **margin** in logical px (CSS `margin-top`): space outside the border box on the top edge.
/// `auto` is allowed (centers the box on that axis); the per-side longhand of [`MARGIN`].
pub const MARGIN_TOP: PropId = PropId::new(23);
/// Right **margin** in logical px (CSS `margin-right`): space outside the border box on the right
/// edge. `auto` is allowed; the per-side longhand of [`MARGIN`].
pub const MARGIN_RIGHT: PropId = PropId::new(24);
/// Bottom **margin** in logical px (CSS `margin-bottom`): space outside the border box on the bottom
/// edge. `auto` is allowed; the per-side longhand of [`MARGIN`].
pub const MARGIN_BOTTOM: PropId = PropId::new(25);
/// Left **margin** in logical px (CSS `margin-left`): space outside the border box on the left edge.
/// `auto` is allowed; the per-side longhand of [`MARGIN`].
pub const MARGIN_LEFT: PropId = PropId::new(26);
/// Top **padding** in logical px (CSS `padding-top`): space inside the border box on the top edge;
/// the per-side longhand of [`PADDING`].
pub const PADDING_TOP: PropId = PropId::new(27);
/// Right **padding** in logical px (CSS `padding-right`): space inside the border box on the right
/// edge; the per-side longhand of [`PADDING`].
pub const PADDING_RIGHT: PropId = PropId::new(28);
/// Bottom **padding** in logical px (CSS `padding-bottom`): space inside the border box on the
/// bottom edge; the per-side longhand of [`PADDING`].
pub const PADDING_BOTTOM: PropId = PropId::new(29);
/// Left **padding** in logical px (CSS `padding-left`): space inside the border box on the left
/// edge; the per-side longhand of [`PADDING`].
pub const PADDING_LEFT: PropId = PropId::new(30);

// Display / visibility.
/// **Display** mode (CSS `display`): `"flex"`, `"block"` (mapped to flex), or `"none"` (removed from
/// layout and paint). The keyword the layout engine uses to decide whether and how a box participates.
pub const DISPLAY: PropId = PropId::new(31);
/// **Visibility** (CSS `visibility`): `"visible"` (default) or `"hidden"` (box keeps its layout space
/// but paints nothing). Paint-only — unlike `display: none` it does not collapse the box.
pub const VISIBILITY: PropId = PropId::new(32);

// Position.
/// **Position** scheme (CSS `position`): `"static"` (default), `"relative"` (offset from the in-flow
/// box by the inset props), or `"absolute"` (taken out of flow, placed against the containing block).
pub const POSITION: PropId = PropId::new(33);
/// **Top** inset in logical px or `auto` (CSS `top`): the box's offset from its containing block's top
/// edge under [`POSITION`] `relative`/`absolute`.
pub const INSET_TOP: PropId = PropId::new(34);
/// **Right** inset in logical px or `auto` (CSS `right`): the box's offset from its containing block's
/// right edge under [`POSITION`] `relative`/`absolute`.
pub const INSET_RIGHT: PropId = PropId::new(35);
/// **Bottom** inset in logical px or `auto` (CSS `bottom`): the box's offset from its containing
/// block's bottom edge under [`POSITION`] `relative`/`absolute`.
pub const INSET_BOTTOM: PropId = PropId::new(36);
/// **Left** inset in logical px or `auto` (CSS `left`): the box's offset from its containing block's
/// left edge under [`POSITION`] `relative`/`absolute`.
pub const INSET_LEFT: PropId = PropId::new(37);
/// **Stacking order** (CSS `z-index`), a signed integer: the paint order of a positioned box relative
/// to its siblings — higher values paint on top.
pub const Z_INDEX: PropId = PropId::new(38);

// Flex.
/// Flex **wrapping** (CSS `flex-wrap`): `"nowrap"` (default), `"wrap"`, or `"wrap-reverse"` — whether a
/// flex container's children overflow onto new lines along the cross axis.
pub const FLEX_WRAP: PropId = PropId::new(39);
/// Flex **basis** (CSS `flex-basis`): the child's initial main-axis size before grow/shrink — a px
/// length, a percentage, or `"auto"` (use the box's own width/height).
pub const FLEX_BASIS: PropId = PropId::new(40);
/// Flex **shrink** factor (CSS `flex-shrink`), a unitless non-negative `f32`: how much this child gives
/// up when the container overflows its main axis, relative to its siblings. `1` is the CSS default.
pub const FLEX_SHRINK: PropId = PropId::new(41);
/// Per-child **cross-axis alignment** (CSS `align-self`): a flex alignment keyword
/// (`"start"`/`"center"`/`"end"`/`"stretch"`) that overrides the container's [`ALIGN`] for this child.
pub const ALIGN_SELF: PropId = PropId::new(42);

// Sizing.
/// **Aspect ratio** (CSS `aspect-ratio`): the box's preferred width-to-height ratio as `"w/h"` or a
/// decimal — the layout engine derives the missing dimension from the one that is set.
pub const ASPECT_RATIO: PropId = PropId::new(43);
/// **Box sizing** (CSS `box-sizing`): `"content-box"` (default — width/height set the content box) or
/// `"border-box"` (width/height include padding and border).
pub const BOX_SIZING: PropId = PropId::new(44);

// Gaps — per-axis; the existing [`GAP`] fills both.
/// **Row gap** in logical px (CSS `row-gap`): the gutter between flex lines / grid rows; the per-axis
/// longhand of [`GAP`] along the block axis.
pub const ROW_GAP: PropId = PropId::new(45);
/// **Column gap** in logical px (CSS `column-gap`): the gutter between items along the inline axis; the
/// per-axis longhand of [`GAP`].
pub const COLUMN_GAP: PropId = PropId::new(46);

// Overflow — reserved for a later wave; the slot is mapped now.
/// **Overflow** handling (CSS `overflow`): `"visible"` (default), `"hidden"`, `"clip"`, or `"scroll"` —
/// what happens to content past the box edge. Reserved: the slot is registered but not yet consumed.
pub const OVERFLOW: PropId = PropId::new(47);

// Border longhands.
/// **Border style** (CSS `border-style`): `"none"` (default), `"solid"`, `"dashed"`, `"dotted"`, or
/// `"double"` — the stroke pattern of the border drawn in [`BORDER_COLOR`].
pub const BORDER_STYLE: PropId = PropId::new(48);
/// **Top border width** in logical px (CSS `border-top-width`): the per-side longhand of
/// [`BORDER_WIDTH`] for the top edge.
pub const BORDER_TOP_WIDTH: PropId = PropId::new(49);
/// **Right border width** in logical px (CSS `border-right-width`): the per-side longhand of
/// [`BORDER_WIDTH`] for the right edge.
pub const BORDER_RIGHT_WIDTH: PropId = PropId::new(50);
/// **Bottom border width** in logical px (CSS `border-bottom-width`): the per-side longhand of
/// [`BORDER_WIDTH`] for the bottom edge.
pub const BORDER_BOTTOM_WIDTH: PropId = PropId::new(51);
/// **Left border width** in logical px (CSS `border-left-width`): the per-side longhand of
/// [`BORDER_WIDTH`] for the left edge.
pub const BORDER_LEFT_WIDTH: PropId = PropId::new(52);
/// **Top border color** (CSS `border-top-color`), a `#rrggbb`/`#rrggbbaa` value: the per-side longhand
/// of [`BORDER_COLOR`] for the top edge.
pub const BORDER_TOP_COLOR: PropId = PropId::new(53);
/// **Right border color** (CSS `border-right-color`), a `#rrggbb`/`#rrggbbaa` value: the per-side
/// longhand of [`BORDER_COLOR`] for the right edge.
pub const BORDER_RIGHT_COLOR: PropId = PropId::new(54);
/// **Bottom border color** (CSS `border-bottom-color`), a `#rrggbb`/`#rrggbbaa` value: the per-side
/// longhand of [`BORDER_COLOR`] for the bottom edge.
pub const BORDER_BOTTOM_COLOR: PropId = PropId::new(55);
/// **Left border color** (CSS `border-left-color`), a `#rrggbb`/`#rrggbbaa` value: the per-side
/// longhand of [`BORDER_COLOR`] for the left edge.
pub const BORDER_LEFT_COLOR: PropId = PropId::new(56);
/// **Top-left corner radius** in logical px (CSS `border-top-left-radius`): the per-corner longhand of
/// [`RADIUS`] for the top-left corner.
pub const BORDER_TOP_LEFT_RADIUS: PropId = PropId::new(57);
/// **Top-right corner radius** in logical px (CSS `border-top-right-radius`): the per-corner longhand of
/// [`RADIUS`] for the top-right corner.
pub const BORDER_TOP_RIGHT_RADIUS: PropId = PropId::new(58);
/// **Bottom-right corner radius** in logical px (CSS `border-bottom-right-radius`): the per-corner
/// longhand of [`RADIUS`] for the bottom-right corner.
pub const BORDER_BOTTOM_RIGHT_RADIUS: PropId = PropId::new(59);
/// **Bottom-left corner radius** in logical px (CSS `border-bottom-left-radius`): the per-corner
/// longhand of [`RADIUS`] for the bottom-left corner.
pub const BORDER_BOTTOM_LEFT_RADIUS: PropId = PropId::new(60);

// Text.
/// **Font size** in logical px (CSS `font-size`): the em size the text backend rasterizes/shapes a run
/// at.
pub const FONT_SIZE: PropId = PropId::new(61);
/// **Font weight** (CSS `font-weight`): a `100`..`900` numeric weight or the `"normal"`/`"bold"`
/// keywords the text backend maps to a face.
pub const FONT_WEIGHT: PropId = PropId::new(62);
/// **Line height** (CSS `line-height`): a px length or a unitless multiplier of [`FONT_SIZE`] — the
/// distance between baselines of wrapped text.
pub const LINE_HEIGHT: PropId = PropId::new(63);
/// **Text decoration** (CSS `text-decoration`): `"none"` (default), `"underline"`, or `"line-through"`
/// — the line the renderer draws across a text run.
pub const TEXT_DECORATION: PropId = PropId::new(64);

// Outline.
/// **Outline width** in logical px (CSS `outline-width`): the stroke width of an outline drawn outside
/// the border box, painted in [`OUTLINE_COLOR`] and offset by [`OUTLINE_OFFSET`].
pub const OUTLINE_WIDTH: PropId = PropId::new(65);
/// **Outline color** (CSS `outline-color`), a `#rrggbb`/`#rrggbbaa` value the renderer strokes the
/// outline in when [`OUTLINE_WIDTH`] is positive.
pub const OUTLINE_COLOR: PropId = PropId::new(66);
/// **Outline offset** in logical px (CSS `outline-offset`), may be negative: the gap between the border
/// box edge and the outline stroke.
pub const OUTLINE_OFFSET: PropId = PropId::new(67);

// Effects.
/// **Box shadow** (CSS `box-shadow`), carrying `offx offy blur spread #color [inset]`: a drop (or, with
/// `inset`, inner) shadow the renderer paints around or inside the box.
pub const BOX_SHADOW: PropId = PropId::new(68);
/// **Background image** (CSS `background-image`), carrying a `linear-gradient(...)` value the renderer
/// rasterizes behind the box's content (in front of [`BG`]).
pub const BACKGROUND_IMAGE: PropId = PropId::new(69);

/// Baked-font cell advance at the reference height, in pixels.
const TEXT_ADVANCE: f32 = 8.0;
/// Baked-font cell height the renderer rasterizes at scale 1.
const TEXT_CELL: f32 = 8.0;
/// Default text cell height when no [`HEIGHT`] is set.
const TEXT_HEIGHT: f32 = 16.0;
/// Default foreground ink (light gray) when no [`FG`] is set.
const DEFAULT_FG: Color = Color {
    r: 0xe6,
    g: 0xe6,
    b: 0xe6,
    a: 255,
};

/// Main-axis direction for an element's flex layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Direction {
    /// Children flow top-to-bottom; main axis is Y.
    Column,
    /// Children flow left-to-right; main axis is X.
    Row,
}

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

// Sizes are integer-pixel strings; integer parsing is `core`-safe (float parsing
// historically was not), which keeps this crate honestly `no_std`.
fn parse_dim(s: &str) -> Option<f32> {
    s.parse::<u32>().ok().map(|v| v as f32)
}

fn parse_direction(s: &str) -> Option<Direction> {
    match s {
        "row" => Some(Direction::Row),
        "column" => Some(Direction::Column),
        _ => None,
    }
}

fn style_color(dom: &Dom, node: NodeId, prop: PropId) -> Option<Color> {
    dom.style(node, prop).and_then(parse_color)
}

fn style_dim(dom: &Dom, node: NodeId, prop: PropId) -> Option<f32> {
    dom.style(node, prop).and_then(parse_dim)
}

/// Raw integer-pixel value of `prop` on `node`, if set and parseable.
fn style_px(dom: &Dom, node: NodeId, prop: PropId) -> Option<u32> {
    dom.style(node, prop).and_then(|s| s.parse::<u32>().ok())
}

fn style_direction(dom: &Dom, node: NodeId) -> Direction {
    dom.style(node, DIRECTION)
        .and_then(parse_direction)
        .unwrap_or(Direction::Column)
}

fn rect_item(rect: Rect, color: Color, radius: f32) -> DisplayItem {
    DisplayItem::Rect {
        rect,
        color,
        radius,
    }
}

/// The node's corner [`RADIUS`] in logical px (default `0.0` = square).
///
/// Read inline like the other pixel styles; whole-pixel integer parsing keeps
/// this crate honestly `no_std`. A node with no `radius`/`border-radius` style
/// gets a square rect, exactly the legacy behavior.
fn style_radius(dom: &Dom, node: NodeId) -> f32 {
    style_dim(dom, node, RADIUS).unwrap_or(0.0)
}

/// The node's [`TEXT_ALIGN`] as a `0.0`/`0.5`/`1.0` fraction: `"center"` => `0.5`,
/// `"right"` => `1.0`, anything else (including absent) => `0.0` (left/start). The
/// fraction rides onto the emitted [`DisplayItem::Text`]'s `align`, where the
/// renderer applies it against its own measured run width.
fn style_text_align(dom: &Dom, node: NodeId) -> f32 {
    match dom.style(node, TEXT_ALIGN) {
        Some("center") => 0.5,
        Some("right") => 1.0,
        _ => 0.0,
    }
}

/// Whether `rect` contains `point` (top/left inclusive, bottom/right exclusive).
fn rect_contains(rect: &Rect, point: Point) -> bool {
    point.x >= rect.origin.x
        && point.y >= rect.origin.y
        && point.x < rect.origin.x + rect.size.w
        && point.y < rect.origin.y + rect.size.h
}

/// Lay the whole tree out within `viewport`, producing both the back-to-front
/// [`DisplayList`] and a [`LayoutResult`] with an **absolute** [`Rect`] for every
/// node (elements *and* text), in tree order. The `LayoutResult` is what
/// [`hit_test`] consumes; the `DisplayList` is what a [`canopy_traits::Renderer`]
/// paints.
///
/// Top-level nodes stack down the viewport. A top-level node with no explicit
/// width stretches to the viewport width (the common "fill the screen" case);
/// nested nodes are sized to their content (or an explicit width/height), so flex
/// rows and buttons stay tight rather than inheriting the viewport's box.
pub fn layout(dom: &Dom, viewport: Size) -> (DisplayList, LayoutResult) {
    let mut items = Vec::new();
    let mut rects = Vec::new();
    let mut y = 0.0;
    for &root in dom.children(ROOT) {
        let size = layout_node(
            dom,
            root,
            Point { x: 0.0, y },
            viewport,
            true,
            &mut items,
            &mut rects,
        );
        y += size.h;
    }
    (DisplayList { items }, LayoutResult { rects })
}

/// Build a display list for the whole tree within `viewport`.
///
/// A thin wrapper over [`layout`] that discards the [`LayoutResult`]; kept at this
/// exact signature so renderer hosts continue to compile unchanged.
pub fn build_scene(dom: &Dom, viewport: Size) -> DisplayList {
    layout(dom, viewport).0
}

/// Return the topmost node whose absolute rect contains `point`, or `None`.
///
/// `layout.rects` is in back-to-front tree order (parents before children, earlier
/// siblings before later), so scanning from the end yields the most-recently-added
/// — i.e. visually topmost — hit.
pub fn hit_test(layout: &LayoutResult, point: Point) -> Option<NodeId> {
    layout
        .rects
        .iter()
        .rev()
        .find(|(_, rect)| rect_contains(rect, point))
        .map(|(id, _)| *id)
}

/// Lay one node out at `origin` within the `avail` box, recording its absolute
/// rect into `rects` and its primitives into `out`. Returns the node's used size.
fn layout_node(
    dom: &Dom,
    id: NodeId,
    origin: Point,
    avail: Size,
    stretch_to_viewport: bool,
    out: &mut Vec<DisplayItem>,
    rects: &mut Vec<(NodeId, Rect)>,
) -> Size {
    let node = match dom.node(id) {
        Some(n) => n,
        None => return Size::default(),
    };

    // Text leaf: a baked-font run, with an optional background rect behind it.
    if let Some(text) = node.text.as_deref() {
        // Scale is the integer baked-font multiplier: max(1, floor(size / 8)).
        // Heights are whole-pixel strings, so the floor is exact integer division
        // — no `f32::floor` (a `std`-only intrinsic) needed in this `no_std` crate.
        let requested_px = style_px(dom, id, HEIGHT).unwrap_or(TEXT_HEIGHT as u32);
        let scale = (requested_px / TEXT_CELL as u32).max(1) as f32;
        let h = TEXT_CELL * scale;
        let advance = TEXT_ADVANCE * scale;
        let w = style_dim(dom, id, WIDTH).unwrap_or(text.chars().count() as f32 * advance);
        let rect = Rect {
            origin,
            size: Size { w, h },
        };
        if let Some(bg) = style_color(dom, id, BG) {
            out.push(rect_item(rect, bg, style_radius(dom, id)));
        }
        let fg = style_color(dom, id, FG).unwrap_or(DEFAULT_FG);
        out.push(DisplayItem::Text {
            origin,
            text: text.to_string(),
            color: fg,
            size: h,
            // Align within the node's own box width (this baked path's `w`), using
            // the node's `text-align`. For the baked CPU run the box equals the
            // run, so the offset is ~0; reading TEXT_ALIGN keeps it consistent with
            // the Taffy path and renders any real-glyph tier correctly.
            box_w: w,
            align: style_text_align(dom, id),
        });
        rects.push((id, rect));
        return Size { w, h };
    }

    // Element: flex its children along the main axis. Reserve the element's own
    // rect slots now (both the display-list background and the hit-test rect) so
    // the background paints behind — and the parent hit-tests before — its
    // children; backfill them once the final size is known.
    let dir = style_direction(dom, id);
    let gap = style_dim(dom, id, GAP).unwrap_or(0.0);
    let pad = style_dim(dom, id, PADDING).unwrap_or(0.0);
    let explicit_w = style_dim(dom, id, WIDTH);
    let explicit_h = style_dim(dom, id, HEIGHT);

    let bg_index = out.len();
    let has_bg = style_color(dom, id, BG).is_some();
    if has_bg {
        // Placeholder; backfilled below once the final rect is known.
        out.push(rect_item(Rect::default(), Color::default(), 0.0));
    }
    let rect_index = rects.len();
    rects.push((id, Rect::default()));

    // Content box: inset by padding on all sides. Children fill the remaining
    // available space along the cross axis.
    let content_origin = Point {
        x: origin.x + pad,
        y: origin.y + pad,
    };
    let child_avail = Size {
        w: (avail.w - 2.0 * pad).max(0.0),
        h: (avail.h - 2.0 * pad).max(0.0),
    };

    let mut main = 0.0_f32; // accumulated extent along the main axis
    let mut cross_max = 0.0_f32; // largest child extent along the cross axis
    let mut first = true;
    for &child in dom.children(id) {
        if !first {
            main += gap;
        }
        first = false;
        let child_origin = match dir {
            Direction::Column => Point {
                x: content_origin.x,
                y: content_origin.y + main,
            },
            Direction::Row => Point {
                x: content_origin.x + main,
                y: content_origin.y,
            },
        };
        let size = layout_node(dom, child, child_origin, child_avail, false, out, rects);
        let (child_main, child_cross) = match dir {
            Direction::Column => (size.h, size.w),
            Direction::Row => (size.w, size.h),
        };
        main += child_main;
        if child_cross > cross_max {
            cross_max = child_cross;
        }
    }

    // Content size = children + gaps along main, max child along cross, + padding.
    let content_main = main + 2.0 * pad;
    let content_cross = cross_max + 2.0 * pad;
    let (content_w, content_h) = match dir {
        Direction::Column => (content_cross, content_main),
        Direction::Row => (content_main, content_cross),
    };

    // Explicit size wins; otherwise content. A top-level node with no explicit
    // width stretches to the viewport width (the "fill the screen" case); nested
    // nodes are content-sized so flex rows/buttons stay tight. Cross-axis stretch
    // to a parent's box (align-items: stretch) is left for the real layout engine
    // (Taffy) behind LayoutEngine.
    let w = explicit_w.unwrap_or(if stretch_to_viewport {
        content_w.max(avail.w)
    } else {
        content_w
    });
    let h = explicit_h.unwrap_or(content_h);

    let rect = Rect {
        origin,
        size: Size { w, h },
    };
    rects[rect_index].1 = rect;
    if has_bg {
        let bg = style_color(dom, id, BG).unwrap_or_default();
        out[bg_index] = rect_item(rect, bg, style_radius(dom, id));
    }
    Size { w, h }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_core::Emitter;
    use canopy_dom::Dom;
    use canopy_protocol::ElementTag;
    use canopy_traits::OpSink;

    #[test]
    fn animation_prop_ids_are_the_next_free_distinct_slots() {
        // OPACITY/TRANSLATE_X/TRANSLATE_Y are the three ids after RADIUS=8. Guard
        // their concrete values (the CSS map and the scene builders key off these)
        // and that they don't collide with the existing layout/paint props.
        assert_eq!(OPACITY.raw(), 9);
        assert_eq!(TRANSLATE_X.raw(), 10);
        assert_eq!(TRANSLATE_Y.raw(), 11);
        // TEXT_ALIGN is the next free slot after JUSTIFY=13.
        assert_eq!(ALIGN.raw(), 12);
        assert_eq!(JUSTIFY.raw(), 13);
        assert_eq!(TEXT_ALIGN.raw(), 14);
        let all = [
            BG,
            FG,
            WIDTH,
            HEIGHT,
            GAP,
            PADDING,
            DIRECTION,
            RADIUS,
            OPACITY,
            TRANSLATE_X,
            TRANSLATE_Y,
            ALIGN,
            JUSTIFY,
            TEXT_ALIGN,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.raw(), b.raw(), "PropId ids must be unique");
            }
        }
    }

    fn dom_from(e: Emitter) -> Dom {
        let mut e = e;
        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();
        dom
    }

    #[test]
    fn stacks_children_and_colors_from_styles() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, BG, "#202830");
        let a = e.create_text("ab"); // 2 chars -> 32px wide at scale 2
        e.append(col, a);
        e.set_inline_style(a, HEIGHT, "20"); // floor(20/8) = scale 2 -> 16px tall
        e.set_inline_style(a, FG, "#ffd040");

        let dom = dom_from(e);

        let (scene, lay) = layout(&dom, Size { w: 100.0, h: 50.0 });
        // First item is the column background (backfilled in place behind its
        // children), spanning the viewport width and the child's height.
        match &scene.items[0] {
            DisplayItem::Rect { rect, color, .. } => {
                assert_eq!(
                    *color,
                    Color {
                        r: 0x20,
                        g: 0x28,
                        b: 0x30,
                        a: 255
                    }
                );
                assert_eq!(rect.size.w, 100.0); // stretched to viewport width
                assert_eq!(rect.size.h, 16.0); // floor(20/8)=2 -> 16px tall text
            }
            _ => panic!("expected the column background rect first"),
        }
        // The text leaf emits a Text run carrying the content, the foreground color,
        // and the snapped cell height — not a placeholder foreground rect.
        let fg = Color {
            r: 0xff,
            g: 0xd0,
            b: 0x40,
            a: 255,
        };
        let text_item = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Text {
                    origin,
                    text,
                    color,
                    size,
                    box_w,
                    align,
                } => Some((*origin, text.clone(), *color, *size, *box_w, *align)),
                _ => None,
            })
            .expect("text run");
        assert_eq!(text_item.1, "ab");
        assert_eq!(text_item.2, fg);
        assert_eq!(text_item.3, 16.0);
        // box_w is the run's own box width (2 chars * 16px advance); no text-align
        // was set, so the run is left-aligned (align 0.0).
        assert_eq!(text_item.4, 32.0);
        assert_eq!(text_item.5, 0.0);
        // No foreground-colored Rect is emitted for the text anymore.
        assert!(
            !scene.items.iter().any(|i| matches!(
                i,
                DisplayItem::Rect { color, .. } if *color == fg
            )),
            "text must not emit a foreground placeholder rect"
        );

        // Every node gets an absolute rect: the column at the origin and the text
        // leaf nested inside it.
        assert_eq!(lay.rects.len(), 2);
        assert_eq!(lay.rects[0].0, col);
        assert_eq!(lay.rects[0].1.origin, Point { x: 0.0, y: 0.0 });
        assert_eq!(lay.rects[1].0, a);
        // 2 chars at scale 2 -> 16px advance each -> 32px wide, 16px tall.
        assert_eq!(lay.rects[1].1.size, Size { w: 32.0, h: 16.0 });
    }

    #[test]
    fn row_places_children_left_to_right() {
        let mut e = Emitter::new();
        let row = e.create_element(ElementTag::new(1));
        e.append(ROOT, row);
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

        // rects: [row, a, b] in tree order.
        let a_rect = lay.rects.iter().find(|(id, _)| *id == a).unwrap().1;
        let b_rect = lay.rects.iter().find(|(id, _)| *id == b).unwrap().1;
        assert_eq!(a_rect.origin.x, 0.0);
        // Second child starts at first child's width + the gap.
        assert_eq!(b_rect.origin.x, a_rect.size.w + 10.0);
        assert_eq!(b_rect.origin.x, 40.0);
        // Same cross-axis start (start-aligned), no vertical offset.
        assert_eq!(a_rect.origin.y, 0.0);
        assert_eq!(b_rect.origin.y, 0.0);
    }

    #[test]
    fn padding_insets_children_and_grows_the_box() {
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
        // Column height = content (10) + 2 * padding (10) = 20.
        let col_rect = lay.rects.iter().find(|(id, _)| *id == col).unwrap().1;
        assert_eq!(col_rect.size.h, 20.0);
    }

    #[test]
    fn hit_test_finds_deepest_node_and_misses_outside() {
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
        let (_, lay) = layout(&dom, Size { w: 100.0, h: 100.0 });

        // A point inside the child resolves to the child (topmost), not the column.
        assert_eq!(hit_test(&lay, Point { x: 10.0, y: 10.0 }), Some(child));
        // A point inside the column but outside the child resolves to the column.
        // The column stretches to the viewport width (100), so x=80 is past the
        // 30px-wide child but still within the column.
        assert_eq!(hit_test(&lay, Point { x: 80.0, y: 10.0 }), Some(col));
        // A point past everything resolves to nothing.
        assert_eq!(hit_test(&lay, Point { x: 500.0, y: 500.0 }), None);
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

        // The single element background rect carries the parsed corner radius.
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
    fn no_radius_style_means_square() {
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, BG, "#313244");
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });

        // Without a radius style the emitted rect is square (radius 0.0), preserving
        // the legacy behavior.
        match &scene.items[0] {
            DisplayItem::Rect { radius, .. } => assert_eq!(*radius, 0.0),
            other => panic!("expected a background rect, got {other:?}"),
        }
    }
}
