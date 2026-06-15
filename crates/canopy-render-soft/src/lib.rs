//! Canopy software renderer: a CPU rasterizer that paints a [`DisplayList`] into an
//! RGBA8888 buffer.
//!
//! It implements the same [`canopy_traits::Renderer`] trait the GPU backend will,
//! so the rest of the host is renderer-agnostic. Bringing it up first has two
//! payoffs: it validates the `Renderer` seam without a GPU or a window (so the whole
//! pipeline stays unit-testable), and it *is* the Tier-2 / bare-metal renderer — the
//! same path that later swaps in `tiny-skia` / `vello_cpu` for antialiased quality.
//!
//! M1 fills opaque rectangles (no antialiasing, no glyph rasterization yet). Glyph
//! runs arrive with the text backend.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use canopy_traits::{Color, DisplayItem, DisplayList, HostError, Rect, Renderer, Size};

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
                // Glyph rasterization arrives with the text backend.
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
}
