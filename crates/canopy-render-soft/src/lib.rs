//! Canopy software renderer: a CPU rasterizer that paints a [`DisplayList`] into an
//! RGBA8888 buffer.
//!
//! It implements the same [`canopy_traits::Renderer`] trait the GPU backend will,
//! so the rest of the host is renderer-agnostic. Bringing it up first has two
//! payoffs: it validates the `Renderer` seam without a GPU or a window (so the whole
//! pipeline stays unit-testable), and it *is* the Tier-2 / bare-metal renderer — the
//! same path that later swaps in `tiny-skia` / `vello_cpu` for antialiased quality.
//!
//! It fills opaque rectangles (no antialiasing) — **including true rounded corners**
//! when a [`DisplayItem::Rect`] carries a positive `radius` (see
//! [`Buffer::fill_round_rect`]) — and blits a baked 8x8 bitmap font
//! ([`canopy_text_baked`]) for [`DisplayItem::Text`] runs. Shaped [`DisplayItem::Glyphs`]
//! runs (the capable-tier path) are not yet rasterized here.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use canopy_text_baked::{glyph, CELL_H, CELL_W};
use canopy_traits::{Color, DisplayItem, DisplayList, HostError, Point, Rect, Renderer, Size};

/// Composite `src` (straight-alpha RGBA) **over** the destination pixel `dst`
/// (`[r, g, b, a]`) in place, the classic Porter–Duff "source over".
///
/// Each color channel becomes `src·a + dst·(1 − a)` with `a = src.a/255`, evaluated
/// in pure integer math as `(src·a + dst·(255 − a) + 127) / 255` — the `+127` gives
/// round-to-nearest so a half-alpha blend lands on the true midpoint instead of
/// drifting dark. This is the same rounding the text compositor uses, kept here so a
/// reduced-alpha fill fades over the background rather than overwriting it. No float,
/// no `f32::round`, no `unsafe` — `no_std`-clean.
///
/// The destination alpha is composited the same way against a notionally opaque
/// source coverage (`255`), so stacking translucent fills keeps the buffer's alpha
/// channel sensible (it trends toward opaque), which matters when the surface is
/// later alpha-composited onto another (e.g. a plugin panel into the frame).
fn blend_over(dst: &mut [u8], src: Color) {
    let a = u32::from(src.a);
    let inv = 255 - a;
    let mix = |s: u8, d: u8| -> u8 { ((u32::from(s) * a + u32::from(d) * inv + 127) / 255) as u8 };
    dst[0] = mix(src.r, dst[0]);
    dst[1] = mix(src.g, dst[1]);
    dst[2] = mix(src.b, dst[2]);
    // Resulting coverage: a + dst·(1 − a), i.e. `mix(255, dst.a)`.
    dst[3] = ((255 * a + u32::from(dst[3]) * inv + 127) / 255) as u8;
}

/// An RGBA8888 pixel buffer, row-major, 4 bytes per pixel.
pub struct Buffer {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

impl Buffer {
    /// Allocate a transparent-black buffer.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0; width * height * 4],
        }
    }

    /// Width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The raw RGBA bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Read one pixel as `[r, g, b, a]`. Out-of-bounds reads return zeros.
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        let i = (y * self.width + x) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    /// Overwrite one pixel with `[r, g, b, a]` (a **straight store**, no blending),
    /// clipped to the buffer.
    ///
    /// This is the write counterpart to [`pixel`](Buffer::pixel): a caller that has
    /// already done its own compositing (it read the destination, blended, and holds
    /// the final value) writes the result back here so [`fill_rect`](Buffer::fill_rect)'s
    /// alpha blend does not double-apply. The text compositor in `canopy-render-text`
    /// uses exactly this pairing.
    pub fn set_pixel(&mut self, x: usize, y: usize, rgba: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = (y * self.width + x) * 4;
        self.data[i..i + 4].copy_from_slice(&rgba);
    }

    /// Fill the whole buffer with one color.
    pub fn clear(&mut self, c: Color) {
        for px in self.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[c.r, c.g, c.b, c.a]);
        }
    }

    /// Fill `rect` with `c`, **src-over alpha-blended** over whatever is already in
    /// the buffer, clipped to the buffer. `f32 as usize` saturates negatives to 0, so
    /// off-screen origins clip cleanly.
    ///
    /// When `c.a == 255` (the common opaque case) the span is a straight overwrite,
    /// exactly the original behavior. For a translucent `c` each destination pixel
    /// becomes `src·a + dst·(1 − a)` per channel (see [`blend_over`]), so a
    /// reduced-alpha fill — the kind a faded-in element emits — composites over the
    /// background instead of punching through it.
    pub fn fill_rect(&mut self, rect: Rect, c: Color) {
        let x0 = (rect.origin.x as usize).min(self.width);
        let y0 = (rect.origin.y as usize).min(self.height);
        let x1 = ((rect.origin.x + rect.size.w) as usize).min(self.width);
        let y1 = ((rect.origin.y + rect.size.h) as usize).min(self.height);
        let opaque = c.a == 255;
        for y in y0..y1 {
            let start = (y * self.width + x0) * 4;
            let end = (y * self.width + x1) * 4;
            for px in self.data[start..end].chunks_exact_mut(4) {
                if opaque {
                    px.copy_from_slice(&[c.r, c.g, c.b, c.a]);
                } else {
                    blend_over(px, c);
                }
            }
        }
    }

    /// Fill `rect` with an opaque color, **rounding the four corners** to `radius`
    /// logical px, clipped to the buffer.
    ///
    /// The body is a plain [`fill_rect`](Buffer::fill_rect); the rounding is carved
    /// out by skipping any pixel that falls inside a corner's `radius`×`radius`
    /// square but *outside* that corner's quarter-circle. Each corner has an
    /// arc-center inset `radius` px from the corner along both axes; a pixel center
    /// at `(px, py)` is "in the corner cut" when its squared distance from the
    /// nearest arc-center exceeds `radius²`. We compare **squared** distances so
    /// there is no `f32::sqrt` (a `std`-only intrinsic) — keeping this `no_std`.
    ///
    /// `radius` is clamped to half the rect's shorter side, so an oversized radius
    /// produces a pill/stadium (or a circle for a square rect) rather than
    /// overflowing. A non-positive radius falls through to a square
    /// [`fill_rect`](Buffer::fill_rect), so callers can pass the display item's
    /// radius unconditionally.
    ///
    /// Like [`fill_rect`](Buffer::fill_rect), the body is **src-over alpha-blended**:
    /// an opaque `c` overwrites (the original behavior) while a translucent `c` blends
    /// over the destination, so a faded rounded card composites correctly. Only the
    /// pixels *inside* the quarter-circle corners are written — the carved-away corner
    /// pixels keep whatever was behind them.
    pub fn fill_round_rect(&mut self, rect: Rect, c: Color, radius: f32) {
        // Clamp to half the shorter side; a non-positive radius is just a square.
        let max_r = 0.5 * rect.size.w.min(rect.size.h);
        let r = radius.min(max_r);
        if r <= 0.0 {
            self.fill_rect(rect, c);
            return;
        }
        let r2 = r * r;
        let opaque = c.a == 255;

        // Pixel-space bounds (saturating `f32 as usize` clips off-screen origins).
        let x0 = (rect.origin.x as usize).min(self.width);
        let y0 = (rect.origin.y as usize).min(self.height);
        let x1 = ((rect.origin.x + rect.size.w) as usize).min(self.width);
        let y1 = ((rect.origin.y + rect.size.h) as usize).min(self.height);

        // Arc-centers: `r` px in from each edge, in absolute logical coordinates.
        let left = rect.origin.x + r;
        let right = rect.origin.x + rect.size.w - r;
        let top = rect.origin.y + r;
        let bottom = rect.origin.y + rect.size.h - r;

        for y in y0..y1 {
            // Pixel center on the y axis.
            let cy = y as f32 + 0.5;
            // Which corner band (if any) this row is in; `None` => fully inside the
            // straight middle, so the whole span fills with no per-pixel test.
            let arc_cy = if cy < top {
                Some(top)
            } else if cy > bottom {
                Some(bottom)
            } else {
                None
            };
            let start = (y * self.width + x0) * 4;
            let end = (y * self.width + x1) * 4;
            let row = &mut self.data[start..end];
            for (i, px) in row.chunks_exact_mut(4).enumerate() {
                if let Some(acy) = arc_cy {
                    let cx = (x0 + i) as f32 + 0.5;
                    let acx = if cx < left {
                        Some(left)
                    } else if cx > right {
                        Some(right)
                    } else {
                        None
                    };
                    if let Some(acx) = acx {
                        // In a true corner: skip pixels beyond the quarter-circle.
                        let dx = cx - acx;
                        let dy = cy - acy;
                        if dx * dx + dy * dy > r2 {
                            continue;
                        }
                    }
                }
                if opaque {
                    px.copy_from_slice(&[c.r, c.g, c.b, c.a]);
                } else {
                    blend_over(px, c);
                }
            }
        }
    }

    /// Stroke a `width`-thick frame **inside** `rect` in `c`, src-over blended,
    /// clipped to the buffer.
    ///
    /// This is the CPU tier's degraded [`DisplayItem::Border`] rasterization: the
    /// four edge bands (top, bottom, left, right) are filled as solid rectangles so
    /// the box reads as framed. A non-positive `width` paints nothing; `width` is
    /// clamped to half the rect's shorter side so the bands never overlap into a
    /// solid fill. `radius` is accepted for signature parity with the rounded fill
    /// but **not** carved here — the corners stay square, a faithful approximation
    /// (the GPU tier draws true rounded strokes). Each band reuses
    /// [`fill_rect`](Buffer::fill_rect), so a translucent border color blends.
    pub fn stroke_rect(&mut self, rect: Rect, c: Color, width: f32, radius: f32) {
        let _ = radius; // square-cornered approximation on the CPU tier.
        if width <= 0.0 {
            return;
        }
        // Clamp the stroke to half the shorter side so opposite bands can't overlap.
        let w = width.min(0.5 * rect.size.w.min(rect.size.h));
        if w <= 0.0 {
            return;
        }
        let Rect { origin, size } = rect;
        // Top and bottom bands span the full width; left/right fill the gap between
        // them, so no corner pixel is painted twice.
        let top = Rect {
            origin,
            size: Size { w: size.w, h: w },
        };
        let bottom = Rect {
            origin: Point {
                x: origin.x,
                y: origin.y + size.h - w,
            },
            size: Size { w: size.w, h: w },
        };
        let left = Rect {
            origin: Point {
                x: origin.x,
                y: origin.y + w,
            },
            size: Size {
                w,
                h: size.h - 2.0 * w,
            },
        };
        let right = Rect {
            origin: Point {
                x: origin.x + size.w - w,
                y: origin.y + w,
            },
            size: Size {
                w,
                h: size.h - 2.0 * w,
            },
        };
        self.fill_rect(top, c);
        self.fill_rect(bottom, c);
        self.fill_rect(left, c);
        self.fill_rect(right, c);
    }

    /// Paint one opaque pixel, clipped to the buffer.
    fn put_pixel(&mut self, x: usize, y: usize, c: Color) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = (y * self.width + x) * 4;
        self.data[i..i + 4].copy_from_slice(&[c.r, c.g, c.b, c.a]);
    }

    /// Blit `text` as a baked-bitmap-font run in `color`, starting at `origin` and
    /// advancing one cell per character.
    ///
    /// Only "ink" bits are painted; non-ink pixels are left untouched so the element
    /// background shows through. `size` is the target cell height in pixels; the
    /// font is 8px tall, so the integer scale is `max(1, (size / 8).floor())`. Each
    /// source pixel is drawn as a `scale`×`scale` block.
    pub fn blit_text(&mut self, origin: Point, text: &str, color: Color, size: f32) {
        let scale = ((size / CELL_H as f32) as usize).max(1);
        let advance = CELL_W as usize * scale;
        let ox = origin.x as usize;
        let oy = origin.y as usize;
        for (col, ch) in text.chars().enumerate() {
            let bitmap = glyph(ch);
            let cell_x = ox + col * advance;
            for (row, bits) in bitmap.iter().enumerate() {
                for bit in 0..CELL_W as usize {
                    // bit 7 (0x80) is the leftmost pixel.
                    if bits & (0x80 >> bit) != 0 {
                        let px0 = cell_x + bit * scale;
                        let py0 = oy + row * scale;
                        for dy in 0..scale {
                            for dx in 0..scale {
                                self.put_pixel(px0 + dx, py0 + dy, color);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Encode as a binary PPM (P6) — a tiny, viewable artifact with no dependencies.
    pub fn to_ppm(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(
            alloc::format!("P6\n{} {}\n255\n", self.width, self.height).as_bytes(),
        );
        for px in self.data.chunks_exact(4) {
            out.extend_from_slice(&px[0..3]);
        }
        out
    }
}

/// A [`Renderer`] that rasterizes into an owned [`Buffer`].
pub struct SoftwareRenderer {
    buffer: Buffer,
    clear: Color,
}

impl SoftwareRenderer {
    /// New renderer with a `clear` background color.
    pub fn new(width: usize, height: usize, clear: Color) -> Self {
        Self {
            buffer: Buffer::new(width, height),
            clear,
        }
    }

    /// The current frame buffer.
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Mutable access to the frame buffer — e.g. to composite another surface (a
    /// plugin panel) into the painted frame before presenting.
    pub fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffer
    }
}

impl Renderer for SoftwareRenderer {
    fn resize(&mut self, size: Size) {
        self.buffer = Buffer::new(size.w as usize, size.h as usize);
    }

    fn render(&mut self, scene: &DisplayList) -> Result<(), HostError> {
        self.buffer.clear(self.clear);
        for item in &scene.items {
            match item {
                DisplayItem::Rect {
                    rect,
                    color,
                    radius,
                } => {
                    // Square is the common case; only pay the per-pixel corner test
                    // when a positive radius is actually requested.
                    if *radius > 0.0 {
                        self.buffer.fill_round_rect(*rect, *color, *radius);
                    } else {
                        self.buffer.fill_rect(*rect, *color);
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
                    // Center / right-align the baked run within its box using the
                    // run's OWN baked width (chars * advance at this size), offset by
                    // `(box_w - run_w) * align` clamped to >= 0. For the baked path the
                    // run usually equals the box, so the offset is ~0; `align == 0.0`
                    // is byte-for-byte the legacy left-aligned blit.
                    let scale = ((size / CELL_H as f32) as usize).max(1);
                    let advance = (CELL_W as usize * scale) as f32;
                    let run_w = text.chars().count() as f32 * advance;
                    let origin = Point {
                        x: origin.x + ((box_w - run_w) * align).max(0.0),
                        y: origin.y,
                    };
                    self.buffer.blit_text(origin, text, *color, *size)
                }
                DisplayItem::Border {
                    rect,
                    color,
                    width,
                    radius,
                } => {
                    // Degraded frame: stroke the four edge bands. The CPU tier has no
                    // rounded-corner stroke, so a positive radius is not carved (the
                    // corners stay square) — a faithful, never-panicking approximation.
                    self.buffer.stroke_rect(*rect, *color, *width, *radius);
                }
                DisplayItem::Gradient {
                    rect,
                    stops,
                    direction: _,
                } => {
                    // Degraded fill: the CPU tier does not interpolate, so fill the box
                    // with the first stop's solid color (transparent if the set is
                    // empty). The direction is irrelevant to a flat fill.
                    self.buffer.fill_rect(*rect, stops.first_color());
                }
                // A soft shadow needs a blur the CPU tier doesn't implement; dropping
                // it is a faithful degradation (it is purely decorative) and never panics.
                DisplayItem::Shadow { .. } => {}
                // Shaped-glyph rasterization arrives with the capable-tier text backend.
                DisplayItem::Glyphs { .. } => {}
                // `DisplayItem` is `#[non_exhaustive]`: a future primitive added to
                // the seam reaches this out-of-tree-style arm. Skipping an unknown
                // item is the safe degradation — paint nothing rather than panic.
                _ => {}
            }
        }
        Ok(())
    }

    fn present(&mut self) -> Result<(), HostError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_traits::Point;

    #[test]
    fn fills_a_clipped_rect() {
        let mut b = Buffer::new(10, 10);
        b.fill_rect(
            Rect {
                origin: Point { x: 8.0, y: 8.0 },
                size: Size { w: 100.0, h: 100.0 },
            },
            Color {
                r: 1,
                g: 2,
                b: 3,
                a: 255,
            },
        );
        assert_eq!(b.pixel(9, 9), [1, 2, 3, 255]);
        assert_eq!(b.pixel(0, 0), [0, 0, 0, 0]);
        assert_eq!(b.pixel(50, 50), [0, 0, 0, 0]); // out of bounds -> zeros
    }

    #[test]
    fn half_alpha_fill_blends_over_opaque_background() {
        // THE alpha-compositing proof: a 50%-alpha white fill over a known opaque
        // background yields the channel-wise midpoint (src·a + dst·(1−a)), not a
        // straight overwrite. This is what makes a faded-in element actually fade.
        let mut b = Buffer::new(4, 4);
        let bg = Color {
            r: 0,
            g: 100,
            b: 200,
            a: 255,
        };
        b.clear(bg);
        // White at alpha 128 (~0.502).
        let src = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 128,
        };
        b.fill_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 4.0, h: 4.0 },
            },
            src,
        );
        // mix(s, d) = (s*128 + d*127 + 127) / 255.
        // r: (255*128 + 0*127 + 127)/255 = 128
        // g: (255*128 + 100*127 + 127)/255 = 178
        // b: (255*128 + 200*127 + 127)/255 = 228
        // a: (255*128 + 255*127 + 127)/255 = 255
        assert_eq!(b.pixel(0, 0), [128, 178, 228, 255]);
        // Every covered pixel got the same blend.
        assert_eq!(b.pixel(3, 3), [128, 178, 228, 255]);
    }

    #[test]
    fn opaque_fill_still_overwrites_exactly() {
        // The fast path (a == 255) must remain a byte-exact overwrite, so opaque
        // scenes are unchanged by the new blend path.
        let mut b = Buffer::new(2, 2);
        b.clear(Color {
            r: 10,
            g: 20,
            b: 30,
            a: 255,
        });
        let src = Color {
            r: 200,
            g: 150,
            b: 100,
            a: 255,
        };
        b.fill_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 2.0, h: 2.0 },
            },
            src,
        );
        assert_eq!(
            b.pixel(0, 0),
            [200, 150, 100, 255],
            "opaque overwrites exactly"
        );
        assert_eq!(b.pixel(1, 1), [200, 150, 100, 255]);
    }

    #[test]
    fn zero_alpha_fill_leaves_the_background() {
        // A fully transparent fill is a no-op on the color channels (the limit of the
        // blend as a -> 0).
        let mut b = Buffer::new(2, 2);
        let bg = Color {
            r: 5,
            g: 6,
            b: 7,
            a: 255,
        };
        b.clear(bg);
        b.fill_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 2.0, h: 2.0 },
            },
            Color {
                r: 255,
                g: 255,
                b: 255,
                a: 0,
            },
        );
        assert_eq!(
            b.pixel(0, 0),
            [5, 6, 7, 255],
            "alpha 0 leaves the background"
        );
    }

    #[test]
    fn half_alpha_round_rect_blends_center_keeps_carved_corner() {
        // The rounded fill blends like the square one: the center is the half-mix,
        // while a carved-away corner is untouched (keeps the background).
        let mut b = Buffer::new(40, 40);
        let bg = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        b.clear(bg);
        let src = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 128,
        };
        b.fill_round_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 40.0, h: 40.0 },
            },
            src,
            12.0,
        );
        // Center: white@128 over black -> (255*128 + 0*127 + 127)/255 = 128 per RGB.
        assert_eq!(
            b.pixel(20, 20),
            [128, 128, 128, 255],
            "center is the half-mix"
        );
        // Extreme corner is carved away: stays the black background.
        assert_eq!(
            b.pixel(0, 0),
            [0, 0, 0, 255],
            "carved corner keeps background"
        );
    }

    #[test]
    fn renders_text_ink_over_background() {
        // A known background so we can tell ink from untouched pixels.
        let bg = Color {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 255,
        };
        let ink = Color {
            r: 0xff,
            g: 0xd0,
            b: 0x40,
            a: 255,
        };
        let mut r = SoftwareRenderer::new(32, 16, bg);
        let scene = DisplayList {
            items: vec![DisplayItem::Text {
                origin: Point { x: 0.0, y: 0.0 },
                text: "A".into(),
                color: ink,
                size: 8.0, // scale 1: glyph maps 1:1 onto the cell
                // box equals the single-cell run, left-aligned: no offset, so this
                // stays the legacy 1:1 blit at the origin.
                box_w: 8.0,
                align: 0.0,
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();

        // At scale 1 the 'A' apex is row 0 = 0x38 (0011_1000), so columns 2,3,4 are
        // ink while column 0 is clear.
        assert_eq!(buf.pixel(2, 0), [ink.r, ink.g, ink.b, ink.a], "ink pixel");

        // The top-left corner of the cell is a clear bit, so it keeps the cleared
        // background — text never paints non-ink pixels.
        assert_eq!(
            buf.pixel(0, 0),
            [bg.r, bg.g, bg.b, bg.a],
            "off-glyph pixel unchanged"
        );

        // Somewhere in the glyph there is at least one ink pixel equal to the color.
        let any_ink =
            (0..8).any(|y| (0..8).any(|x| buf.pixel(x, y) == [ink.r, ink.g, ink.b, ink.a]));
        assert!(any_ink, "expected at least one ink pixel");
    }

    /// THE text-align proof on the deterministic baked path: a single-cell run in a
    /// box much wider than the run, with `align = 0.5`, must have its ink land near
    /// the box center — not near x=0. The baked 'A' at scale 1 is one 8px cell; a
    /// `box_w` of 64 offsets it by `(64 - 8) * 0.5 = 28`, so the glyph cell occupies
    /// columns 28..36 (its leftmost lit bit is cell-column 0 -> absolute column 28),
    /// snug around the box midpoint (32), with the whole left region untouched
    /// background.
    #[test]
    fn text_align_center_centers_the_baked_run_in_its_box() {
        let bg = Color {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 255,
        };
        let ink = Color {
            r: 0xff,
            g: 0xff,
            b: 0xff,
            a: 255,
        };
        let box_w = 64.0_f32;
        let mut r = SoftwareRenderer::new(box_w as usize, 8, bg);
        let scene = DisplayList {
            items: vec![DisplayItem::Text {
                origin: Point { x: 0.0, y: 0.0 },
                text: "A".into(),
                color: ink,
                size: 8.0, // scale 1: one 8px cell
                box_w,
                align: 0.5, // centered
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();

        let is_ink = |x: usize, y: usize| buf.pixel(x, y) == [ink.r, ink.g, ink.b, ink.a];
        // The leftmost inked column across the whole buffer.
        let leftmost_ink = (0..buf.width())
            .find(|&x| (0..buf.height()).any(|y| is_ink(x, y)))
            .expect("the run must have inked some column");

        // Offset = (64 - 8) * 0.5 = 28; the 'A' glyph's leftmost lit bit is
        // cell-column 0 (rows 0xC6/0xFE), so the leftmost ink is at absolute column
        // 28 — snug around box center 32 and far from the left edge: proof the run is
        // centered, not left-stuck.
        assert_eq!(leftmost_ink, 28, "leftmost ink sits near the box center");
        let center = (box_w as usize) / 2; // 32
        assert!(
            leftmost_ink.abs_diff(center) <= 4,
            "leftmost ink {leftmost_ink} should be within a few px of center {center}"
        );

        // The entire left quarter of the box is untouched background — the glyphs are
        // not stuck at the left edge.
        for x in 0..(box_w as usize / 4) {
            for y in 0..buf.height() {
                assert_eq!(
                    buf.pixel(x, y),
                    [bg.r, bg.g, bg.b, bg.a],
                    "left region must stay background, ink at ({x},{y})"
                );
            }
        }
    }

    /// THE rounded-rect proof: a filled rounded rect over a contrasting background
    /// must leave its corner pixels showing the *background* while the center is the
    /// *fill* — i.e. the corner quadrants are genuinely carved away, not just drawn
    /// square.
    #[test]
    fn round_rect_clears_corners_keeps_center() {
        let bg = Color {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 255,
        };
        let fill = Color {
            r: 0xf3,
            g: 0x8b,
            b: 0xa8,
            a: 255,
        };
        let mut b = Buffer::new(40, 40);
        b.clear(bg);
        b.fill_round_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 40.0, h: 40.0 },
            },
            fill,
            12.0,
        );

        // The four extreme corner pixels lie well outside the quarter-circles, so
        // they keep the background.
        let bg_px = [bg.r, bg.g, bg.b, bg.a];
        assert_eq!(b.pixel(0, 0), bg_px, "top-left corner kept background");
        assert_eq!(b.pixel(39, 0), bg_px, "top-right corner kept background");
        assert_eq!(b.pixel(0, 39), bg_px, "bottom-left corner kept background");
        assert_eq!(
            b.pixel(39, 39),
            bg_px,
            "bottom-right corner kept background"
        );

        // The center is solidly inside the rect, so it is the fill color.
        let fill_px = [fill.r, fill.g, fill.b, fill.a];
        assert_eq!(b.pixel(20, 20), fill_px, "center is the fill color");

        // A mid-edge pixel (no corner rounding on the straight edges) is also fill.
        assert_eq!(b.pixel(20, 0), fill_px, "top mid-edge is straight (fill)");
        assert_eq!(b.pixel(0, 20), fill_px, "left mid-edge is straight (fill)");
    }

    /// The same guarantee through the full `Renderer` path: a `DisplayItem::Rect`
    /// carrying a positive `radius` paints rounded, clearing the corner to the
    /// clear color while the center is the fill. Writes a viewable PPM artifact.
    #[test]
    fn renderer_paints_rounded_rect_from_display_item() {
        let clear = Color {
            r: 0x18,
            g: 0x18,
            b: 0x24,
            a: 255,
        };
        let fill = Color {
            r: 0x89,
            g: 0xb4,
            b: 0xfa,
            a: 255,
        };
        let mut r = SoftwareRenderer::new(64, 64, clear);
        let scene = DisplayList {
            items: vec![DisplayItem::Rect {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size { w: 64.0, h: 64.0 },
                },
                color: fill,
                radius: 20.0,
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();

        // Corner shows the clear color (carved away); center is the fill.
        assert_eq!(
            buf.pixel(0, 0),
            [clear.r, clear.g, clear.b, clear.a],
            "rounded corner shows the clear color"
        );
        assert_eq!(
            buf.pixel(32, 32),
            [fill.r, fill.g, fill.b, fill.a],
            "center is the fill color"
        );

        // Write a viewable PPM artifact next to the crate's target dir (matches the
        // render-text test's artifact convention).
        let ppm = buf.to_ppm();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("round_rect.ppm");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &ppm).unwrap();
    }

    /// An oversized radius clamps to half the shorter side: a square rect becomes a
    /// circle, so its bounding-box corners are background and its center is fill.
    #[test]
    fn oversized_radius_clamps_to_a_circle() {
        let bg = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let fill = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        let mut b = Buffer::new(20, 20);
        b.clear(bg);
        // A radius far larger than the box clamps to 10 (half of 20) -> a circle.
        b.fill_round_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 20.0, h: 20.0 },
            },
            fill,
            1000.0,
        );
        assert_eq!(
            b.pixel(0, 0),
            [bg.r, bg.g, bg.b, bg.a],
            "corner outside circle"
        );
        assert_eq!(
            b.pixel(10, 10),
            [fill.r, fill.g, fill.b, fill.a],
            "center inside circle"
        );
        // The mid-edge touches the circle, so it is filled.
        assert_eq!(
            b.pixel(10, 0),
            [fill.r, fill.g, fill.b, fill.a],
            "mid-edge on circle"
        );
    }

    /// THE degraded-border proof: a `DisplayItem::Border` strokes a frame — the
    /// edge bands take the border color while the interior keeps the background, so
    /// the box reads as framed, not filled.
    #[test]
    fn border_strokes_a_frame_keeps_interior() {
        let bg = Color {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 255,
        };
        let frame = Color {
            r: 0xff,
            g: 0x00,
            b: 0x00,
            a: 255,
        };
        let mut r = SoftwareRenderer::new(20, 20, bg);
        let scene = DisplayList {
            items: vec![DisplayItem::Border {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size { w: 20.0, h: 20.0 },
                },
                color: frame,
                width: 3.0,
                radius: 0.0,
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();
        let frame_px = [frame.r, frame.g, frame.b, frame.a];
        let bg_px = [bg.r, bg.g, bg.b, bg.a];
        // Edge pixels (within the 3px band) are the frame color.
        assert_eq!(buf.pixel(0, 10), frame_px, "left edge is the border");
        assert_eq!(buf.pixel(19, 10), frame_px, "right edge is the border");
        assert_eq!(buf.pixel(10, 0), frame_px, "top edge is the border");
        assert_eq!(buf.pixel(10, 19), frame_px, "bottom edge is the border");
        // The interior (well inside the 3px band) keeps the background — a frame,
        // not a fill.
        assert_eq!(buf.pixel(10, 10), bg_px, "interior stays background");
    }

    /// A `DisplayItem::Gradient` degrades to a **solid fill of the first stop's
    /// color** on the CPU tier (no interpolation), so the whole box is painted with
    /// that color regardless of the gradient direction.
    #[test]
    fn gradient_degrades_to_first_stop_solid_fill() {
        use canopy_traits::{GradientDirection, GradientStop, GradientStops};
        let bg = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let first = Color {
            r: 0x12,
            g: 0x34,
            b: 0x56,
            a: 255,
        };
        let last = Color {
            r: 0xab,
            g: 0xcd,
            b: 0xef,
            a: 255,
        };
        let mut r = SoftwareRenderer::new(16, 16, bg);
        let scene = DisplayList {
            items: vec![DisplayItem::Gradient {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size { w: 16.0, h: 16.0 },
                },
                stops: GradientStops::from_slice(&[
                    GradientStop {
                        color: first,
                        position: 0.0,
                    },
                    GradientStop {
                        color: last,
                        position: 1.0,
                    },
                ]),
                direction: GradientDirection::Vertical,
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();
        let first_px = [first.r, first.g, first.b, first.a];
        // Every probed pixel is the first stop's color — a flat fill, not a ramp.
        assert_eq!(buf.pixel(0, 0), first_px, "top is the first stop");
        assert_eq!(buf.pixel(8, 8), first_px, "center is the first stop");
        assert_eq!(buf.pixel(15, 15), first_px, "bottom is the first stop");
    }

    /// A `DisplayItem::Shadow` is a no-op on the CPU tier: it never panics and leaves
    /// the background untouched (a faithful degradation of a soft shadow).
    #[test]
    fn shadow_is_a_no_op_on_cpu() {
        let bg = Color {
            r: 7,
            g: 8,
            b: 9,
            a: 255,
        };
        let mut r = SoftwareRenderer::new(8, 8, bg);
        let scene = DisplayList {
            items: vec![DisplayItem::Shadow {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size { w: 8.0, h: 8.0 },
                },
                color: Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 128,
                },
                blur: 4.0,
                offset: Point { x: 2.0, y: 2.0 },
            }],
        };
        r.render(&scene).unwrap();
        // The whole surface is still the clear background — the shadow drew nothing.
        let buf = r.buffer();
        for y in 0..buf.height() {
            for x in 0..buf.width() {
                assert_eq!(
                    buf.pixel(x, y),
                    [bg.r, bg.g, bg.b, bg.a],
                    "shadow must leave ({x},{y}) untouched"
                );
            }
        }
    }
}
