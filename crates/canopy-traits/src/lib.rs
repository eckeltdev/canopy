//! Canopy's platform-abstraction layer (PAL): the backend traits and the
//! Canopy-owned types that cross them.
//!
//! This crate is the seam that makes Canopy portable. **The one rule:** the types
//! that cross these traits ([`ComputedStyle`], [`LayoutResult`], [`ShapedGlyphs`],
//! [`DisplayList`], …) are Canopy-owned and `no_std`. A backend may use Stylo,
//! Taffy, Parley, Vello, winit, or a bare-metal framebuffer internally, but a
//! vendor type must **never** appear in a trait signature — leaking one would weld
//! the runtime to the desktop stack and break the bare-metal promise.
//!
//! Desktop impls of these traits are `std` leaf crates; bare-metal impls are
//! `no_std`. The core never knows which is linked.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use canopy_protocol::NodeId;

// ---------------------------------------------------------------------------
// Geometry and resolved-style types (Canopy-owned; no vendor types).
// ---------------------------------------------------------------------------

/// A size in logical pixels.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Size {
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

/// A point in logical pixels.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Point {
    /// X.
    pub x: f32,
    /// Y.
    pub y: f32,
}

/// An axis-aligned rectangle.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    /// Top-left origin.
    pub origin: Point,
    /// Size.
    pub size: Size,
}

/// Straight-alpha RGBA color.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Color {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha.
    pub a: u8,
}

/// The axis a simple two-stop [`LinearGradient`] runs along.
///
/// The seam carries only the two common orthogonal directions a basic CSS
/// `linear-gradient(to bottom, …)` / `linear-gradient(to right, …)` produces;
/// any other angle (or a diagonal "to corner") is mapped to the nearer axis when
/// flattening, so a renderer only ever has to fill along one of these two.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GradientAxis {
    /// Top → bottom (`to bottom`): `start` at the top edge, `end` at the bottom.
    #[default]
    Vertical,
    /// Left → right (`to right`): `start` at the left edge, `end` at the right.
    Horizontal,
}

/// A reduced **two-stop linear gradient** background.
///
/// This is the seam's small, `Copy` stand-in for a CSS `linear-gradient`: the
/// first and last color stop plus the [`axis`](Self::axis) it runs along. A
/// renderer fills the box by interpolating `start` → `end` across that axis. CSS
/// gradients with more than two stops collapse to their first and last stop here
/// (a faithful endpoint match); non-axis-aligned angles snap to the nearer of the
/// two [`GradientAxis`] directions.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct LinearGradient {
    /// Color at the start of the axis (top for vertical, left for horizontal).
    pub start: Color,
    /// Color at the end of the axis (bottom for vertical, right for horizontal).
    pub end: Color,
    /// The axis the gradient runs along.
    pub axis: GradientAxis,
}

/// A reduced **outset box-shadow**: an offset, a blur radius, and a color.
///
/// The seam's `Copy` stand-in for a CSS `box-shadow`: the shadow is drawn as a
/// soft rectangle the same size as the element's border-box, translated by
/// (`dx`, `dy`) and feathered by `blur` logical px, in `color`. Only the first
/// **outset** (non-`inset`) shadow of a `box-shadow` list is carried; spread and
/// inset shadows are dropped.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct BoxShadow {
    /// Horizontal offset in logical px (positive = right).
    pub dx: f32,
    /// Vertical offset in logical px (positive = down).
    pub dy: f32,
    /// Blur radius in logical px (`0.0` = a hard-edged offset rect).
    pub blur: f32,
    /// Shadow color (already resolved against `currentColor`).
    pub color: Color,
}

/// How a node lays its children out.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Display {
    /// Block flow.
    #[default]
    Block,
    /// Flexbox.
    Flex,
    /// Hidden / not generated.
    None,
}

/// A flat, fully-resolved style for one node.
///
/// This is the output of a [`StyleEngine`] — Stylo on the desktop, a const/
/// build-time resolver on a constrained target. The retained tree only ever sees
/// this; there is no "cascade" type in the core.
///
/// The paint-affecting fields beyond the box model — `border_width`,
/// `border_color`, `border_radius`, and `opacity` — let a renderer draw a framed,
/// rounded, optionally-faded box without re-consulting the style engine.
/// `opacity` is a straight multiplier on every painted color's alpha (`1.0` =
/// fully opaque, the default).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ComputedStyle {
    /// Layout mode.
    pub display: Display,
    /// Text/foreground color.
    pub color: Color,
    /// Background color.
    pub background: Color,
    /// Font size in logical pixels.
    pub font_size: f32,
    /// Uniform padding in logical pixels.
    pub padding: f32,
    /// Uniform border width in logical pixels (`border-top-width`). `0.0` = no
    /// border frame.
    pub border_width: f32,
    /// Border color (`border-top-color`), painted as the frame when
    /// `border_width > 0.0`.
    pub border_color: Color,
    /// Uniform corner radius in logical pixels (`border-top-left-radius`). `0.0` =
    /// square corners.
    pub border_radius: f32,
    /// Element opacity in `[0.0, 1.0]`: a straight multiplier applied to every
    /// painted color's alpha. Defaults to `1.0` (fully opaque).
    pub opacity: f32,
    /// Whether the element's first `font-family` is **Ahem** (case-insensitive).
    ///
    /// Ahem is the metrics-perfect WPT test font where every glyph is a solid 1em
    /// square. A renderer that lacks a real Ahem face (e.g. the baked-bitmap CPU
    /// path) can honor this flag by drawing each character as a filled `font_size`-
    /// by-`font_size` square in the foreground `color`, so the painted geometry
    /// matches what [`measure`](StyleEngine) /
    /// [`TextEngine`](crate::TextEngine) sized the box to. Defaults to `false`.
    pub is_ahem: bool,
    /// A reduced two-stop `linear-gradient` background, if the element has one.
    ///
    /// When `Some`, a renderer fills the box with this gradient *instead of* the
    /// flat [`background`](Self::background) color (the gradient is the more
    /// specific paint). `None` (the default) means there is no gradient and the
    /// flat background applies.
    pub gradient: Option<LinearGradient>,
    /// A reduced outset `box-shadow`, if the element has one.
    ///
    /// When `Some`, a renderer draws a soft shadow rect behind the element's box.
    /// `None` (the default) means no shadow.
    pub box_shadow: Option<BoxShadow>,
}

impl Default for ComputedStyle {
    fn default() -> Self {
        ComputedStyle {
            display: Display::default(),
            color: Color::default(),
            background: Color::default(),
            font_size: 0.0,
            padding: 0.0,
            border_width: 0.0,
            border_color: Color::default(),
            border_radius: 0.0,
            // Opacity must default to fully-opaque, not 0.0, so a style that
            // never sets it paints normally.
            opacity: 1.0,
            // Default font-family is not Ahem.
            is_ahem: false,
            // No gradient / shadow by default.
            gradient: None,
            box_shadow: None,
        }
    }
}

/// Per-node computed layout boxes for a frame.
#[derive(Clone, Debug, Default)]
pub struct LayoutResult {
    /// Resolved rectangle per node.
    pub rects: Vec<(NodeId, Rect)>,
}

/// One positioned glyph.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Glyph {
    /// Glyph id within the font.
    pub id: u32,
    /// Pen X.
    pub x: f32,
    /// Pen Y.
    pub y: f32,
}

/// Output of a [`TextEngine`] shaping pass.
#[derive(Clone, Debug, Default)]
pub struct ShapedGlyphs {
    /// Positioned glyphs.
    pub glyphs: Vec<Glyph>,
}

/// One drawable primitive in a resolved display list.
#[derive(Clone, Debug)]
pub enum DisplayItem {
    /// A filled rectangle.
    Rect {
        /// Bounds.
        rect: Rect,
        /// Fill color.
        color: Color,
        /// Corner radius in logical px; 0.0 = square.
        ///
        /// Renderers that don't implement rounding ignore this and draw a plain
        /// rectangle (the legacy behavior); the CPU renderers
        /// ([`canopy_render_soft`](https://docs.rs/canopy-render-soft) and
        /// `canopy-render-text`) round the four corners. Renderers clamp the
        /// radius to half the rect's shorter side, so an arbitrarily large value
        /// yields a pill/stadium rather than overflowing the box.
        radius: f32,
    },
    /// A run of shaped glyphs (capable tiers: Parley/Vello produce these).
    Glyphs {
        /// Shaped glyphs.
        glyphs: ShapedGlyphs,
        /// Fill color.
        color: Color,
    },
    /// A run of unshaped text, drawn by a baked bitmap font on constrained tiers.
    ///
    /// The renderer lays the `text` out as a monospace run starting at `origin`,
    /// painting "ink" pixels in `color`. `size` is the target cell height in
    /// logical pixels; the baked font is 8px tall, so the integer scale factor is
    /// `max(1, (size / 8).floor())`.
    ///
    /// `box_w` and `align` let a renderer center or right-align the run **using its
    /// own measured run width**, which is the honest way to align proportional
    /// glyphs whose drawn width differs from the layout box's baked-font width.
    /// Each renderer measures the run in its own metric (the real pixel width for
    /// the antialiased Parley path; `char_count * advance` for the baked-font CPU
    /// paths) and shifts the run's start x by `(box_w - run_width) * align`, clamped
    /// to `>= 0`. With `align == 0.0` (the default) the run is unshifted, exactly the
    /// legacy left/start-aligned behavior.
    Text {
        /// Top-left pen position of the first cell, before any alignment shift.
        origin: Point,
        /// The text to draw.
        text: String,
        /// Ink color.
        color: Color,
        /// Target cell height in logical pixels (scale = `size / 8`).
        size: f32,
        /// The node's box width to align the run within, in logical pixels. The
        /// renderer centers/right-aligns the run inside this width using its own
        /// measured run width (see [`align`](DisplayItem::Text::align)).
        box_w: f32,
        /// Horizontal alignment of the run within `box_w`: `0.0` = left/start (the
        /// default, legacy behavior), `0.5` = centered, `1.0` = right/end. The
        /// renderer offsets the run's start x by `(box_w - run_width) * align`,
        /// clamped to `>= 0` so a box narrower than the run never pushes ink left.
        align: f32,
    },
}

/// A flat, back-to-front list of primitives handed to a [`Renderer`].
#[derive(Clone, Debug, Default)]
pub struct DisplayList {
    /// Items, painted in order.
    pub items: Vec<DisplayItem>,
}

// ---------------------------------------------------------------------------
// Errors (core-only; no `std::error::Error`).
// ---------------------------------------------------------------------------

/// A host-side failure applying ops or running a backend.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HostError {
    /// A handle referenced a node that does not exist or was not owned by the guest.
    BadHandle,
    /// The op-stream could not be decoded.
    Decode,
    /// The operation is not supported by this backend/tier.
    Unsupported,
}

/// A transport-layer failure moving ops or events.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportError {
    /// The peer is gone.
    Closed,
    /// The batch exceeded a configured limit.
    TooLarge,
    /// Backend-specific failure (e.g. a trap in the WASM guest).
    Backend,
}

impl core::fmt::Display for HostError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            HostError::BadHandle => "bad node handle",
            HostError::Decode => "op-stream decode error",
            HostError::Unsupported => "unsupported operation for this tier",
        })
    }
}

impl core::fmt::Display for TransportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            TransportError::Closed => "transport closed",
            TransportError::TooLarge => "op batch too large",
            TransportError::Backend => "transport backend error",
        })
    }
}

// ---------------------------------------------------------------------------
// The backend traits (the PAL).
// ---------------------------------------------------------------------------

/// Applies a batch of `canopy-protocol` op bytes atomically to the host's
/// retained tree. Implemented by the host; this is the consuming end of the
/// op-stream.
pub trait OpSink {
    /// Decode and apply one batch.
    fn apply(&mut self, ops: &[u8]) -> Result<(), HostError>;
}

/// Moves op bytes guest→host and event bytes host→guest. The two impls
/// (compiled-in native and WASM-sandboxed) carry the **same** op bytes; only the
/// delivery mechanism and the trust model differ.
pub trait Transport {
    /// Send one encoded op batch to the host.
    fn send(&mut self, batch: &[u8]) -> Result<(), TransportError>;
    /// Drain any pending host→guest event bytes into `out`.
    fn poll_events(&mut self, out: &mut Vec<u8>) -> Result<(), TransportError>;
}

/// Resolves a node's flat [`ComputedStyle`] (Stylo on desktop; a reduced resolver
/// on constrained tiers).
pub trait StyleEngine {
    /// Compute the style for `node` given its parent's computed style, if any.
    fn resolve(
        &mut self,
        node: NodeId,
        parent: Option<&ComputedStyle>,
    ) -> Result<ComputedStyle, HostError>;
}

/// Computes layout boxes for the tree (Taffy on every tier).
pub trait LayoutEngine {
    /// Lay the tree rooted at `root` out within `available`, writing boxes to `out`.
    fn layout(
        &mut self,
        root: NodeId,
        available: Size,
        out: &mut LayoutResult,
    ) -> Result<(), HostError>;
}

/// Measures and shapes text (Parley/cosmic-text on capable tiers; a baked glyph
/// atlas on constrained tiers).
pub trait TextEngine {
    /// Measure a run without shaping it (used by layout to size flex children).
    fn measure(&mut self, text: &str, style: &ComputedStyle) -> Size;
    /// Shape a run into positioned glyphs.
    fn shape(
        &mut self,
        text: &str,
        style: &ComputedStyle,
        out: &mut ShapedGlyphs,
    ) -> Result<(), HostError>;
}

/// Rasterizes a [`DisplayList`] to a surface (Vello+wgpu on capable tiers;
/// `vello_cpu`/software on constrained tiers).
pub trait Renderer {
    /// React to a surface resize.
    fn resize(&mut self, size: Size);
    /// Paint one frame.
    fn render(&mut self, scene: &DisplayList) -> Result<(), HostError>;
    /// Present the painted frame.
    fn present(&mut self) -> Result<(), HostError>;
}

/// What the event loop should do after a pump.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlFlow {
    /// Keep running.
    Continue,
    /// Tear down.
    Exit,
}

/// Owns the window/surface, input, and the monotonic clock (winit on desktop; a
/// HAL on bare metal).
pub trait Platform {
    /// Current surface size.
    fn surface_size(&self) -> Size;
    /// Monotonic milliseconds. Bare-metal supplies its own timer.
    fn now_millis(&self) -> u64;
    /// Pump the platform, appending any input as `canopy-protocol` event bytes.
    fn pump(&mut self, events: &mut Vec<u8>) -> ControlFlow;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let s = ComputedStyle::default();
        assert_eq!(s.display, Display::Block);
        assert_eq!(s.color, Color::default());
        // The enriched paint fields default to "no frame, square, fully opaque".
        assert_eq!(s.border_width, 0.0);
        assert_eq!(s.border_color, Color::default());
        assert_eq!(s.border_radius, 0.0);
        assert_eq!(s.opacity, 1.0);
        assert!(DisplayList::default().items.is_empty());
    }

    #[test]
    fn errors_display() {
        use alloc::format;
        assert_eq!(format!("{}", HostError::BadHandle), "bad node handle");
        assert_eq!(format!("{}", TransportError::Closed), "transport closed");
    }
}
