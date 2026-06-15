//! Canopy headless host loop: the part of a host that owns the retained tree and a
//! renderer but no window.
//!
//! A [`Host`] ties together the two host-side seams already built: it holds a
//! [`canopy_dom::Dom`] (the [`canopy_traits::OpSink`] that turns op bytes into a
//! retained tree) and a [`canopy_traits::Renderer`]. [`Host::apply`] feeds a batch
//! of op bytes into the `Dom`; [`Host::paint`] runs [`canopy_paint::build_scene`]
//! over the current tree and hands the resulting [`canopy_traits::DisplayList`] to
//! the renderer, then presents it.
//!
//! It is `no_std` + `alloc` and renderer-generic, so the same loop drives the
//! software rasterizer in a test, a GPU backend on the desktop, or a framebuffer on
//! bare metal. The transport that delivers the op bytes and a windowed,
//! [`canopy_traits::Platform`]-driven event loop layer on top of this; this crate is
//! just the apply→paint core.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use canopy_dom::Dom;
use canopy_paint::build_scene;
use canopy_traits::{HostError, OpSink, Renderer, Size};

/// A headless host: a retained [`Dom`] plus a [`Renderer`] `R`.
///
/// Drive it by [`apply`](Host::apply)ing op batches and then
/// [`paint`](Host::paint)ing a viewport.
pub struct Host<R: Renderer> {
    dom: Dom,
    renderer: R,
}

impl<R: Renderer> Host<R> {
    /// New host wrapping `renderer` over an empty tree.
    pub fn new(renderer: R) -> Self {
        Self {
            dom: Dom::new(),
            renderer,
        }
    }

    /// Apply one encoded op batch to the retained tree.
    ///
    /// Forwards straight to the `Dom`'s [`OpSink`], so a forged handle or a corrupt
    /// stream surfaces as [`HostError::BadHandle`] / [`HostError::Decode`] here.
    pub fn apply(&mut self, ops: &[u8]) -> Result<(), HostError> {
        self.dom.apply(ops)
    }

    /// Build the scene for `viewport` from the current tree and paint it.
    ///
    /// Walks the `Dom` into a [`DisplayList`](canopy_traits::DisplayList) via
    /// [`build_scene`], hands it to the renderer, and presents the frame. Renderer
    /// or present failures propagate as [`HostError`].
    pub fn paint(&mut self, viewport: Size) -> Result<(), HostError> {
        let scene = build_scene(&self.dom, viewport);
        self.renderer.render(&scene)?;
        self.renderer.present()
    }

    /// Borrow the retained tree (for queries / tests).
    pub fn dom(&self) -> &Dom {
        &self.dom
    }

    /// Borrow the renderer (e.g. to read its frame buffer).
    pub fn renderer(&self) -> &R {
        &self.renderer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_core::Emitter;
    use canopy_paint::{BG, FG};
    use canopy_protocol::ElementTag;
    use canopy_render_soft::SoftwareRenderer;
    use canopy_traits::Color;

    #[test]
    fn applies_a_batch_and_paints_the_column_background() {
        // Guest builds a column with a styled text child.
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(canopy_dom::ROOT, col);
        e.set_inline_style(col, BG, "#202830");
        let label = e.create_text("hi");
        e.append(col, label);
        e.set_inline_style(label, FG, "#ffd040");
        let batch = e.take_batch(0);

        // A host over a software renderer that clears to opaque black.
        let clear = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let mut host = Host::new(SoftwareRenderer::new(64, 32, clear));

        host.apply(&batch).unwrap();
        // The batch produced exactly the column element and its text child.
        assert_eq!(host.dom().node_count(), 2);

        host.paint(Size { w: 64.0, h: 32.0 }).unwrap();

        // The column background spans the viewport width (64px) and the child's
        // height (16px). The text child's box only reaches x=16 ("hi" -> 2 chars *
        // 8px), so a pixel to its right is pure column background while a pixel under
        // the text shows the text's foreground color.
        let buf = host.renderer().buffer();
        assert_eq!(
            buf.pixel(40, 0),
            [0x20, 0x28, 0x30, 255],
            "column background"
        );
        assert_eq!(buf.pixel(0, 0), [0xff, 0xd0, 0x40, 255], "text foreground");
    }

    #[test]
    fn paint_with_empty_tree_clears_to_background() {
        let clear = Color {
            r: 10,
            g: 20,
            b: 30,
            a: 255,
        };
        let mut host = Host::new(SoftwareRenderer::new(8, 8, clear));
        host.paint(Size { w: 8.0, h: 8.0 }).unwrap();
        assert_eq!(host.renderer().buffer().pixel(0, 0), [10, 20, 30, 255]);
    }
}
