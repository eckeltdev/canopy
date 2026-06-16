//! Canopy's **capable-tier renderer**: paint a [`Dom`] into a software RGBA buffer
//! with *real, antialiased* glyphs.
//!
//! This is the sharp-text counterpart to [`canopy_render_soft`]'s baked 8×8
//! 1-bit font path. It reuses the same geometry ([`canopy_layout_taffy::layout`])
//! and the same pixel surface ([`canopy_render_soft::Buffer`]), but rasterizes
//! every [`DisplayItem::Text`] run through [`canopy_text_parley`] — which shapes
//! and rasterizes with `cosmic-text`/`swash` against a bundled DejaVu Sans Mono —
//! and **alpha-over composites** the resulting 8-bit coverage mask onto the
//! buffer. The partial-coverage edge pixels that compositing produces are exactly
//! the antialiasing the 1-bit baked font can never express.
//!
//! # Compositing
//!
//! [`canopy_text_parley`] hands back *straight* (non-premultiplied) alpha
//! coverage: `0` = background, `255` = full ink, in-between = a fractional edge.
//! For each glyph pixel we read the destination through [`Buffer::pixel`], blend
//! `ink` over it with the per-pixel coverage as alpha
//! (`out = ink·a + dst·(1−a)`, rounded), and write the result back through a 1×1
//! [`Buffer::fill_rect`]. We never touch [`canopy_render_soft`]'s internals — only
//! its public read/write surface — so AA edges feather correctly over whatever was
//! painted underneath (a background rect, another run, the clear color).
//!
//! # Entry points
//!
//! - [`render_dom`] — one-shot: lay out `dom` at `viewport`, paint onto a freshly
//!   cleared [`Buffer`], return it.
//! - [`TextRenderer`] — an owned [`Buffer`] + cached [`TextEngine`] that implements
//!   [`canopy_traits::Renderer`], so it slots into the renderer-agnostic host seam
//!   alongside [`canopy_render_soft::SoftwareRenderer`].
//!
//! This is a `std` crate (the text engine needs `std`); it is *not* `no_std`.

use canopy_dom::Dom;
use canopy_render_soft::Buffer;
use canopy_text_parley::TextEngine;
use canopy_traits::{Color, DisplayItem, DisplayList, HostError, Point, Renderer, Size};

/// Composite a straight-alpha coverage mask onto `buffer` in `ink`, with its
/// top-left at `origin`.
///
/// `coverage` is row-major, one byte per pixel, exactly `width * height` bytes
/// (the layout [`canopy_text_parley::Glyphs`] uses). Each byte is the fractional
/// ink coverage at that pixel: `0` leaves the destination untouched, `255`
/// overwrites it with `ink`, and anything between blends `ink` over the existing
/// pixel — `out = ink·a + dst·(1 − a)` per channel, with `a = coverage/255`.
///
/// The destination is read through [`Buffer::pixel`] and written through a 1×1
/// [`Buffer::fill_rect`], so this only uses the buffer's public surface and works
/// over *whatever* was already painted there (giving feathered AA edges). Pixels
/// that fall outside the buffer are skipped; the ink's own alpha is folded into
/// the coverage so a translucent ink stays translucent.
pub fn composite_coverage(
    buffer: &mut Buffer,
    origin: Point,
    coverage: &[u8],
    width: u32,
    height: u32,
    ink: Color,
) {
    // Snap the run origin to whole pixels; negatives clamp to 0 so off-screen-left
    // origins still composite their visible remainder.
    let ox = origin.x.max(0.0) as usize;
    let oy = origin.y.max(0.0) as usize;
    let w = width as usize;
    let h = height as usize;

    for row in 0..h {
        let py = oy + row;
        if py >= buffer.height() {
            break;
        }
        let row_base = row * w;
        for col in 0..w {
            let px = ox + col;
            if px >= buffer.width() {
                break;
            }
            let cov = coverage[row_base + col];
            if cov == 0 {
                continue; // fully transparent: leave the destination as-is.
            }
            // Fold the ink's own alpha into the coverage (translucent ink stays
            // translucent). `a` is the effective per-pixel coverage in 0..=255.
            let a = (u32::from(cov) * u32::from(ink.a)) / 255;
            let out = if a >= 255 {
                // Fully opaque: skip the read/blend, just stamp the ink.
                Color {
                    r: ink.r,
                    g: ink.g,
                    b: ink.b,
                    a: 255,
                }
            } else {
                let dst = buffer.pixel(px, py);
                Color {
                    r: blend(ink.r, dst[0], a),
                    g: blend(ink.g, dst[1], a),
                    b: blend(ink.b, dst[2], a),
                    // Straight-alpha "over": resulting coverage is a + dst·(1−a).
                    a: blend(255, dst[3], a),
                }
            };
            // We have already composited `out` against the destination above, so we
            // store it straight — never back through `fill_rect`, which now alpha-
            // blends (a 1×1 blended write would double-apply the composite over a
            // non-opaque destination). `set_pixel` is the straight-store counterpart
            // of the `pixel` read we did.
            buffer.set_pixel(px, py, [out.r, out.g, out.b, out.a]);
        }
    }
}

/// The horizontal offset that aligns a run of width `run_w` within a box of width
/// `box_w` at fraction `align` (`0.0` left, `0.5` center, `1.0` right): `(box_w −
/// run_w) * align`, clamped to `>= 0`.
///
/// The clamp keeps a box narrower than its run from yielding a negative offset that
/// would push ink off the left edge — a too-narrow box just falls back to left-
/// aligned. With `align == 0.0` the offset is `0`, so an un-aligned run is left
/// exactly where it was (the legacy behavior).
#[inline]
fn align_offset(box_w: f32, run_w: f32, align: f32) -> f32 {
    ((box_w - run_w) * align).max(0.0)
}

/// `src` over `dst` with straight alpha `a` (0..=255), rounded: `src·a + dst·(255−a)`.
///
/// Pure integer math (no float, no `unsafe`); `+127` gives round-to-nearest so the
/// blend is symmetric and never drifts darker at low coverage.
#[inline]
fn blend(src: u8, dst: u8, a: u32) -> u8 {
    let inv = 255 - a;
    let v = (u32::from(src) * a + u32::from(dst) * inv + 127) / 255;
    v as u8
}

/// Rasterize a single [`DisplayList`] onto `buffer` using `engine` for text.
///
/// [`DisplayItem::Rect`] fills via [`Buffer::fill_rect`], or
/// [`Buffer::fill_round_rect`] when it carries a positive `radius` (rounded cards
/// and pills); [`DisplayItem::Text`] is
/// shaped+rasterized by `engine` at the item's `size`/`color` and the resulting
/// antialiased coverage mask is alpha-over composited at the item's `origin` (see
/// [`composite_coverage`]). Pre-shaped [`DisplayItem::Glyphs`] runs are not emitted
/// by the Canopy layout path and are skipped.
///
/// The buffer is **not** cleared here — callers control the background (e.g.
/// [`render_dom`] clears first; the [`Renderer`] impl clears per frame).
pub fn paint_display_list(buffer: &mut Buffer, engine: &mut TextEngine, scene: &DisplayList) {
    for item in &scene.items {
        match item {
            DisplayItem::Rect {
                rect,
                color,
                radius,
            } => {
                // Reuse the software buffer's rounded fill so capable-tier cards and
                // pills round identically to the constrained-tier path. Square is the
                // common case, so only round when a positive radius is requested.
                if *radius > 0.0 {
                    buffer.fill_round_rect(*rect, *color, *radius);
                } else {
                    buffer.fill_rect(*rect, *color);
                }
            }
            DisplayItem::Text {
                origin,
                text,
                color,
                size,
                box_w,
                align,
            } => {
                let glyphs = engine.rasterize(text, *size, *color);
                // Center / right-align the run within its box using the run's OWN
                // real pixel width (`glyphs.width`) — the honest metric for these
                // proportional glyphs, which the baked layout width over-estimates.
                // Offset = (box_w - run_w) * align, clamped to >= 0 so a box narrower
                // than the run never shoves ink off the left edge. align 0 => 0
                // (legacy left-aligned).
                let origin = Point {
                    x: origin.x + align_offset(*box_w, glyphs.width as f32, *align),
                    y: origin.y,
                };
                composite_coverage(
                    buffer,
                    origin,
                    &glyphs.coverage,
                    glyphs.width,
                    glyphs.height,
                    *color,
                );
            }
            // Canopy's layout never emits pre-shaped glyph runs; nothing to do.
            DisplayItem::Glyphs { .. } => {}
        }
    }
}

/// Lay out `dom` at `viewport`, paint it onto a freshly cleared [`Buffer`] with
/// real antialiased text, and return the buffer.
///
/// Geometry comes from [`canopy_layout_taffy::layout`]; backgrounds fill as opaque
/// rects and text runs are rasterized + alpha-over composited (see
/// [`paint_display_list`]). The buffer is sized to `viewport` (rounded to whole
/// pixels) and cleared to `clear` before painting. A fresh [`TextEngine`] is built
/// per call — for repeated frames, hold a [`TextRenderer`] instead to reuse the
/// engine's shaping/rasterization caches.
#[must_use]
pub fn render_dom(dom: &Dom, viewport: Size, clear: Color) -> Buffer {
    let mut engine = TextEngine::new();
    render_dom_with(dom, viewport, clear, &mut engine)
}

/// Like [`render_dom`], but reuses a caller-owned [`TextEngine`] so its glyph
/// caches persist across frames.
#[must_use]
pub fn render_dom_with(dom: &Dom, viewport: Size, clear: Color, engine: &mut TextEngine) -> Buffer {
    let (scene, _layout) = canopy_layout_taffy::layout(dom, viewport);
    let mut buffer = Buffer::new(viewport.w as usize, viewport.h as usize);
    buffer.clear(clear);
    paint_display_list(&mut buffer, engine, &scene);
    buffer
}

/// A [`Renderer`] that paints [`DisplayList`]s into an owned [`Buffer`] with real
/// antialiased glyphs.
///
/// The sharp-text sibling of [`canopy_render_soft::SoftwareRenderer`]: same trait,
/// same surface, but text goes through [`canopy_text_parley`] and is alpha-over
/// composited instead of blitting a 1-bit baked font. The held [`TextEngine`]
/// caches shaping/rasterization across [`render`](Renderer::render) calls.
pub struct TextRenderer {
    buffer: Buffer,
    engine: TextEngine,
    clear: Color,
}

impl TextRenderer {
    /// New renderer with a `clear` background color and an empty buffer of the
    /// given size.
    #[must_use]
    pub fn new(width: usize, height: usize, clear: Color) -> Self {
        Self {
            buffer: Buffer::new(width, height),
            engine: TextEngine::new(),
            clear,
        }
    }

    /// The current frame buffer.
    #[must_use]
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Mutable access to the frame buffer — e.g. to composite another surface into
    /// the painted frame before presenting.
    pub fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffer
    }
}

impl Renderer for TextRenderer {
    fn resize(&mut self, size: Size) {
        self.buffer = Buffer::new(size.w as usize, size.h as usize);
    }

    fn render(&mut self, scene: &DisplayList) -> Result<(), HostError> {
        self.buffer.clear(self.clear);
        paint_display_list(&mut self.buffer, &mut self.engine, scene);
        Ok(())
    }

    fn present(&mut self) -> Result<(), HostError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_core::Emitter;
    use canopy_dom::ROOT;
    use canopy_protocol::{ElementTag, PropId};
    use canopy_traits::OpSink;
    use canopy_traits::Rect;

    // PropIds from canopy-paint (mirrored here to avoid a dep just for consts).
    const FG: PropId = PropId::new(2);
    const HEIGHT: PropId = PropId::new(4);

    fn color(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }

    /// Build a Dom with a single text leaf carrying `text`, sized and inked.
    fn dom_with_text(text: &str, height_px: &str, fg: &str) -> Dom {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1)); // COLUMN
        e.append(ROOT, col);
        let t = e.create_text(text);
        e.append(col, t);
        e.set_inline_style(t, HEIGHT, height_px);
        e.set_inline_style(t, FG, fg);
        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();
        dom
    }

    /// The alignment offset is `(box_w - run_w) * align`, clamped to >= 0.
    #[test]
    fn align_offset_centers_rights_and_clamps() {
        // Centering a 20-wide run in a 100-wide box: (100 - 20) * 0.5 = 40.
        assert_eq!(align_offset(100.0, 20.0, 0.5), 40.0);
        // Right-aligning: (100 - 20) * 1.0 = 80.
        assert_eq!(align_offset(100.0, 20.0, 1.0), 80.0);
        // Left (align 0.0) never shifts, regardless of box width.
        assert_eq!(align_offset(100.0, 20.0, 0.0), 0.0);
        // A box narrower than the run clamps to 0 (never pushes ink left).
        assert_eq!(align_offset(10.0, 20.0, 0.5), 0.0);
    }

    /// THE Parley-path centering proof: rendering a short run with `align = 0.5` in a
    /// box much wider than the run lands the real antialiased ink centered in the box
    /// — and strictly to the right of the same run drawn left-aligned. This exercises
    /// the run's OWN measured pixel width, the whole point of text-align.
    #[test]
    fn centered_run_inks_the_middle_and_beats_left_aligned() {
        let bg = color(0, 0, 0);
        let ink = color(255, 255, 255);
        let box_w = 200.0_f32;
        let mut engine = TextEngine::new();

        // Helper: render "Hi" at the given align and return the ink centroid x.
        let centroid_x = |engine: &mut TextEngine, align: f32| -> f32 {
            let mut buf = Buffer::new(box_w as usize, 24);
            buf.clear(bg);
            let scene = DisplayList {
                items: vec![DisplayItem::Text {
                    origin: Point { x: 0.0, y: 0.0 },
                    text: "Hi".into(),
                    color: ink,
                    size: 16.0,
                    box_w,
                    align,
                }],
            };
            paint_display_list(&mut buf, engine, &scene);
            let (mut sum_x, mut weight) = (0.0_f64, 0.0_f64);
            for y in 0..buf.height() {
                for x in 0..buf.width() {
                    let v = buf.pixel(x, y)[0] as f64; // white-on-black: red == coverage
                    if v > 0.0 {
                        sum_x += x as f64 * v;
                        weight += v;
                    }
                }
            }
            assert!(weight > 0.0, "the run must have inked something");
            (sum_x / weight) as f32
        };

        let left_cx = centroid_x(&mut engine, 0.0);
        let center_cx = centroid_x(&mut engine, 0.5);
        let box_center = box_w / 2.0;

        // Left-aligned ink hugs the left edge; centered ink sits near the box middle.
        assert!(
            left_cx < box_center * 0.6,
            "left-aligned centroid {left_cx} should hug the left edge"
        );
        assert!(
            (center_cx - box_center).abs() < 12.0,
            "centered centroid {center_cx} should sit near box center {box_center}"
        );
        // And centering must move the ink decisively rightward vs. left-aligned.
        assert!(
            center_cx > left_cx + 40.0,
            "centered ink ({center_cx}) must be well right of left-aligned ({left_cx})"
        );
    }

    /// blend(ink, bg, a) lands strictly between the two endpoints at partial alpha.
    #[test]
    fn blend_is_monotonic_and_bounded() {
        assert_eq!(blend(255, 0, 0), 0); // a=0 -> destination
        assert_eq!(blend(255, 0, 255), 255); // a=255 -> source
        let mid = blend(255, 0, 128);
        assert!(
            mid > 0 && mid < 255,
            "mid-alpha must be intermediate: {mid}"
        );
        // Rounding is symmetric: 200 over 100 at half alpha ~= 150.
        assert_eq!(blend(200, 100, 128), 150);
    }

    /// Compositing a known coverage mask over a known background must yield the
    /// straight-alpha "over" result, including a feathered mid-gray at the edge.
    #[test]
    fn composite_blends_over_background() {
        let bg = color(0, 0, 0);
        let ink = color(255, 255, 255);
        let mut buf = Buffer::new(3, 1);
        buf.clear(bg);
        // coverage: transparent | half | full
        composite_coverage(
            buf_mut(&mut buf),
            Point { x: 0.0, y: 0.0 },
            &[0, 128, 255],
            3,
            1,
            ink,
        );
        assert_eq!(buf.pixel(0, 0), [0, 0, 0, 255], "0 coverage leaves bg");
        let half = buf.pixel(1, 0);
        assert!(
            half[0] > 0 && half[0] < 255,
            "half coverage must be intermediate gray, got {half:?}"
        );
        assert_eq!(buf.pixel(2, 0), [255, 255, 255, 255], "full coverage = ink");
    }

    // Tiny helper so the call above reads naturally.
    fn buf_mut(b: &mut Buffer) -> &mut Buffer {
        b
    }

    /// Faded ink (a translucent ink color) fades the text: the ink's own alpha folds
    /// into the per-pixel coverage, so a full-coverage pixel of half-alpha white over
    /// black lands at the half-mix, not full ink. This is what carries an OPACITY-
    /// faded text run through to the pixels.
    #[test]
    fn translucent_ink_fades_full_coverage_text() {
        let bg = color(0, 0, 0);
        // White ink at alpha 128: even full glyph coverage is only ~half-strength.
        let ink = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 128,
        };
        let mut buf = Buffer::new(1, 1);
        buf.clear(bg);
        composite_coverage(&mut buf, Point { x: 0.0, y: 0.0 }, &[255], 1, 1, ink);
        // Effective alpha = 255 (coverage) * 128 (ink.a) / 255 = 128.
        // RGB: blend(255, 0, 128) = (255*128 + 0*127 + 127)/255 = 128.
        // A : blend(255, 255, 128) = (255*128 + 255*127 + 127)/255 = 255 (opaque bg
        //     stays opaque under "over").
        let px = buf.pixel(0, 0);
        assert_eq!(
            px,
            [128, 128, 128, 255],
            "half-alpha ink over opaque black is the half-mix color over an opaque dst"
        );
        // And it must be strictly dimmer than opaque ink would be.
        assert!(px[0] < 255, "faded ink must not reach full ink");
    }

    /// The headline guarantee: rendering real text produces antialiased
    /// (intermediate) pixels the 1-bit baked font could never make.
    #[test]
    fn renders_antialiased_text_with_intermediate_grays() {
        // White ink on a black background so any non-{0,255} channel value is, by
        // construction, an antialiased edge — impossible with a 1-bit font.
        let bg = color(0, 0, 0);
        // "Ag" has a curved bowl and a descender: lots of slanted/curved edges.
        let dom = dom_with_text("Ag", "32", "#ffffff");

        let buf = render_dom(&dom, Size { w: 160.0, h: 64.0 }, bg);

        // Tally pixels by class.
        let (mut full_ink, mut pure_bg, mut intermediate) = (0usize, 0usize, 0usize);
        for y in 0..buf.height() {
            for x in 0..buf.width() {
                let [r, g, b, _a] = buf.pixel(x, y);
                let v = r.max(g).max(b);
                if v == 0 {
                    pure_bg += 1;
                } else if r == 255 && g == 255 && b == 255 {
                    full_ink += 1;
                } else {
                    intermediate += 1;
                }
            }
        }
        println!(
            "render_dom(\"Ag\", 32px) -> {}x{}: full_ink={full_ink}, intermediate(AA)={intermediate}, bg={pure_bg}",
            buf.width(),
            buf.height()
        );

        assert!(full_ink > 0, "expected fully-inked glyph cores");
        assert!(pure_bg > 0, "expected untouched background");
        // THE antialiasing assertion: strictly-between-bg-and-ink pixels exist.
        assert!(
            intermediate > 0,
            "expected antialiased intermediate-gray pixels (a 1-bit font cannot \
             produce these); got 0"
        );

        // Write a viewable PPM artifact next to the crate's target dir.
        let ppm = buf.to_ppm();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("ag.ppm");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &ppm).unwrap();
        println!(
            "CONFIRMED antialiasing: {intermediate} intermediate-gray pixels. \
             Wrote PPM ({} bytes) to {}",
            ppm.len(),
            path.display()
        );
    }

    /// A background rect under the text must show through the feathered AA edges
    /// (the composite reads the destination, not the clear color).
    #[test]
    fn aa_edges_feather_over_a_painted_rect() {
        let bg = color(0, 0, 0);
        let panel = color(40, 40, 40);
        let mut engine = TextEngine::new();

        let mut buf = Buffer::new(160, 64);
        buf.clear(bg);
        // Paint a mid-gray panel, then composite white text over it.
        buf.fill_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 160.0, h: 64.0 },
            },
            panel,
        );
        let glyphs = engine.rasterize("Ag", 32.0, color(255, 255, 255));
        composite_coverage(
            &mut buf,
            Point { x: 0.0, y: 0.0 },
            &glyphs.coverage,
            glyphs.width,
            glyphs.height,
            color(255, 255, 255),
        );

        // Some pixel must be strictly between the panel gray (40) and full ink
        // (255) — i.e. an edge blended over the panel, not the clear color.
        let feathered = (0..buf.height()).any(|y| {
            (0..buf.width()).any(|x| {
                let [r, _, _, _] = buf.pixel(x, y);
                r > panel.r && r < 255
            })
        });
        assert!(feathered, "AA edge must blend over the painted panel");
    }

    /// The `Renderer` impl paints the same antialiased result as `render_dom`.
    #[test]
    fn renderer_impl_paints_antialiased_text() {
        let bg = color(0, 0, 0);
        let dom = dom_with_text("Ag", "32", "#ffffff");
        let (scene, _) = canopy_layout_taffy::layout(&dom, Size { w: 160.0, h: 64.0 });

        let mut r = TextRenderer::new(160, 64, bg);
        r.render(&scene).unwrap();
        let buf = r.buffer();

        let intermediate = (0..buf.height())
            .flat_map(|y| (0..buf.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let [r, g, b, _] = buf.pixel(x, y);
                let v = r.max(g).max(b);
                v != 0 && !(r == 255 && g == 255 && b == 255)
            })
            .count();
        assert!(
            intermediate > 0,
            "Renderer impl must produce antialiased pixels too"
        );
    }
}
