//! L3 **PAINT**: rasterize the cascaded + laid-out Stylo tree to pixels.
//!
//! The crate's L1 (cascade, [`StyloEngine::resolve_styles`]) and L2
//! ([`StyloEngine::layout`]) stages produce, for every element in DFS order, a
//! flat [`ComputedStyle`](canopy_traits::ComputedStyle) and an absolute
//! border-box [`Rect`](canopy_traits::Rect). This stage zips those two streams and
//! paints them into a [`canopy_render_soft::Buffer`] — the same CPU rasterizer the
//! rest of the host uses — so the full style → layout → paint path is exercised end
//! to end without a GPU or a window.
//!
//! ## Paint order
//!
//! Backgrounds are painted **back-to-front**. [`StyloEngine::element_dfs_order`] is
//! pre-order (parent before child), which is exactly the right order for opaque
//! backgrounds: a child's box is painted *after* (i.e. on top of) its ancestor's, so
//! a nested element correctly draws over its parent. Text for an element is blitted
//! immediately after that element's background, at the box origin, using the
//! element's resolved foreground `color` and `font_size`.

use canopy_render_soft::Buffer;
use canopy_traits::{Color, Size};

use crate::{NodeKind, StyloEngine};

impl StyloEngine {
    /// Render the cascaded + laid-out tree into an RGBA8 [`Buffer`] of size
    /// `viewport`.
    ///
    /// Runs [`layout`](StyloEngine::layout) (which itself resolves styles), then for
    /// each `(slab, rect)` pair reads the element's flat
    /// [`ComputedStyle`](canopy_traits::ComputedStyle) and paints:
    ///   * its background as a filled rect (only when `background.a > 0`), and
    ///   * its text — if the element has a direct [`NodeKind::Text`] child — blitted
    ///     at the box origin in the element's foreground `color` at `font_size`.
    ///
    /// The buffer is cleared to opaque white first so painted boxes sit on a defined,
    /// reftest-stable background.
    pub fn render(&mut self, viewport: Size) -> Buffer {
        let rects = self.layout(viewport);
        let order = self.element_dfs_order();

        let mut buffer = Buffer::new(viewport.w as usize, viewport.h as usize);
        buffer.clear(Color {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        });

        // DFS order is parent-before-child, the correct back-to-front order for
        // opaque backgrounds.
        for (&slab, &rect) in order.iter().zip(rects.iter()) {
            let Some(style) = self.computed_style_for(slab) else {
                continue;
            };

            // Background (skip fully transparent fills so they don't clobber what is
            // behind them with the source color at alpha 0).
            if style.background.a > 0 {
                buffer.fill_round_rect(rect, style.background, 0.0);
            }

            // Text: an element with a direct text child renders that text at the box
            // origin in the element's resolved foreground color + font size.
            if let Some(text) = self.direct_text_child(slab) {
                buffer.blit_text(rect.origin, &text, style.color, style.font_size);
            }
        }

        buffer
    }

    /// Render to raw RGBA bytes plus dimensions, for later reftest pixel comparison.
    ///
    /// Returns `(rgba, width, height)` where `rgba` is row-major RGBA8 — the buffer's
    /// [`data`](Buffer::data) copied out.
    pub fn render_to_rgba(&mut self, viewport: Size) -> (Vec<u8>, usize, usize) {
        let buffer = self.render(viewport);
        let w = viewport.w as usize;
        let h = viewport.h as usize;
        (buffer.data().to_vec(), w, h)
    }

    /// The text of this element's first **direct** [`NodeKind::Text`] child, if any.
    fn direct_text_child(&self, slab: usize) -> Option<String> {
        let node = self.doc.nodes.get(slab)?;
        for &child in &node.children {
            if let NodeKind::Text(text) = &self.doc.nodes[child].kind {
                return Some(text.clone());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_traits::Size;

    /// THE smoke test: a `width:100px; height:50px; background:#ff0000` box, rendered
    /// into a 120×60 viewport, must put a red pixel inside the box.
    #[test]
    fn renders_a_red_box() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        // Root element wrapper (matches the cascade's "first element child of node 0"
        // root rule), then the styled box under it.
        let html = doc.add_element(0, "html", None, &[]);
        let boxed = doc.add_element(html, "div", None, &[]);
        engine
            .document_mut()
            .set_inline_style(boxed, "width:100px; height:50px; background:#ff0000");

        let buffer = engine.render(Size { w: 120.0, h: 60.0 });
        let data = buffer.data();
        let w = 120usize;

        // A pixel well inside the 100×50 box at the origin.
        let (x, y) = (10usize, 10usize);
        let i = (y * w + x) * 4;
        let (r, g, b, a) = (data[i], data[i + 1], data[i + 2], data[i + 3]);
        assert!(
            r >= 250 && g <= 5 && b <= 5,
            "pixel at ({x},{y}) should be red, got rgba=({r},{g},{b},{a})"
        );

        // And a pixel outside the box keeps the white clear color.
        let (ox, oy) = (110usize, 55usize);
        let oi = (oy * w + ox) * 4;
        assert_eq!(
            (data[oi], data[oi + 1], data[oi + 2]),
            (255, 255, 255),
            "pixel outside the box should be the white background"
        );
    }
}
