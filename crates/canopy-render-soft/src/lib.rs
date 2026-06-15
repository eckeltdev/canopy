//! Canopy software renderer: a CPU rasterizer that paints a [`DisplayList`] into an
//! RGBA8888 buffer.
//!
//! It implements the same [`canopy_traits::Renderer`] trait the GPU backend will,
//! so the rest of the host is renderer-agnostic. Bringing it up first has two
//! payoffs: it validates the `Renderer` seam without a GPU or a window (so the whole
//! pipeline stays unit-testable), and it *is* the Tier-2 / bare-metal renderer — the
//! same path that later swaps in `tiny-skia` / `vello_cpu` for antialiased quality.
//!
//! It fills opaque rectangles (no antialiasing) and blits a baked 8x8 bitmap font
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
}

impl Renderer for SoftwareRenderer {
    fn resize(&mut self, size: Size) {
        self.buffer = Buffer::new(size.w as usize, size.h as usize);
    }

    fn render(&mut self, scene: &DisplayList) -> Result<(), HostError> {
        self.buffer.clear(self.clear);
        for item in &scene.items {
            match item {
                DisplayItem::Rect { rect, color } => self.buffer.fill_rect(*rect, *color),
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
}
