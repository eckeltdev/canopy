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

    /// Fill the whole buffer with one color.
    pub fn clear(&mut self, c: Color) {
        for px in self.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[c.r, c.g, c.b, c.a]);
        }
    }

    /// Fill `rect` with an opaque color, clipped to the buffer. `f32 as usize`
    /// saturates negatives to 0, so off-screen origins clip cleanly.
    pub fn fill_rect(&mut self, rect: Rect, c: Color) {
        let x0 = (rect.origin.x as usize).min(self.width);
        let y0 = (rect.origin.y as usize).min(self.height);
        let x1 = ((rect.origin.x + rect.size.w) as usize).min(self.width);
        let y1 = ((rect.origin.y + rect.size.h) as usize).min(self.height);
        for y in y0..y1 {
            let start = (y * self.width + x0) * 4;
            let end = (y * self.width + x1) * 4;
            for px in self.data[start..end].chunks_exact_mut(4) {
                px.copy_from_slice(&[c.r, c.g, c.b, c.a]);
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
    pub fn fill_round_rect(&mut self, rect: Rect, c: Color, radius: f32) {
        // Clamp to half the shorter side; a non-positive radius is just a square.
        let max_r = 0.5 * rect.size.w.min(rect.size.h);
        let r = radius.min(max_r);
        if r <= 0.0 {
            self.fill_rect(rect, c);
            return;
        }
        let r2 = r * r;

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
                px.copy_from_slice(&[c.r, c.g, c.b, c.a]);
            }
        }
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
                } => self.buffer.blit_text(*origin, text, *color, *size),
                // Shaped-glyph rasterization arrives with the capable-tier text backend.
                DisplayItem::Glyphs { .. } => {}
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
}
