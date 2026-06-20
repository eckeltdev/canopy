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
    ALIGN, ALIGN_SELF, ASPECT_RATIO, BACKGROUND_IMAGE, BG, BORDER_BOTTOM_WIDTH, BORDER_COLOR,
    BORDER_LEFT_WIDTH, BORDER_RIGHT_WIDTH, BORDER_TOP_WIDTH, BORDER_WIDTH, BOX_SHADOW, BOX_SIZING,
    COLUMN_GAP, DIRECTION, DISPLAY, FG, FLEX_BASIS, FLEX_GROW, FLEX_SHRINK, FLEX_WRAP, FONT_SIZE,
    GAP, HEIGHT, INSET_BOTTOM, INSET_LEFT, INSET_RIGHT, INSET_TOP, JUSTIFY, MARGIN, MARGIN_BOTTOM,
    MARGIN_LEFT, MARGIN_RIGHT, MARGIN_TOP, MAX_HEIGHT, MAX_WIDTH, MIN_HEIGHT, MIN_WIDTH, OPACITY,
    OUTLINE_COLOR, OUTLINE_OFFSET, OUTLINE_WIDTH, PADDING, PADDING_BOTTOM, PADDING_LEFT,
    PADDING_RIGHT, PADDING_TOP, POSITION, RADIUS, ROW_GAP, TEXT_ALIGN, TEXT_DECORATION,
    TRANSLATE_X, TRANSLATE_Y, VISIBILITY, WIDTH, Z_INDEX,
};
use canopy_protocol::{ElementTag, NodeId, PropId};
use canopy_traits::{
    Color, DisplayItem, DisplayList, GradientDirection, GradientStop, GradientStops, LayoutResult,
    Point, Rect, Size,
};

use taffy::prelude::length;
use taffy::{
    AlignItems, AvailableSpace, BoxSizing, Dimension, Display, FlexDirection, FlexWrap,
    JustifyContent, LengthPercentage, LengthPercentageAuto, Position, Rect as TaffyRect,
    Size as TaffySize, Style, TaffyTree,
};

/// Default text size (px) when no [`HEIGHT`] is set.
const TEXT_HEIGHT: u32 = 16;
/// The baked font's square cell size in px (`canopy_text_baked::CELL_W == CELL_H == 8`). A
/// text run's width estimate steps by this so the layout box matches the renderer's advance.
const BAKED_CELL_PX: f32 = 8.0;
/// Well-known element tags (mirrors the protocol's `ElementTag` registry) whose lite layout
/// gets a UA-stylesheet default: a button/input centers its content unless the author overrides.
const EL_BUTTON: u16 = 3;
const EL_INPUT: u16 = 4;
/// Default foreground ink (light gray) when no [`FG`] is set.
const DEFAULT_FG: Color = Color {
    r: 0xe6,
    g: 0xe6,
    b: 0xe6,
    a: 255,
};

/// Parse a `#rrggbb` (alpha = 255) or `#rrggbbaa` (4th byte is the alpha) hex color.
/// The 8-hex form lets translucent shadows/gradients survive parsing (a half-alpha
/// `#00000080` drop shadow, a gradient stop fading to transparent), which the 6-hex-only
/// form would reject. Any other length is rejected.
fn parse_color(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 && hex.len() != 8 {
        return None;
    }
    let a = if hex.len() == 8 {
        u8::from_str_radix(&hex[6..8], 16).ok()?
    } else {
        255
    };
    Some(Color {
        r: u8::from_str_radix(&hex[0..2], 16).ok()?,
        g: u8::from_str_radix(&hex[2..4], 16).ok()?,
        b: u8::from_str_radix(&hex[4..6], 16).ok()?,
        a,
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

/// A parsed `box-shadow`: the producer freezes the value as four space-separated tokens,
/// `<dx> <dy> <blur> <#hex>` (no spread, no `inset`). `dx`/`dy`/`blur` are signed integer
/// px; the color is `#rrggbb`/`#rrggbbaa` (often translucent).
struct ParsedShadow {
    offset: Point,
    blur: f32,
    color: Color,
}

/// Parse the frozen `BOX_SHADOW` string `"<dx> <dy> <blur> <#hex>"` into its offset, blur,
/// and color. Returns `None` if any of the four tokens is missing or unparseable. Offsets
/// and blur are read via [`signed_px`] (a leading `-` survives, `no_std`-clean); the color
/// via [`parse_color`] (so an 8-hex translucent shadow parses).
fn parse_box_shadow(s: &str) -> Option<ParsedShadow> {
    let mut tok = s.split_whitespace();
    let dx = signed_px(tok.next()?)?;
    let dy = signed_px(tok.next()?)?;
    let blur = signed_px(tok.next()?)?;
    let color = parse_color(tok.next()?)?;
    Some(ParsedShadow {
        offset: Point { x: dx, y: dy },
        blur,
        color,
    })
}

/// Parse the frozen `BACKGROUND_IMAGE` string `"linear-gradient(<deg>, <#hex>[, <#hex>...]"`
/// into evenly-spaced [`GradientStops`] plus a [`GradientDirection`]. `<deg>` is a bare
/// integer `0..359` (0=to-top, 90=to-right, 180=to-bottom, 270=to-left); each `<#hex>` is
/// `#rrggbb`/`#rrggbbaa`, 1..8 stops. Returns `None` if the wrapper is missing, the angle is
/// unparseable, or there are no parseable color stops.
///
/// Stops are spaced evenly across `[0, 1]` (`i / (n - 1)`, a single stop at `0.0`). The angle
/// maps to [`GradientDirection::Vertical`] for a roughly vertical run (`deg <= 45`, `135..=225`,
/// or `>= 315`) and [`GradientDirection::Horizontal`] otherwise.
fn parse_linear_gradient(s: &str) -> Option<(GradientStops, GradientDirection)> {
    let inner = s
        .trim()
        .strip_prefix("linear-gradient(")?
        .strip_suffix(')')?;
    let mut parts = inner.split(',');
    let deg = parts.next()?.trim().parse::<i32>().ok()?;
    // Parse the color stops in order; ignore any unparseable token rather than aborting the
    // whole gradient (the producer freezes well-formed hex, so this is defensive).
    let mut colors: Vec<Color> = Vec::new();
    for tok in parts {
        if let Some(c) = parse_color(tok.trim()) {
            colors.push(c);
        }
    }
    if colors.is_empty() {
        return None;
    }
    let n = colors.len();
    let mut stops: Vec<GradientStop> = Vec::with_capacity(n);
    for (i, color) in colors.into_iter().enumerate() {
        // Evenly spaced: a single stop sits at 0.0; otherwise i/(n-1) spans [0, 1].
        let position = if n == 1 {
            0.0
        } else {
            i as f32 / (n - 1) as f32
        };
        stops.push(GradientStop { color, position });
    }
    let direction = gradient_direction(deg);
    Some((GradientStops::from_slice(&stops), direction))
}

/// Map a `linear-gradient` angle (degrees) to one of the two display-list axes. A roughly
/// vertical run (`deg <= 45`, `135..=225`, or `>= 315` — i.e. near 0/180/360) is
/// [`GradientDirection::Vertical`]; everything else (near 90/270) is
/// [`GradientDirection::Horizontal`]. The frozen producer angle is `0..359`, but the
/// comparisons tolerate any integer.
fn gradient_direction(deg: i32) -> GradientDirection {
    if deg <= 45 || (135..=225).contains(&deg) || deg >= 315 {
        GradientDirection::Vertical
    } else {
        GradientDirection::Horizontal
    }
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
    /// `visibility: visible` — when `false` (`hidden`) this node emits no
    /// background/border/text of its own, but it is still laid out and its children
    /// still paint. Not inherited: a hidden parent does not hide a visible child.
    visible: bool,
    /// Paint order: a higher [`Z_INDEX`] paints later (on top). The display-list builder
    /// stable-sorts siblings by this, so tree order breaks ties for equal z.
    z_index: i32,
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

/// Resolve one **padding** edge into a Taffy [`LengthPercentage`]. Padding is
/// non-negative (`LengthPercentage` cannot express `auto` or a negative length), so
/// each edge is read as: the per-side longhand (`side`, e.g. [`PADDING_TOP`]) if set,
/// else the uniform [`PADDING`] shorthand, else `0`. A percentage (`"50%"`) resolves
/// against the container; a length (`"Npx"`/`"N"`) is taken in px.
fn padding_edge(dom: &Dom, id: NodeId, side: PropId) -> LengthPercentage {
    let raw = dom.style(id, side).or_else(|| dom.style(id, PADDING));
    if let Some(s) = raw {
        if let Some(pct) = s.strip_suffix('%') {
            if let Ok(n) = pct.trim().parse::<u32>() {
                return LengthPercentage::percent(n as f32 / 100.0);
            }
        }
        let num = s.strip_suffix("px").map(str::trim).unwrap_or(s);
        if let Ok(n) = num.parse::<u32>() {
            return LengthPercentage::length(n as f32);
        }
    }
    LengthPercentage::length(0.0)
}

/// Resolve one **margin** or **inset** edge into a Taffy [`LengthPercentageAuto`].
/// Unlike padding, margins/insets admit `auto` (`margin: 0 auto` centering,
/// `margin-left: auto`) and **negative** lengths (a pulled-in box). Each edge is:
/// the per-side longhand (`side`) if set, else `fallback` (the uniform shorthand for
/// margin, or `None` for inset which has no shorthand), else `default`.
///
/// `default` differs by property: margin's unset edge is `length(0)` (zero spacing);
/// inset's unset edge is `auto` (Taffy's default — use the in-flow static position, not
/// a 0-offset that would stretch an absolute box across the container).
///
/// `auto` -> [`LengthPercentageAuto::auto`]; a percentage (`"50%"`) -> percent of the
/// container; a signed length (`"-12"`, `"24px"`) -> px (parsed via [`signed_px`] so a
/// leading `-` survives, keeping this `no_std`-clean).
fn auto_edge(
    dom: &Dom,
    id: NodeId,
    side: PropId,
    fallback: Option<PropId>,
    default: LengthPercentageAuto,
) -> LengthPercentageAuto {
    let raw = dom
        .style(id, side)
        .or_else(|| fallback.and_then(|f| dom.style(id, f)));
    if let Some(s) = raw {
        let s = s.trim();
        if s == "auto" {
            return LengthPercentageAuto::auto();
        }
        if let Some(pct) = s.strip_suffix('%') {
            if let Ok(n) = pct.trim().parse::<u32>() {
                return LengthPercentageAuto::percent(n as f32 / 100.0);
            }
        }
        if let Some(n) = signed_px(s) {
            return LengthPercentageAuto::length(n);
        }
    }
    default
}

/// Parse a signed integer-or-decimal pixel string (`"24"`, `"-12"`, `"-12px"`, `"7.5"`)
/// into an `f32`, or `None` when unparseable. A trailing `"px"` is tolerated. Reuses
/// `f32::from_str` (available in `core` on Canopy's targets), so negatives and decimals
/// — which the `u32`-only [`style_px`] rejects — parse without pulling in `std`.
fn signed_px(s: &str) -> Option<f32> {
    let num = s.strip_suffix("px").map(str::trim).unwrap_or(s);
    num.parse::<f32>().ok()
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

/// Map a CSS alignment keyword to a Taffy [`AlignItems`] (taffy 0.11 models alignment
/// as a struct with associated-const keywords), or `None` when unrecognized. Shared by
/// [`style_align`] (the container's `align-items`) and [`style_align_self`] (an item's
/// `align-self`), which take the same keyword set.
fn parse_align(value: &str) -> Option<AlignItems> {
    match value {
        "start" | "flex-start" => Some(AlignItems::FLEX_START),
        "center" => Some(AlignItems::CENTER),
        "end" | "flex-end" => Some(AlignItems::FLEX_END),
        "stretch" => Some(AlignItems::STRETCH),
        _ => None,
    }
}

/// The container's cross-axis alignment ([`ALIGN`] / CSS `align-items`), or `None`
/// (Taffy's default = stretch/start) when unset or unrecognized.
fn style_align(dom: &Dom, id: NodeId) -> Option<AlignItems> {
    dom.style(id, ALIGN).and_then(parse_align)
}

/// The item's own cross-axis alignment ([`ALIGN_SELF`] / CSS `align-self`), overriding
/// the parent's `align-items` for this one child. `None` (Taffy's default) when unset
/// or unrecognized, so it falls back to the container's alignment.
fn style_align_self(dom: &Dom, id: NodeId) -> Option<AlignItems> {
    dom.style(id, ALIGN_SELF).and_then(parse_align)
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

/// The node's [`DISPLAY`] mapped to a Taffy [`Display`]: `"none"` -> [`Display::None`]
/// (Taffy zero-sizes the node and skips its subtree), `"flex"` (or any other value) ->
/// [`Display::Flex`]. Default is `Flex` — this crate is flex-only, so an unset or
/// unrecognized `display` keeps the existing flex behavior.
fn style_display(dom: &Dom, id: NodeId) -> Display {
    match dom.style(id, DISPLAY) {
        Some("none") => Display::None,
        _ => Display::Flex,
    }
}

/// Whether the node is **visible** for painting ([`VISIBILITY`]). `"hidden"` -> `false`
/// (the node is still laid out, but emits no background/border/text of its own; its
/// children still paint). Anything else (incl. unset / `"visible"`) -> `true`.
fn style_visible(dom: &Dom, id: NodeId) -> bool {
    !matches!(dom.style(id, VISIBILITY), Some("hidden"))
}

/// The node's [`POSITION`] mapped to a Taffy [`Position`]: `"absolute"` ->
/// [`Position::Absolute`], `"relative"`/`"static"`/anything else (incl. unset) ->
/// [`Position::Relative`] (Taffy's default). Taffy has no separate `static`; both
/// `relative` and `static` map to `Relative`, which is in-flow.
fn style_position(dom: &Dom, id: NodeId) -> Position {
    match dom.style(id, POSITION) {
        Some("absolute") => Position::Absolute,
        _ => Position::Relative,
    }
}

/// The node's [`FLEX_WRAP`] mapped to a Taffy [`FlexWrap`]: `"wrap"` -> [`FlexWrap::Wrap`],
/// `"wrap-reverse"` -> [`FlexWrap::WrapReverse`], `"nowrap"`/anything else (incl. unset) ->
/// [`FlexWrap::NoWrap`] (Taffy's default).
fn style_flex_wrap(dom: &Dom, id: NodeId) -> FlexWrap {
    match dom.style(id, FLEX_WRAP) {
        Some("wrap") => FlexWrap::Wrap,
        Some("wrap-reverse") => FlexWrap::WrapReverse,
        _ => FlexWrap::NoWrap,
    }
}

/// The node's [`BOX_SIZING`] mapped to a Taffy [`BoxSizing`]: `"content-box"` ->
/// [`BoxSizing::ContentBox`], `"border-box"`/anything else (incl. unset) ->
/// [`BoxSizing::BorderBox`] (Taffy's default, which matches the box model authors get
/// once `border`/`padding` inset content).
fn style_box_sizing(dom: &Dom, id: NodeId) -> BoxSizing {
    match dom.style(id, BOX_SIZING) {
        Some("content-box") => BoxSizing::ContentBox,
        _ => BoxSizing::BorderBox,
    }
}

/// The node's [`ASPECT_RATIO`] as a Taffy `Option<f32>` (width / height). Accepts two
/// CSS forms: a bare decimal (`"1.5"`) used directly, or a `"w/h"` ratio (`"16/9"`)
/// split on `'/'` and divided. `None` when unset, unparseable, or a zero denominator
/// (which would be a non-finite ratio).
fn style_aspect_ratio(dom: &Dom, id: NodeId) -> Option<f32> {
    let s = dom.style(id, ASPECT_RATIO)?.trim();
    if let Some((w, h)) = s.split_once('/') {
        let w = w.trim().parse::<f32>().ok()?;
        let h = h.trim().parse::<f32>().ok()?;
        if h == 0.0 {
            return None;
        }
        return Some(w / h);
    }
    s.parse::<f32>().ok()
}

/// The node's [`Z_INDEX`] paint order (default `0`). Accepts a signed integer (`"-1"`,
/// `"10"`); higher paints later (on top). Unset or unparseable -> `0`. Parsed via
/// [`signed_px`] (which tolerates a leading `-`) then truncated to an `i32`.
fn style_z_index(dom: &Dom, id: NodeId) -> i32 {
    dom.style(id, Z_INDEX)
        .and_then(signed_px)
        .map(|n| n as i32)
        .unwrap_or(0)
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
    // A text leaf's cell height is its `font-size` when set, decoupling glyph size from
    // the box `height`: `font-size: 24` renders 24px text even in a shorter/auto box.
    // Falls back to the legacy `height`-derived size (then `TEXT_HEIGHT`) so sheets with
    // no `font-size` are byte-for-byte unchanged.
    let h = style_px(dom, id, FONT_SIZE)
        .or_else(|| style_px(dom, id, HEIGHT))
        .unwrap_or(TEXT_HEIGHT) as f32;
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

/// Resolve one **border** edge into a Taffy [`LengthPercentage`] width. The border now
/// participates in the box model (it insets content, not just paints on top), so its
/// geometry is fed to Taffy here: the per-side longhand (`side`, e.g. [`BORDER_TOP_WIDTH`])
/// if set, else the uniform [`BORDER_WIDTH`], else `0`. Widths are non-negative px.
fn border_edge(dom: &Dom, id: NodeId, side: PropId) -> LengthPercentage {
    let w = style_px(dom, id, side)
        .or_else(|| style_px(dom, id, BORDER_WIDTH))
        .unwrap_or(0);
    LengthPercentage::length(w as f32)
}

/// Build the Taffy [`Style`] for one element from its inline styles.
fn element_style(dom: &Dom, id: NodeId) -> Style {
    let dir = match dom.style(id, DIRECTION) {
        Some("row") => FlexDirection::Row,
        _ => FlexDirection::Column,
    };
    // Gap: per-axis ROW_GAP (cross/block -> `gap.height`) and COLUMN_GAP (main/inline ->
    // `gap.width`) win when present; the uniform GAP shorthand fills whichever axis the
    // per-axis longhand leaves unset. Default 0 on both.
    let uniform_gap = style_px(dom, id, GAP).unwrap_or(0) as f32;
    let row_gap = style_px(dom, id, ROW_GAP)
        .map(|n| n as f32)
        .unwrap_or(uniform_gap);
    let column_gap = style_px(dom, id, COLUMN_GAP)
        .map(|n| n as f32)
        .unwrap_or(uniform_gap);
    // Width/height accept a percentage ("100%"/"50%"), a length ("Npx"/"N"), or are
    // auto (content-sized) when absent — see `style_dimension`.
    let width = style_dimension(dom, id, WIDTH);
    let height = style_dimension(dom, id, HEIGHT);
    // min/max constraints reuse `style_dimension`, which returns `Dimension::auto()` when
    // absent — exactly Taffy's "no constraint" value for an unset min/max bound.
    let min_width = style_dimension(dom, id, MIN_WIDTH);
    let min_height = style_dimension(dom, id, MIN_HEIGHT);
    let max_width = style_dimension(dom, id, MAX_WIDTH);
    let max_height = style_dimension(dom, id, MAX_HEIGHT);
    // `flex-grow`/`flex-shrink` are unitless and may be fractional ("1", "0.5"), so they
    // are read raw and parsed as f32. flex-grow defaults to 0.0 (does not grow); Taffy's
    // own flex-shrink default is 1.0, which `style_f32` returns when unset.
    let flex_grow = dom
        .style(id, FLEX_GROW)
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.0);
    let flex_shrink = style_f32(dom, id, FLEX_SHRINK, 1.0);
    // `flex-basis` accepts px / % / auto exactly like width (Dimension::auto by default).
    let flex_basis = style_dimension(dom, id, FLEX_BASIS);
    // UA-stylesheet default: a button/input centers its label on BOTH axes (the cross axis via
    // align-items, the main axis via justify-content) unless the author sets align/justify,
    // mirroring how browsers center `<button>` content. Plain containers keep Taffy's defaults
    // (stretch / flex-start). This is what seats a label in the middle of a taller button without
    // any per-node align/justify — the glyph is then vertically centered within that box by
    // canopy_render_soft, so font scale (driven by the run height) is unaffected.
    let centers_content = matches!(
        dom.node(id).and_then(|n| n.tag),
        Some(tag) if tag == ElementTag::new(EL_BUTTON) || tag == ElementTag::new(EL_INPUT)
    );
    let align_items = style_align(dom, id).or(centers_content.then_some(AlignItems::CENTER));
    let justify_content =
        style_justify(dom, id).or(centers_content.then_some(JustifyContent::CENTER));
    Style {
        display: style_display(dom, id),
        position: style_position(dom, id),
        box_sizing: style_box_sizing(dom, id),
        flex_direction: dir,
        flex_wrap: style_flex_wrap(dom, id),
        align_items,
        // `align-self` overrides the parent's `align-items` for this one item; `None`
        // (unset) falls back to the container's alignment, which is Taffy's default.
        align_self: style_align_self(dom, id),
        justify_content,
        flex_grow,
        flex_shrink,
        flex_basis,
        aspect_ratio: style_aspect_ratio(dom, id),
        gap: TaffySize {
            width: length(column_gap),
            height: length(row_gap),
        },
        // Per-edge padding: per-side longhand (PADDING_*) else uniform PADDING else 0.
        // Padding is `LengthPercentage` (non-negative, no `auto`).
        padding: TaffyRect {
            left: padding_edge(dom, id, PADDING_LEFT),
            right: padding_edge(dom, id, PADDING_RIGHT),
            top: padding_edge(dom, id, PADDING_TOP),
            bottom: padding_edge(dom, id, PADDING_BOTTOM),
        },
        // Per-edge margin: per-side longhand (MARGIN_*) else uniform MARGIN else 0.
        // Margin is `LengthPercentageAuto` — it admits `auto` (`margin: 0 auto`
        // centering, `margin-left: auto`) and negative px.
        margin: TaffyRect {
            left: auto_edge(
                dom,
                id,
                MARGIN_LEFT,
                Some(MARGIN),
                LengthPercentageAuto::length(0.0),
            ),
            right: auto_edge(
                dom,
                id,
                MARGIN_RIGHT,
                Some(MARGIN),
                LengthPercentageAuto::length(0.0),
            ),
            top: auto_edge(
                dom,
                id,
                MARGIN_TOP,
                Some(MARGIN),
                LengthPercentageAuto::length(0.0),
            ),
            bottom: auto_edge(
                dom,
                id,
                MARGIN_BOTTOM,
                Some(MARGIN),
                LengthPercentageAuto::length(0.0),
            ),
        },
        // Border participates in the box model: feed its width to Taffy so it insets
        // content. Per-side longhand (BORDER_*_WIDTH) else uniform BORDER_WIDTH else 0.
        // The frame's STYLE/color rendering stays in `push_border`, now consistent with
        // this inset. Inset (for `position: absolute`/`relative` offset): no shorthand,
        // so the fallback is `None` and each unset edge is `auto`.
        border: TaffyRect {
            left: border_edge(dom, id, BORDER_LEFT_WIDTH),
            right: border_edge(dom, id, BORDER_RIGHT_WIDTH),
            top: border_edge(dom, id, BORDER_TOP_WIDTH),
            bottom: border_edge(dom, id, BORDER_BOTTOM_WIDTH),
        },
        inset: TaffyRect {
            left: auto_edge(dom, id, INSET_LEFT, None, LengthPercentageAuto::auto()),
            right: auto_edge(dom, id, INSET_RIGHT, None, LengthPercentageAuto::auto()),
            top: auto_edge(dom, id, INSET_TOP, None, LengthPercentageAuto::auto()),
            bottom: auto_edge(dom, id, INSET_BOTTOM, None, LengthPercentageAuto::auto()),
        },
        size: TaffySize { width, height },
        min_size: TaffySize {
            width: min_width,
            height: min_height,
        },
        max_size: TaffySize {
            width: max_width,
            height: max_height,
        },
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
    // `display: none` removes the node AND its subtree from paint and hit-test: Taffy
    // already zero-sizes it, and we prune it here so it contributes no rect (no phantom
    // zero-area fill) and we do not descend into its children. This is the "skips subtree"
    // half of the contract; the layout half is Taffy's zero-sizing.
    if id.is_some_and(|id| style_display(dom, id) == Display::None) {
        return;
    }
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
        // `visibility` and `z-index` are per-node (not inherited / not accumulated):
        // a hidden node still lays out its visible children, and each node carries its
        // own paint order.
        let visible = style_visible(dom, id);
        let z_index = style_z_index(dom, id);
        rects.push((id, rect));
        paints.push(NodePaint {
            opacity,
            fg,
            align,
            visible,
            z_index,
        });
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
///
/// Paint order is the rects' tree order **re-sorted by [`Z_INDEX`]**: a higher z-index
/// paints later (on top), and the sort is *stable* so equal-z nodes keep tree order
/// (parents before children, earlier siblings first). Most scenes have all-zero z, so
/// the stable sort is a no-op and the output is byte-for-byte the old tree order.
///
/// A node with `visibility: hidden` ([`NodePaint::visible`] == `false`) contributes no
/// primitives of its own — its background, border, and text are skipped — but it stays
/// in the list position-wise so its (visible) children, processed via their own entries,
/// still paint.
fn build_display_list(
    dom: &Dom,
    rects: &[(NodeId, Rect)],
    paints: &[NodePaint],
) -> Vec<DisplayItem> {
    // Stable paint order by z-index: collect the indices, then sort by the node's
    // `z_index` only. A stable sort leaves equal-z entries in their original (tree) order,
    // so tree order breaks ties — and an all-zero scene is unchanged.
    let mut order: Vec<usize> = (0..rects.len()).collect();
    order.sort_by_key(|&i| paints.get(i).map(|p| p.z_index).unwrap_or(0));

    let mut items = Vec::new();
    for &i in &order {
        let (id, rect) = rects[i];
        let Some(node) = dom.node(id) else { continue };
        // Parallel vecs are built together in `collect_rects`, so the index is always
        // valid; default to opaque/inherited-light/visible if a caller ever passes a
        // short slice.
        let paint = paints.get(i).copied().unwrap_or(NodePaint {
            opacity: 1.0,
            fg: DEFAULT_FG,
            align: 0.0,
            visible: true,
            z_index: 0,
        });
        // `visibility: hidden` lays out but paints nothing of its own; children still
        // emit through their own list entries.
        if !paint.visible {
            continue;
        }
        // Per-node paint order, back to front: box-shadow (behind the box), the background
        // fill (`Rect`), the background gradient (over the solid fill), the border frame,
        // then — for a text leaf — the text run and its decoration. The outline lands last
        // (on top of everything this node draws). The shared parts are factored out so the
        // text and non-text branches stay in lockstep.
        push_shadow(dom, id, rect, paint.opacity, &mut items);
        // `background` is NOT inherited (per-element), so it is read off this node.
        if let Some(bg) = style_color(dom, id, BG) {
            items.push(DisplayItem::Rect {
                rect,
                color: fade(bg, paint.opacity),
                radius: style_radius(dom, id),
            });
        }
        push_gradient(dom, id, rect, paint.opacity, &mut items);
        push_border(dom, id, rect, paint.opacity, &mut items);
        if let Some(text) = node.text.as_deref() {
            // The text `color` IS inherited (resolved in the tree walk -> `paint.fg`).
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
            // The decoration line rides on the text run's resolved (inherited) ink color and
            // is emitted just after the run, so it paints over the glyph cell.
            push_text_decoration(dom, id, rect, fade(paint.fg, paint.opacity), &mut items);
        }
        // Outline is paint-only and on top of everything this node drew.
        push_outline(dom, id, rect, paint.opacity, &mut items);
    }
    items
}

/// Emit a [`DisplayItem::Border`] frame for `id` over `rect` when the node sets a positive
/// [`BORDER_WIDTH`] *and* a parseable [`BORDER_COLOR`]; otherwise emit nothing.
///
/// The border is **paint-only** — it never altered the Taffy geometry, so `rect` is the
/// node's laid-out box and the frame is stroked inside it (the renderer's `stroke_rect`).
/// It is pushed *after* the node's background `Rect` so the frame draws on top, and its
/// color is [`fade`]d by the node's resolved `opacity` exactly like the background fill,
/// so a translucent subtree fades its borders in lockstep with its fills. The corner
/// radius is the node's own [`RADIUS`], matching the rounded fill it frames.
fn push_border(dom: &Dom, id: NodeId, rect: Rect, opacity: f32, items: &mut Vec<DisplayItem>) {
    let width = style_px(dom, id, BORDER_WIDTH).unwrap_or(0);
    if width == 0 {
        return;
    }
    let Some(color) = style_color(dom, id, BORDER_COLOR) else {
        return;
    };
    items.push(DisplayItem::Border {
        rect,
        color: fade(color, opacity),
        width: width as f32,
        radius: style_radius(dom, id),
    });
}

/// Emit a [`DisplayItem::Shadow`] for `id`'s [`BOX_SHADOW`] over `rect` (the node's
/// border-box), or nothing when the property is unset/unparseable.
///
/// The shadow is pushed **first** in the node's paint sequence so it sits *behind* the box.
/// The frozen value is `"<dx> <dy> <blur> <#hex>"` (no spread/inset); its color is [`fade`]d
/// by the node's resolved opacity, matching how the fill and border fade. `rect` is the
/// border-box; the renderer offsets it by `offset` and feathers it by `blur`.
fn push_shadow(dom: &Dom, id: NodeId, rect: Rect, opacity: f32, items: &mut Vec<DisplayItem>) {
    let Some(shadow) = dom.style(id, BOX_SHADOW).and_then(parse_box_shadow) else {
        return;
    };
    items.push(DisplayItem::Shadow {
        rect,
        color: fade(shadow.color, opacity),
        blur: shadow.blur,
        offset: shadow.offset,
    });
}

/// Emit a [`DisplayItem::Gradient`] for `id`'s [`BACKGROUND_IMAGE`] over `rect`, or nothing
/// when the property is unset/unparseable.
///
/// Pushed **just after** the background `Rect` so it paints over the solid fill (a CSS
/// `background-image` layers in front of `background-color`). The frozen value is
/// `"linear-gradient(<deg>, <#hex>[, <#hex>...])"`; stops are spaced evenly across the axis
/// and each stop's color is [`fade`]d by the node's resolved opacity so a translucent subtree
/// fades the gradient with its other fills.
fn push_gradient(dom: &Dom, id: NodeId, rect: Rect, opacity: f32, items: &mut Vec<DisplayItem>) {
    let Some((stops, direction)) = dom
        .style(id, BACKGROUND_IMAGE)
        .and_then(parse_linear_gradient)
    else {
        return;
    };
    // Fade each stop's color by the node opacity, matching the fill/border treatment.
    let faded: Vec<GradientStop> = stops
        .as_slice()
        .iter()
        .map(|s| GradientStop {
            color: fade(s.color, opacity),
            position: s.position,
        })
        .collect();
    items.push(DisplayItem::Gradient {
        rect,
        stops: GradientStops::from_slice(&faded),
        direction,
    });
}

/// Emit a thin [`DisplayItem::Rect`] for `id`'s [`TEXT_DECORATION`] (`underline` /
/// `line-through`) across the text run, or nothing for `none`/unset/an unknown keyword.
///
/// `rect` is the text leaf's laid-out box and `color` the (already-faded) ink. The line
/// spans the box width with thickness `max(1, font_size / 16)`, where `font_size` is the
/// same height the text run uses (`rect.size.h`). An `underline` sits near the bottom of the
/// cell (one thickness above the baseline edge); a `line-through` sits at the vertical
/// middle. Pushed just after the text run so it paints over the glyphs.
fn push_text_decoration(
    dom: &Dom,
    id: NodeId,
    rect: Rect,
    color: Color,
    items: &mut Vec<DisplayItem>,
) {
    let kind = match dom.style(id, TEXT_DECORATION) {
        Some("underline") => Decoration::Underline,
        Some("line-through") => Decoration::LineThrough,
        // `none`, unset, or an unrecognized keyword draws no line.
        _ => return,
    };
    let font_size = rect.size.h;
    // Thickness scales with the font size (a 16px run -> 1px), floored at 1px so the line is
    // never invisible. Integer-friendly: cast-truncate, no `f32::floor` (std-only).
    let thickness = ((font_size / 16.0) as u32).max(1) as f32;
    let y = match kind {
        // Underline: near the bottom of the cell, one thickness up from the bottom edge so
        // the full line stays inside the box.
        Decoration::Underline => rect.origin.y + font_size - thickness,
        // Line-through: centered vertically through the run.
        Decoration::LineThrough => rect.origin.y + (font_size - thickness) / 2.0,
    };
    items.push(DisplayItem::Rect {
        rect: Rect {
            origin: Point {
                x: rect.origin.x,
                y,
            },
            size: Size {
                w: rect.size.w,
                h: thickness,
            },
        },
        color,
        radius: 0.0,
    });
}

/// Which line a [`TEXT_DECORATION`] draws.
enum Decoration {
    /// A line near the bottom of the text cell.
    Underline,
    /// A line through the vertical middle of the text cell.
    LineThrough,
}

/// Emit a [`DisplayItem::Border`] for `id`'s outline ([`OUTLINE_WIDTH`] / [`OUTLINE_COLOR`] /
/// [`OUTLINE_OFFSET`]) over `rect` inflated by the offset, or nothing when the width is `0`.
///
/// The outline is **paint-only** (it never affects layout) and pushed **last** in the node's
/// sequence so it sits on top of everything else this node drew — matching CSS `outline`,
/// which is drawn outside the border box and over neighboring content. `rect` (the
/// border-box) is inflated by [`OUTLINE_OFFSET`] (which may be negative, pulling the outline
/// inward) on all four sides; the stroke uses the node's own [`RADIUS`]. The color is
/// [`fade`]d by the node's resolved opacity. A missing/unparseable color defaults to the
/// node's [`FG`] ink, then [`DEFAULT_FG`], mirroring CSS `outline-color: currentColor`.
fn push_outline(dom: &Dom, id: NodeId, rect: Rect, opacity: f32, items: &mut Vec<DisplayItem>) {
    let width = style_px(dom, id, OUTLINE_WIDTH).unwrap_or(0);
    if width == 0 {
        return;
    }
    // CSS `outline-color` defaults to `currentColor`; fall back to the node's own `color`
    // then the crate default so an outline with no explicit color still strokes.
    let color = style_color(dom, id, OUTLINE_COLOR)
        .or_else(|| style_color(dom, id, FG))
        .unwrap_or(DEFAULT_FG);
    // `outline-offset` is a signed px gap; a negative value pulls the outline inside the box.
    let offset = dom
        .style(id, OUTLINE_OFFSET)
        .and_then(signed_px)
        .unwrap_or(0.0);
    let inflated = Rect {
        origin: Point {
            x: rect.origin.x - offset,
            y: rect.origin.y - offset,
        },
        size: Size {
            w: rect.size.w + offset * 2.0,
            h: rect.size.h + offset * 2.0,
        },
    };
    items.push(DisplayItem::Border {
        rect: inflated,
        color: fade(color, opacity),
        width: width as f32,
        radius: style_radius(dom, id),
    });
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
    fn a_button_centers_its_label_by_default() {
        // UA-stylesheet default: a button (tag 3) centers its label on both axes with NO explicit
        // align/justify — the common case the lite tier should "just work".
        let mut e = Emitter::new();
        let btn = e.create_element(ElementTag::new(3)); // button
        e.append(ROOT, btn);
        e.set_inline_style(btn, WIDTH, "100");
        e.set_inline_style(btn, HEIGHT, "60");
        let label = e.create_text("ok"); // 2 chars -> 32x16 box (advance 16)
        e.append(btn, label);
        let dom = dom_from(e);

        let (_scene, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let btn_r = lay.rects.iter().find(|(id, _)| *id == btn).unwrap().1;
        let lbl_r = lay.rects.iter().find(|(id, _)| *id == label).unwrap().1;
        let lbl_cx = lbl_r.origin.x + lbl_r.size.w / 2.0;
        let lbl_cy = lbl_r.origin.y + lbl_r.size.h / 2.0;
        let btn_cx = btn_r.origin.x + btn_r.size.w / 2.0;
        let btn_cy = btn_r.origin.y + btn_r.size.h / 2.0;
        assert!(
            (lbl_cx - btn_cx).abs() < 0.5,
            "label centered horizontally ({lbl_cx} vs {btn_cx})"
        );
        assert!(
            (lbl_cy - btn_cy).abs() < 0.5,
            "label centered vertically ({lbl_cy} vs {btn_cy})"
        );
    }

    #[test]
    fn a_plain_column_leaves_its_child_at_the_start() {
        // The default is scoped to buttons/inputs: a plain column keeps Taffy's flex-start, so the
        // child stays at the top-left (no surprise centering of arbitrary containers).
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1)); // column
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "100");
        e.set_inline_style(col, HEIGHT, "60");
        let label = e.create_text("ok");
        e.append(col, label);
        let dom = dom_from(e);

        let (_scene, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let lbl_r = lay.rects.iter().find(|(id, _)| *id == label).unwrap().1;
        assert_eq!(
            lbl_r.origin,
            Point { x: 0.0, y: 0.0 },
            "plain column: child at the start"
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
    fn margin_offsets_a_node_and_its_sibling() {
        // A row of two boxes where the first carries a uniform margin of 10. The margin
        // insets the first box from the row's top-left, and pushes the second box right
        // by the first box's width PLUS both boxes' touching margins (10 right of A +
        // 10 left of B = 20), so the sibling visibly shifts because of the margin.
        let mut e = Emitter::new();
        let row = e.create_element(ElementTag::new(1));
        e.append(ROOT, row);
        e.set_inline_style(row, DIRECTION, "row");
        let a = e.create_element(ElementTag::new(2));
        e.append(row, a);
        e.set_inline_style(a, WIDTH, "30");
        e.set_inline_style(a, HEIGHT, "20");
        e.set_inline_style(a, MARGIN, "10");
        let b = e.create_element(ElementTag::new(2));
        e.append(row, b);
        e.set_inline_style(b, WIDTH, "40");
        e.set_inline_style(b, HEIGHT, "20");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 300.0, h: 100.0 });

        let a_rect = lay.rects.iter().find(|(id, _)| *id == a).unwrap().1;
        let b_rect = lay.rects.iter().find(|(id, _)| *id == b).unwrap().1;
        // A is inset from the row origin by its own margin on both axes.
        assert_eq!(
            a_rect.origin,
            Point { x: 10.0, y: 10.0 },
            "margin insets the node from the container edge"
        );
        // Only A carries a margin, so B starts past A's right margin: 10 (A left margin)
        // + 30 (A width) + 10 (A right margin) = 50. Without A's margin B would sit at 30,
        // so the margin visibly shifts the sibling by 20.
        assert_eq!(
            b_rect.origin.x, 50.0,
            "the margin pushes the sibling further along the main axis"
        );
    }

    #[test]
    fn min_and_max_width_clamp_the_box() {
        // min-width raises a smaller box UP to the floor; max-width lowers a larger box
        // DOWN to the ceiling. Two children in a column, each given an explicit width that
        // its min/max then overrides.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        // Small box: width 20, but min-width 50 -> clamps UP to 50.
        let small = e.create_element(ElementTag::new(2));
        e.append(col, small);
        e.set_inline_style(small, WIDTH, "20");
        e.set_inline_style(small, HEIGHT, "10");
        e.set_inline_style(small, MIN_WIDTH, "50");
        // Big box: width 200, but max-width 80 -> clamps DOWN to 80.
        let big = e.create_element(ElementTag::new(2));
        e.append(col, big);
        e.set_inline_style(big, WIDTH, "200");
        e.set_inline_style(big, HEIGHT, "10");
        e.set_inline_style(big, MAX_WIDTH, "80");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 400.0, h: 200.0 });

        let small_rect = lay.rects.iter().find(|(id, _)| *id == small).unwrap().1;
        let big_rect = lay.rects.iter().find(|(id, _)| *id == big).unwrap().1;
        assert_eq!(
            small_rect.size.w, 50.0,
            "min-width clamps a smaller box up to the floor"
        );
        assert_eq!(
            big_rect.size.w, 80.0,
            "max-width clamps a larger box down to the ceiling"
        );
    }

    #[test]
    fn flex_grow_splits_free_main_axis_space() {
        // A 200-wide row with two `flex-grow: 1` children and no explicit widths: they
        // split the free main-axis space evenly, 100 each.
        let mut e = Emitter::new();
        let row = e.create_element(ElementTag::new(1));
        e.append(ROOT, row);
        e.set_inline_style(row, DIRECTION, "row");
        e.set_inline_style(row, WIDTH, "200");
        e.set_inline_style(row, HEIGHT, "20");
        let a = e.create_element(ElementTag::new(2));
        e.append(row, a);
        e.set_inline_style(a, HEIGHT, "20");
        e.set_inline_style(a, FLEX_GROW, "1");
        let b = e.create_element(ElementTag::new(2));
        e.append(row, b);
        e.set_inline_style(b, HEIGHT, "20");
        e.set_inline_style(b, FLEX_GROW, "1");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });

        let a_rect = lay.rects.iter().find(|(id, _)| *id == a).unwrap().1;
        let b_rect = lay.rects.iter().find(|(id, _)| *id == b).unwrap().1;
        assert_eq!(
            a_rect.size.w, 100.0,
            "first grow:1 child takes half the row"
        );
        assert_eq!(
            b_rect.size.w, 100.0,
            "second grow:1 child takes the other half"
        );
        // The second child starts exactly where the first ends — they tile the row.
        assert_eq!(b_rect.origin.x, 100.0);
    }

    #[test]
    fn border_width_and_color_emit_a_border_item() {
        // A node with `border-width` + `border-color` emits a `DisplayItem::Border`
        // carrying the stroke width, the parsed color, and the node's radius — and it is
        // emitted AFTER the node's own background fill so the frame draws on top.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, BG, "#202020");
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, RADIUS, "6");
        e.set_inline_style(card, BORDER_WIDTH, "3");
        e.set_inline_style(card, BORDER_COLOR, "#89b4fa");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });

        let border = scene.items.iter().find_map(|i| match i {
            DisplayItem::Border {
                width,
                color,
                radius,
                ..
            } => Some((*width, *color, *radius)),
            _ => None,
        });
        let (width, color, radius) = border.expect("a Border item is in the scene");
        assert_eq!(width, 3.0, "border width rides onto the Border item");
        assert_eq!(
            color,
            Color {
                r: 0x89,
                g: 0xb4,
                b: 0xfa,
                a: 255
            },
            "border color is the parsed #rrggbb"
        );
        assert_eq!(
            radius, 6.0,
            "border radius matches the node's corner radius"
        );

        // The fill is emitted before the frame so the border draws on top.
        let bg_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Rect { .. }))
            .unwrap();
        let border_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Border { .. }))
            .unwrap();
        assert!(
            bg_idx < border_idx,
            "the border frame paints on top of the fill"
        );
    }

    #[test]
    fn border_with_zero_width_or_no_color_emits_nothing() {
        // Border is conditional: a zero width (or a missing/unparseable color) emits no
        // Border item — it must not paint a phantom frame.
        let mut e = Emitter::new();
        // width 0 + a color -> no border.
        let a = e.create_element(ElementTag::new(1));
        e.append(ROOT, a);
        e.set_inline_style(a, WIDTH, "20");
        e.set_inline_style(a, HEIGHT, "20");
        e.set_inline_style(a, BORDER_WIDTH, "0");
        e.set_inline_style(a, BORDER_COLOR, "#ffffff");
        // a positive width but NO color -> no border.
        let b = e.create_element(ElementTag::new(1));
        e.append(ROOT, b);
        e.set_inline_style(b, WIDTH, "20");
        e.set_inline_style(b, HEIGHT, "20");
        e.set_inline_style(b, BORDER_WIDTH, "2");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });
        let borders = scene
            .items
            .iter()
            .filter(|i| matches!(i, DisplayItem::Border { .. }))
            .count();
        assert_eq!(borders, 0, "no border without both a width and a color");
    }

    #[test]
    fn border_color_fades_with_opacity() {
        // The border color is faded by the node's resolved opacity, exactly like the
        // background fill — so a 0.5-opacity node strokes a half-alpha frame.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, OPACITY, "0.5");
        e.set_inline_style(card, BORDER_WIDTH, "2");
        e.set_inline_style(card, BORDER_COLOR, "#ffffff");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });
        let color = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Border { color, .. } => Some(*color),
                _ => None,
            })
            .expect("a Border item");
        // 255 * 0.5 rounded = 128; RGB untouched (straight-alpha fade).
        assert_eq!(
            color.a, 128,
            "the border alpha is scaled by the node opacity"
        );
        assert_eq!((color.r, color.g, color.b), (0xff, 0xff, 0xff));
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

    #[test]
    fn per_side_padding_insets_each_edge_independently() {
        // padding-left/top differ from padding-right/bottom: the child's origin reflects
        // the top-left longhands exactly, independent of the others.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "100");
        e.set_inline_style(col, HEIGHT, "100");
        e.set_inline_style(col, PADDING_LEFT, "7");
        e.set_inline_style(col, PADDING_TOP, "11");
        e.set_inline_style(col, PADDING_RIGHT, "3");
        e.set_inline_style(col, PADDING_BOTTOM, "5");
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, WIDTH, "20");
        e.set_inline_style(child, HEIGHT, "10");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 200.0 });
        let child_rect = lay.rects.iter().find(|(id, _)| *id == child).unwrap().1;
        assert_eq!(
            child_rect.origin,
            Point { x: 7.0, y: 11.0 },
            "child is inset by the top/left padding longhands, not the uniform value"
        );
    }

    #[test]
    fn per_side_longhand_overrides_uniform_shorthand() {
        // The uniform PADDING shorthand applies to the edges with no longhand; a longhand
        // on one edge overrides only that edge.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "100");
        e.set_inline_style(col, HEIGHT, "100");
        e.set_inline_style(col, PADDING, "4"); // base for every edge
        e.set_inline_style(col, PADDING_LEFT, "12"); // override left only
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, WIDTH, "10");
        e.set_inline_style(child, HEIGHT, "10");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 200.0 });
        let child_rect = lay.rects.iter().find(|(id, _)| *id == child).unwrap().1;
        // left = 12 (longhand), top = 4 (uniform fallback).
        assert_eq!(child_rect.origin, Point { x: 12.0, y: 4.0 });
    }

    #[test]
    fn per_side_margin_offsets_one_edge() {
        // A single margin-left longhand insets the box from the container's left edge,
        // without the uniform shorthand.
        let mut e = Emitter::new();
        let row = e.create_element(ElementTag::new(1));
        e.append(ROOT, row);
        e.set_inline_style(row, DIRECTION, "row");
        let a = e.create_element(ElementTag::new(2));
        e.append(row, a);
        e.set_inline_style(a, WIDTH, "20");
        e.set_inline_style(a, HEIGHT, "20");
        e.set_inline_style(a, MARGIN_LEFT, "15");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let a_rect = lay.rects.iter().find(|(id, _)| *id == a).unwrap().1;
        assert_eq!(
            a_rect.origin,
            Point { x: 15.0, y: 0.0 },
            "margin-left longhand pushes the box right; top is untouched"
        );
    }

    #[test]
    fn margin_auto_centers_a_box_horizontally() {
        // `margin: 0 auto` (here margin-left:auto + margin-right:auto) on a fixed-width box
        // in a wider container centers it: the two auto margins split the free space.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "200");
        e.set_inline_style(col, HEIGHT, "100");
        let box_ = e.create_element(ElementTag::new(2));
        e.append(col, box_);
        e.set_inline_style(box_, WIDTH, "40");
        e.set_inline_style(box_, HEIGHT, "20");
        e.set_inline_style(box_, MARGIN_LEFT, "auto");
        e.set_inline_style(box_, MARGIN_RIGHT, "auto");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let box_rect = lay.rects.iter().find(|(id, _)| *id == box_).unwrap().1;
        // (200 - 40) / 2 = 80.
        assert_eq!(
            box_rect.origin.x, 80.0,
            "auto left+right margins center the box in its container"
        );
    }

    #[test]
    fn negative_margin_pulls_a_box_left() {
        // A negative margin-left pulls the box outward (left), past the container origin.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, DIRECTION, "row");
        e.set_inline_style(col, WIDTH, "200");
        e.set_inline_style(col, HEIGHT, "40");
        let box_ = e.create_element(ElementTag::new(2));
        e.append(col, box_);
        e.set_inline_style(box_, WIDTH, "20");
        e.set_inline_style(box_, HEIGHT, "20");
        e.set_inline_style(box_, MARGIN_LEFT, "-10");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let box_rect = lay.rects.iter().find(|(id, _)| *id == box_).unwrap().1;
        assert_eq!(box_rect.origin.x, -10.0, "negative margin-left pulls left");
    }

    #[test]
    fn display_none_zero_sizes_and_skips_the_subtree() {
        // `display: none` collapses the node to a zero box AND skips its subtree: neither
        // the node nor its child contributes a non-empty rect, and a following sibling
        // stacks as if the none node were not there.
        let mut e = Emitter::new();
        let gone = e.create_element(ElementTag::new(1));
        e.append(ROOT, gone);
        e.set_inline_style(gone, WIDTH, "50");
        e.set_inline_style(gone, HEIGHT, "50");
        e.set_inline_style(gone, DISPLAY, "none");
        e.set_inline_style(gone, BG, "#ff0000");
        let child = e.create_element(ElementTag::new(2));
        e.append(gone, child);
        e.set_inline_style(child, WIDTH, "30");
        e.set_inline_style(child, HEIGHT, "30");

        let dom = dom_from(e);
        let (scene, lay) = layout(&dom, Size { w: 200.0, h: 200.0 });
        // Taffy zero-sizes a display:none node; we then prune it AND its subtree from the
        // rects, so neither the node nor its child appears (no paint, no hit-test target).
        assert!(
            lay.rects.iter().all(|(id, _)| *id != gone),
            "display:none prunes the node from paint/hit-test"
        );
        assert!(
            lay.rects.iter().all(|(id, _)| *id != child),
            "display:none skips the subtree (the child is not laid out either)"
        );
        // No red fill is emitted for the none node.
        let has_red = scene.items.iter().any(|i| {
            matches!(i, DisplayItem::Rect { color, .. }
                if (color.r, color.g, color.b) == (0xff, 0x00, 0x00))
        });
        assert!(!has_red, "display:none emits no background");
    }

    #[test]
    fn visibility_hidden_lays_out_but_paints_nothing_itself() {
        // `visibility: hidden` keeps the node in layout (its box still reserves space and
        // its child still paints) but suppresses the node's own background.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "100");
        e.set_inline_style(col, HEIGHT, "100");
        let hidden = e.create_element(ElementTag::new(2));
        e.append(col, hidden);
        e.set_inline_style(hidden, WIDTH, "40");
        e.set_inline_style(hidden, HEIGHT, "40");
        e.set_inline_style(hidden, VISIBILITY, "hidden");
        e.set_inline_style(hidden, BG, "#ff0000"); // would-be parent fill, suppressed
        let kid = e.create_element(ElementTag::new(3));
        e.append(hidden, kid);
        e.set_inline_style(kid, WIDTH, "10");
        e.set_inline_style(kid, HEIGHT, "10");
        e.set_inline_style(kid, BG, "#00ff00"); // visible child fill, still painted

        let dom = dom_from(e);
        let (scene, lay) = layout(&dom, Size { w: 200.0, h: 200.0 });
        // The hidden node is still laid out at its full size.
        let hidden_rect = lay.rects.iter().find(|(id, _)| *id == hidden).unwrap().1;
        assert_eq!(
            hidden_rect.size,
            Size { w: 40.0, h: 40.0 },
            "visibility:hidden still lays the node out"
        );
        // Its own red fill is suppressed...
        let has_red = scene.items.iter().any(|i| {
            matches!(i, DisplayItem::Rect { color, .. }
                if (color.r, color.g, color.b) == (0xff, 0x00, 0x00))
        });
        assert!(!has_red, "the hidden node's own background is not emitted");
        // ...but the visible child still paints its green fill.
        let has_green = scene.items.iter().any(|i| {
            matches!(i, DisplayItem::Rect { color, .. }
                if (color.r, color.g, color.b) == (0x00, 0xff, 0x00))
        });
        assert!(has_green, "children of a hidden node still paint");
    }

    #[test]
    fn position_absolute_with_inset_offsets_from_the_container() {
        // An absolutely-positioned child with top/left insets is placed at those offsets
        // relative to its (relatively-positioned) container, out of the normal flow.
        let mut e = Emitter::new();
        let container = e.create_element(ElementTag::new(1));
        e.append(ROOT, container);
        e.set_inline_style(container, WIDTH, "200");
        e.set_inline_style(container, HEIGHT, "200");
        e.set_inline_style(container, POSITION, "relative");
        let abs = e.create_element(ElementTag::new(2));
        e.append(container, abs);
        e.set_inline_style(abs, WIDTH, "30");
        e.set_inline_style(abs, HEIGHT, "30");
        e.set_inline_style(abs, POSITION, "absolute");
        e.set_inline_style(abs, INSET_LEFT, "25");
        e.set_inline_style(abs, INSET_TOP, "40");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 400.0, h: 400.0 });
        let abs_rect = lay.rects.iter().find(|(id, _)| *id == abs).unwrap().1;
        assert_eq!(
            abs_rect.origin,
            Point { x: 25.0, y: 40.0 },
            "absolute + inset places the box at top/left within the container"
        );
        assert_eq!(abs_rect.size, Size { w: 30.0, h: 30.0 });
    }

    #[test]
    fn flex_wrap_pushes_overflowing_items_to_a_second_line() {
        // A 100-wide wrapping row with three 40-wide items: two fit on line one (0, 40),
        // and the third wraps to a second line (back to x=0, below the first row).
        let mut e = Emitter::new();
        let row = e.create_element(ElementTag::new(1));
        e.append(ROOT, row);
        e.set_inline_style(row, DIRECTION, "row");
        e.set_inline_style(row, FLEX_WRAP, "wrap");
        e.set_inline_style(row, WIDTH, "100");
        e.set_inline_style(row, HEIGHT, "100");
        let mut ids = Vec::new();
        for _ in 0..3 {
            let b = e.create_element(ElementTag::new(2));
            e.append(row, b);
            e.set_inline_style(b, WIDTH, "40");
            e.set_inline_style(b, HEIGHT, "20");
            ids.push(b);
        }
        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 200.0 });
        let r0 = lay.rects.iter().find(|(id, _)| *id == ids[0]).unwrap().1;
        let r1 = lay.rects.iter().find(|(id, _)| *id == ids[1]).unwrap().1;
        let r2 = lay.rects.iter().find(|(id, _)| *id == ids[2]).unwrap().1;
        assert_eq!(r0.origin.x, 0.0);
        assert_eq!(r1.origin.x, 40.0);
        // Third item wraps: back to the left edge, on a new (lower) line.
        assert_eq!(r2.origin.x, 0.0, "the third item wraps to the next line");
        assert!(
            r2.origin.y > r0.origin.y,
            "the wrapped item is below the first row"
        );
    }

    #[test]
    fn flex_basis_sets_the_main_axis_size() {
        // In a row, a child with `flex-basis: 70` and no explicit width takes 70px on the
        // main axis (flex-basis is the initial main size).
        let mut e = Emitter::new();
        let row = e.create_element(ElementTag::new(1));
        e.append(ROOT, row);
        e.set_inline_style(row, DIRECTION, "row");
        e.set_inline_style(row, WIDTH, "200");
        e.set_inline_style(row, HEIGHT, "40");
        let a = e.create_element(ElementTag::new(2));
        e.append(row, a);
        e.set_inline_style(a, HEIGHT, "20");
        e.set_inline_style(a, FLEX_BASIS, "70");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 400.0, h: 100.0 });
        let a_rect = lay.rects.iter().find(|(id, _)| *id == a).unwrap().1;
        assert_eq!(
            a_rect.size.w, 70.0,
            "flex-basis sets the item's main-axis size"
        );
    }

    #[test]
    fn align_self_overrides_the_containers_align_items() {
        // A column with `align-items: flex-start` and a child that sets `align-self:
        // flex-end`: the child aligns to the cross-axis (right) end, overriding the
        // container default. The 40-wide child in a 200-wide column lands at x=160.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "200");
        e.set_inline_style(col, HEIGHT, "100");
        e.set_inline_style(col, ALIGN, "flex-start");
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, WIDTH, "40");
        e.set_inline_style(child, HEIGHT, "20");
        e.set_inline_style(child, ALIGN_SELF, "flex-end");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 400.0, h: 200.0 });
        let child_rect = lay.rects.iter().find(|(id, _)| *id == child).unwrap().1;
        assert_eq!(
            child_rect.origin.x, 160.0,
            "align-self: flex-end pushes the child to the cross-axis end (200 - 40)"
        );
    }

    #[test]
    fn aspect_ratio_derives_height_from_width() {
        // A box with width 60 and `aspect-ratio: 2` (or "2/1") resolves height to 30
        // (width / ratio). Both the decimal and the w/h forms must agree.
        for value in ["2", "2/1"] {
            let mut e = Emitter::new();
            let col = e.create_element(ElementTag::new(1));
            e.append(ROOT, col);
            let box_ = e.create_element(ElementTag::new(2));
            e.append(col, box_);
            e.set_inline_style(box_, WIDTH, "60");
            e.set_inline_style(box_, ASPECT_RATIO, value);

            let dom = dom_from(e);
            let (_, lay) = layout(&dom, Size { w: 400.0, h: 400.0 });
            let box_rect = lay.rects.iter().find(|(id, _)| *id == box_).unwrap().1;
            assert_eq!(
                box_rect.size,
                Size { w: 60.0, h: 30.0 },
                "aspect-ratio {value:?} -> height = width / 2"
            );
        }
    }

    #[test]
    fn column_gap_spaces_items_on_the_main_axis() {
        // column-gap (the main/inline axis in a row) goes onto `gap.width`: the second item
        // starts at the first item's width PLUS the column-gap.
        let mut e = Emitter::new();
        let row = e.create_element(ElementTag::new(1));
        e.append(ROOT, row);
        e.set_inline_style(row, DIRECTION, "row");
        e.set_inline_style(row, COLUMN_GAP, "8");
        let a = e.create_element(ElementTag::new(2));
        e.append(row, a);
        e.set_inline_style(a, WIDTH, "40");
        e.set_inline_style(a, HEIGHT, "20");
        let b = e.create_element(ElementTag::new(2));
        e.append(row, b);
        e.set_inline_style(b, WIDTH, "20");
        e.set_inline_style(b, HEIGHT, "20");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let b_rect = lay.rects.iter().find(|(id, _)| *id == b).unwrap().1;
        assert_eq!(
            b_rect.origin.x, 48.0,
            "column-gap (40 width + 8 gap) spaces items on the main axis"
        );
    }

    #[test]
    fn row_gap_spaces_wrapped_lines_on_the_cross_axis() {
        // row-gap (the cross/block axis in a row) goes onto `gap.height`: it widens the
        // space between WRAPPED LINES. The row is given a constrained width (to force the
        // wrap) but NO explicit height, so it content-sizes on the cross axis and the
        // lines do not stretch — the wrapped line then sits exactly first-line-height +
        // row-gap below the top.
        let mut e = Emitter::new();
        let row = e.create_element(ElementTag::new(1));
        e.append(ROOT, row);
        e.set_inline_style(row, DIRECTION, "row");
        e.set_inline_style(row, FLEX_WRAP, "wrap");
        e.set_inline_style(row, WIDTH, "100");
        e.set_inline_style(row, ROW_GAP, "12");
        let mut ids = Vec::new();
        for _ in 0..3 {
            let b = e.create_element(ElementTag::new(2));
            e.append(row, b);
            e.set_inline_style(b, WIDTH, "40");
            e.set_inline_style(b, HEIGHT, "20");
            ids.push(b);
        }
        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 300.0 });
        let r0 = lay.rects.iter().find(|(id, _)| *id == ids[0]).unwrap().1;
        let r2 = lay.rects.iter().find(|(id, _)| *id == ids[2]).unwrap().1;
        assert_eq!(
            r2.origin.x, 0.0,
            "the third item wraps back to the left edge"
        );
        // First line height (20) + row-gap (12) = 32.
        assert_eq!(
            r2.origin.y - r0.origin.y,
            32.0,
            "row-gap (12) plus the first line height (20) places the wrapped line"
        );
    }

    #[test]
    fn uniform_gap_still_fills_both_axes_when_no_per_axis_set() {
        // With only the uniform GAP shorthand (no row/column-gap), both axes use it — the
        // existing single-value behavior is preserved.
        let mut e = Emitter::new();
        let row = e.create_element(ElementTag::new(1));
        e.append(ROOT, row);
        e.set_inline_style(row, DIRECTION, "row");
        e.set_inline_style(row, GAP, "10");
        let a = e.create_element(ElementTag::new(2));
        e.append(row, a);
        e.set_inline_style(a, WIDTH, "20");
        e.set_inline_style(a, HEIGHT, "20");
        let b = e.create_element(ElementTag::new(2));
        e.append(row, b);
        e.set_inline_style(b, WIDTH, "20");
        e.set_inline_style(b, HEIGHT, "20");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let b_rect = lay.rects.iter().find(|(id, _)| *id == b).unwrap().1;
        assert_eq!(
            b_rect.origin.x, 30.0,
            "uniform gap (20 width + 10 gap) still applies"
        );
    }

    #[test]
    fn font_size_sizes_a_text_cell_independent_of_height() {
        // A text leaf with `font-size: 24` and no `height` sizes its cell from font-size:
        // height = 24, and width matches the renderer's baked advance at that size
        // (scale = floor(24/8) = 3, advance = 24px/glyph, 2 chars -> 48).
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let t = e.create_text("ab"); // 2 chars
        e.append(col, t);
        e.set_inline_style(t, FONT_SIZE, "24");

        let dom = dom_from(e);
        let (scene, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let t_rect = lay.rects.iter().find(|(id, _)| *id == t).unwrap().1;
        assert_eq!(
            t_rect.size,
            Size { w: 48.0, h: 24.0 },
            "font-size drives the text cell size when height is absent"
        );
        let size = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Text { size, .. } => Some(*size),
                _ => None,
            })
            .expect("text run");
        assert_eq!(size, 24.0, "the run's size is the font-size");
    }

    #[test]
    fn font_size_wins_over_height_for_text_cell() {
        // When both are present, font-size determines the text cell (height is no longer
        // the glyph size): font-size 16 over height 40 -> a 16px run.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let t = e.create_text("x");
        e.append(col, t);
        e.set_inline_style(t, HEIGHT, "40");
        e.set_inline_style(t, FONT_SIZE, "16");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });
        let t_rect = lay.rects.iter().find(|(id, _)| *id == t).unwrap().1;
        assert_eq!(
            t_rect.size.h, 16.0,
            "font-size, not height, drives the text cell height"
        );
    }

    #[test]
    fn border_insets_content_via_the_box_model() {
        // The border now participates in the box model (it is fed to Taffy), so a bordered
        // container insets its child by the border width — not just paints a frame on top.
        // A 100x100 column with a 6px border places its child at (6, 6).
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "100");
        e.set_inline_style(col, HEIGHT, "100");
        e.set_inline_style(col, BORDER_WIDTH, "6");
        e.set_inline_style(col, BORDER_COLOR, "#89b4fa");
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, WIDTH, "20");
        e.set_inline_style(child, HEIGHT, "20");

        let dom = dom_from(e);
        let (scene, lay) = layout(&dom, Size { w: 200.0, h: 200.0 });
        let child_rect = lay.rects.iter().find(|(id, _)| *id == child).unwrap().1;
        assert_eq!(
            child_rect.origin,
            Point { x: 6.0, y: 6.0 },
            "the border width insets content, like padding"
        );
        // The frame is still emitted (push_border unchanged), now consistent with the inset.
        let has_border = scene
            .items
            .iter()
            .any(|i| matches!(i, DisplayItem::Border { width, .. } if *width == 6.0));
        assert!(has_border, "the border frame is still painted");
    }

    #[test]
    fn z_index_orders_sibling_paint() {
        // Two overlapping siblings: the one with the LOWER z-index paints first (behind),
        // the higher one later (on top), even though the higher one comes first in tree
        // order — so z-index, not tree order, decides.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        // `high` is first in tree order but has the greater z-index -> must paint LAST.
        let high = e.create_element(ElementTag::new(2));
        e.append(col, high);
        e.set_inline_style(high, WIDTH, "20");
        e.set_inline_style(high, HEIGHT, "20");
        e.set_inline_style(high, BG, "#ff0000");
        e.set_inline_style(high, Z_INDEX, "5");
        let low = e.create_element(ElementTag::new(2));
        e.append(col, low);
        e.set_inline_style(low, WIDTH, "20");
        e.set_inline_style(low, HEIGHT, "20");
        e.set_inline_style(low, BG, "#0000ff");
        e.set_inline_style(low, Z_INDEX, "1");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });
        let red_idx = scene
            .items
            .iter()
            .position(|i| {
                matches!(i, DisplayItem::Rect { color, .. }
                if (color.r, color.g, color.b) == (0xff, 0x00, 0x00))
            })
            .expect("red rect");
        let blue_idx = scene
            .items
            .iter()
            .position(|i| {
                matches!(i, DisplayItem::Rect { color, .. }
                if (color.r, color.g, color.b) == (0x00, 0x00, 0xff))
            })
            .expect("blue rect");
        assert!(
            blue_idx < red_idx,
            "lower z-index (blue, z=1) paints before higher (red, z=5), regardless of tree order"
        );
    }

    #[test]
    fn equal_z_index_keeps_tree_order() {
        // Equal z-index (the common all-default case) preserves tree order: the first
        // sibling still paints first. This proves the z-sort is stable.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let first = e.create_element(ElementTag::new(2));
        e.append(col, first);
        e.set_inline_style(first, WIDTH, "20");
        e.set_inline_style(first, HEIGHT, "20");
        e.set_inline_style(first, BG, "#ff0000");
        let second = e.create_element(ElementTag::new(2));
        e.append(col, second);
        e.set_inline_style(second, WIDTH, "20");
        e.set_inline_style(second, HEIGHT, "20");
        e.set_inline_style(second, BG, "#0000ff");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });
        let red_idx = scene
            .items
            .iter()
            .position(|i| {
                matches!(i, DisplayItem::Rect { color, .. }
                if (color.r, color.g, color.b) == (0xff, 0x00, 0x00))
            })
            .expect("red rect");
        let blue_idx = scene
            .items
            .iter()
            .position(|i| {
                matches!(i, DisplayItem::Rect { color, .. }
                if (color.r, color.g, color.b) == (0x00, 0x00, 0xff))
            })
            .expect("blue rect");
        assert!(
            red_idx < blue_idx,
            "equal z-index keeps tree order (stable sort): first sibling paints first"
        );
    }

    #[test]
    fn parse_color_accepts_8_hex_alpha() {
        // `#rrggbb` keeps alpha 255; `#rrggbbaa` parses the 4th byte as the alpha. A
        // translucent value must survive so shadows/gradients can fade.
        assert_eq!(
            parse_color("#11223344"),
            Some(Color {
                r: 0x11,
                g: 0x22,
                b: 0x33,
                a: 0x44
            }),
            "8-hex parses the trailing alpha byte"
        );
        assert_eq!(
            parse_color("#112233"),
            Some(Color {
                r: 0x11,
                g: 0x22,
                b: 0x33,
                a: 255
            }),
            "6-hex defaults alpha to opaque"
        );
        // A non-6/8 length is still rejected.
        assert_eq!(parse_color("#1234"), None);
    }

    #[test]
    fn background_image_emits_a_gradient_with_even_stops() {
        // A `linear-gradient(180, #ff0000, #00ff00, #0000ff)` background emits a Gradient
        // just after the solid background Rect, with three evenly-spaced stops (0, 0.5, 1)
        // and a Vertical direction (180deg).
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, BG, "#000000"); // a solid fill behind the gradient
        e.set_inline_style(
            card,
            BACKGROUND_IMAGE,
            "linear-gradient(180, #ff0000, #00ff00, #0000ff)",
        );

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });

        let (stops, direction) = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Gradient {
                    stops, direction, ..
                } => Some((*stops, *direction)),
                _ => None,
            })
            .expect("a Gradient item is in the scene");
        assert_eq!(direction, GradientDirection::Vertical, "180deg -> vertical");
        let slice = stops.as_slice();
        assert_eq!(slice.len(), 3, "three stops");
        assert_eq!(slice[0].position, 0.0);
        assert_eq!(slice[1].position, 0.5, "evenly spaced: middle stop at 0.5");
        assert_eq!(slice[2].position, 1.0);
        assert_eq!(
            (slice[0].color.r, slice[0].color.g, slice[0].color.b),
            (0xff, 0x00, 0x00)
        );
        assert_eq!(
            (slice[2].color.r, slice[2].color.g, slice[2].color.b),
            (0x00, 0x00, 0xff)
        );

        // The gradient paints over the solid background fill.
        let bg_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Rect { .. }))
            .unwrap();
        let grad_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Gradient { .. }))
            .unwrap();
        assert!(
            bg_idx < grad_idx,
            "the gradient paints over the solid background"
        );
    }

    #[test]
    fn horizontal_gradient_maps_to_horizontal_direction() {
        // A 90deg gradient (to-right) maps to the Horizontal axis; a single stop sits at 0.0.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, BACKGROUND_IMAGE, "linear-gradient(90, #abcdef)");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });
        let (stops, direction) = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Gradient {
                    stops, direction, ..
                } => Some((*stops, *direction)),
                _ => None,
            })
            .expect("a Gradient item");
        assert_eq!(
            direction,
            GradientDirection::Horizontal,
            "90deg -> horizontal"
        );
        let slice = stops.as_slice();
        assert_eq!(slice.len(), 1);
        assert_eq!(slice[0].position, 0.0, "a single stop sits at 0.0");
    }

    #[test]
    fn box_shadow_emits_a_shadow_with_offset_blur_and_translucent_color() {
        // `box-shadow: 4 -2 6 #00000080` emits a Shadow behind the box, carrying the
        // offset (4, -2), blur 6, and a HALF-ALPHA black — proving `parse_color`'s 8-hex
        // alpha path feeds the shadow.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, BG, "#202020");
        e.set_inline_style(card, BOX_SHADOW, "4 -2 6 #00000080");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });
        let (color, blur, offset) = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Shadow {
                    color,
                    blur,
                    offset,
                    ..
                } => Some((*color, *blur, *offset)),
                _ => None,
            })
            .expect("a Shadow item is in the scene");
        assert_eq!(
            offset,
            Point { x: 4.0, y: -2.0 },
            "signed dx/dy ride through"
        );
        assert_eq!(blur, 6.0);
        assert_eq!(
            color,
            Color {
                r: 0x00,
                g: 0x00,
                b: 0x00,
                a: 0x80
            },
            "the translucent #00000080 shadow color survives (parse_color alpha)"
        );

        // The shadow is emitted BEFORE the background fill so it sits behind the box.
        let shadow_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Shadow { .. }))
            .unwrap();
        let bg_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Rect { .. }))
            .unwrap();
        assert!(shadow_idx < bg_idx, "the shadow paints behind the box");
    }

    #[test]
    fn underline_emits_a_thin_rect_near_the_baseline() {
        // A text leaf with `text-decoration: underline` emits a thin Rect spanning the box
        // width, positioned near the bottom of the cell, just after the Text run.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let t = e.create_text("ab");
        e.append(col, t);
        e.set_inline_style(t, WIDTH, "60");
        e.set_inline_style(t, HEIGHT, "16");
        e.set_inline_style(t, FG, "#ffd040");
        e.set_inline_style(t, TEXT_DECORATION, "underline");

        let dom = dom_from(e);
        let (scene, lay) = layout(&dom, Size { w: 100.0, h: 50.0 });
        let t_rect = lay.rects.iter().find(|(id, _)| *id == t).unwrap().1;

        // The decoration Rect: full box width, 1px thick (16/16 = 1), the run's ink color,
        // near the bottom (origin.y + 16 - 1 = 15).
        let (deco_rect, color) = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Rect { rect, color, .. } => Some((*rect, *color)),
                _ => None,
            })
            .expect("an underline Rect");
        assert_eq!(deco_rect.size.w, 60.0, "the line spans the text box width");
        assert_eq!(deco_rect.size.h, 1.0, "thickness max(1, 16/16) = 1");
        assert_eq!(
            deco_rect.origin.y,
            t_rect.origin.y + 16.0 - 1.0,
            "underline sits near the bottom of the cell"
        );
        assert_eq!(
            color,
            Color {
                r: 0xff,
                g: 0xd0,
                b: 0x40,
                a: 255
            },
            "the underline takes the text's ink color"
        );

        // The decoration is emitted AFTER the text run.
        let text_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Text { .. }))
            .unwrap();
        let deco_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Rect { .. }))
            .unwrap();
        assert!(text_idx < deco_idx, "the decoration paints over the glyphs");
    }

    #[test]
    fn line_through_sits_at_the_vertical_middle() {
        // `text-decoration: line-through` puts the line through the vertical middle of the
        // cell, not the bottom. A 32px run -> thickness 2 (32/16), y = origin + (32-2)/2 = 15.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let t = e.create_text("ab");
        e.append(col, t);
        e.set_inline_style(t, WIDTH, "60");
        e.set_inline_style(t, HEIGHT, "32");
        e.set_inline_style(t, TEXT_DECORATION, "line-through");

        let dom = dom_from(e);
        let (scene, lay) = layout(&dom, Size { w: 100.0, h: 60.0 });
        let t_rect = lay.rects.iter().find(|(id, _)| *id == t).unwrap().1;
        let deco_rect = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Rect { rect, .. } => Some(*rect),
                _ => None,
            })
            .expect("a line-through Rect");
        assert_eq!(deco_rect.size.h, 2.0, "thickness max(1, 32/16) = 2");
        assert_eq!(
            deco_rect.origin.y,
            t_rect.origin.y + (32.0 - 2.0) / 2.0,
            "line-through sits at the vertical middle"
        );
    }

    #[test]
    fn text_decoration_none_emits_no_line() {
        // `text-decoration: none` (and an unset value) draws no decoration Rect.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let t = e.create_text("ab");
        e.append(col, t);
        e.set_inline_style(t, HEIGHT, "16");
        e.set_inline_style(t, TEXT_DECORATION, "none");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 50.0 });
        let rects = scene
            .items
            .iter()
            .filter(|i| matches!(i, DisplayItem::Rect { .. }))
            .count();
        assert_eq!(rects, 0, "text-decoration: none draws no line");
    }

    #[test]
    fn outline_emits_an_inflated_border_last() {
        // `outline-width: 2`, `outline-color`, `outline-offset: 3` on a node emits a Border
        // whose rect is the border-box inflated by the offset on all sides, with the node's
        // radius — and it is the LAST item this node draws (on top of its own border fill).
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, RADIUS, "5");
        e.set_inline_style(card, BG, "#202020");
        e.set_inline_style(card, OUTLINE_WIDTH, "2");
        e.set_inline_style(card, OUTLINE_COLOR, "#89b4fa");
        e.set_inline_style(card, OUTLINE_OFFSET, "3");

        let dom = dom_from(e);
        let (scene, lay) = layout(&dom, Size { w: 100.0, h: 100.0 });
        let card_rect = lay.rects.iter().find(|(id, _)| *id == card).unwrap().1;

        let (rect, color, width, radius) = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Border {
                    rect,
                    color,
                    width,
                    radius,
                } => Some((*rect, *color, *width, *radius)),
                _ => None,
            })
            .expect("an outline Border item");
        assert_eq!(width, 2.0, "outline width rides onto the Border");
        assert_eq!(radius, 5.0, "outline uses the node's corner radius");
        assert_eq!(
            color,
            Color {
                r: 0x89,
                g: 0xb4,
                b: 0xfa,
                a: 255
            }
        );
        // Inflated by the 3px offset on all four sides.
        assert_eq!(
            rect.origin,
            Point {
                x: card_rect.origin.x - 3.0,
                y: card_rect.origin.y - 3.0,
            },
            "the outline rect is inflated outward by the offset"
        );
        assert_eq!(
            rect.size,
            Size {
                w: card_rect.size.w + 6.0,
                h: card_rect.size.h + 6.0,
            },
            "inflated by the offset on both sides of each axis"
        );

        // The outline is the last item the node emits (after its background fill).
        let bg_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Rect { .. }))
            .unwrap();
        let outline_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Border { .. }))
            .unwrap();
        assert!(bg_idx < outline_idx, "the outline paints on top of the box");
    }

    #[test]
    fn zero_outline_width_emits_no_border() {
        // An `outline-width: 0` (or unset) emits no Border, even with a color set.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, OUTLINE_WIDTH, "0");
        e.set_inline_style(card, OUTLINE_COLOR, "#ffffff");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });
        let borders = scene
            .items
            .iter()
            .filter(|i| matches!(i, DisplayItem::Border { .. }))
            .count();
        assert_eq!(borders, 0, "zero outline width draws no frame");
    }

    #[test]
    fn full_node_paint_order_is_shadow_bg_gradient_border_outline() {
        // A single node carrying ALL of the new paint properties emits them in the canonical
        // back-to-front order: shadow, background fill, gradient, border, outline.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, BG, "#101010");
        e.set_inline_style(card, BOX_SHADOW, "2 2 4 #00000080");
        e.set_inline_style(
            card,
            BACKGROUND_IMAGE,
            "linear-gradient(0, #ff0000, #0000ff)",
        );
        e.set_inline_style(card, BORDER_WIDTH, "2");
        e.set_inline_style(card, BORDER_COLOR, "#ffffff");
        e.set_inline_style(card, OUTLINE_WIDTH, "1");
        e.set_inline_style(card, OUTLINE_COLOR, "#00ff00");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });

        let shadow = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Shadow { .. }))
            .unwrap();
        let bg = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Rect { .. }))
            .unwrap();
        let gradient = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Gradient { .. }))
            .unwrap();
        // The border frame and the outline are both `Border` items; the border is pushed
        // before the outline, so the first Border is the frame and the last is the outline.
        let first_border = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Border { .. }))
            .unwrap();
        let last_border = scene
            .items
            .iter()
            .rposition(|i| matches!(i, DisplayItem::Border { .. }))
            .unwrap();
        assert!(
            shadow < bg && bg < gradient && gradient < first_border && first_border < last_border,
            "paint order: shadow < bg < gradient < border < outline (got {shadow}, {bg}, {gradient}, {first_border}, {last_border})"
        );
    }

    #[test]
    fn gradient_and_shadow_fade_with_opacity() {
        // The gradient stops and the shadow color are faded by the node's resolved opacity,
        // exactly like the background fill and border.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, OPACITY, "0.5");
        e.set_inline_style(card, BOX_SHADOW, "2 2 4 #000000"); // opaque, then faded to ~128
        e.set_inline_style(card, BACKGROUND_IMAGE, "linear-gradient(180, #ffffff)");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });
        let shadow_a = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Shadow { color, .. } => Some(color.a),
                _ => None,
            })
            .expect("a Shadow item");
        assert_eq!(shadow_a, 128, "the shadow alpha is scaled by opacity 0.5");
        let stop_a = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Gradient { stops, .. } => Some(stops.as_slice()[0].color.a),
                _ => None,
            })
            .expect("a Gradient item");
        assert_eq!(
            stop_a, 128,
            "the gradient stop alpha is scaled by opacity 0.5"
        );
    }

    #[test]
    fn negative_outline_offset_pulls_the_outline_inward() {
        // A negative `outline-offset` deflates the rect (the outline is drawn inside the box).
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, OUTLINE_WIDTH, "2");
        e.set_inline_style(card, OUTLINE_COLOR, "#ffffff");
        e.set_inline_style(card, OUTLINE_OFFSET, "-4");

        let dom = dom_from(e);
        let (scene, lay) = layout(&dom, Size { w: 100.0, h: 100.0 });
        let card_rect = lay.rects.iter().find(|(id, _)| *id == card).unwrap().1;
        let rect = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Border { rect, .. } => Some(*rect),
                _ => None,
            })
            .expect("an outline Border");
        assert_eq!(
            rect.origin,
            Point {
                x: card_rect.origin.x + 4.0,
                y: card_rect.origin.y + 4.0,
            },
            "a negative offset pulls the outline inward"
        );
        assert_eq!(
            rect.size,
            Size { w: 32.0, h: 32.0 },
            "deflated by 4 on each side"
        );
    }
}
