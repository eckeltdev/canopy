//! Canopy headless host loop: the part of a host that owns the retained tree and a
//! renderer but no window.
//!
//! A [`Host`] ties together the host-side seams: it holds a [`canopy_dom::Dom`] (the
//! [`canopy_traits::OpSink`] that turns op bytes into a retained tree), a
//! [`SceneBuilder`], and a [`canopy_traits::Renderer`]. [`Host::apply`] feeds a batch
//! of op bytes into the `Dom`; [`Host::paint`] asks the scene builder to turn the
//! current tree into a [`canopy_traits::DisplayList`], hands that to the renderer, and
//! presents it.
//!
//! # The tier seam
//!
//! The [`SceneBuilder`] is what makes one host loop drive **either** style tier. The
//! constrained tier uses the default [`LiteSceneBuilder`] (a thin wrapper over
//! [`canopy_paint::build_scene`], which paints the `Dom`'s author-resolved inline
//! styles). A capable tier injects its own builder — e.g. one that runs the real Stylo
//! cascade over the `Dom` and emits its display list — via
//! [`with_scene_builder`](Host::with_scene_builder). This crate stays a workspace member
//! with no dependency on the heavy, excluded Stylo crate; the capable builder lives in an
//! excluded crate/example and plugs in through this trait.
//!
//! It is `no_std` + `alloc` and renderer-generic, so the same loop drives the software
//! rasterizer in a test, a GPU backend on the desktop, or a framebuffer on bare metal.
//! The transport that delivers the op bytes and a windowed,
//! [`canopy_traits::Platform`]-driven event loop layer on top of this; this crate is
//! just the apply→paint core.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use canopy_dom::Dom;
use canopy_paint::build_scene;
use canopy_traits::{DisplayList, HostError, OpSink, Renderer, Size};

/// Turns the retained [`Dom`] into a [`DisplayList`] for a `viewport` — the seam that
/// lets one [`Host`] loop drive either style tier.
///
/// `&mut self` so a stateful engine (e.g. a Stylo cascade that caches a resolved
/// document, or one that updates its device on a viewport change) can do real work per
/// frame; a stateless builder like [`LiteSceneBuilder`] simply ignores it.
pub trait SceneBuilder {
    /// Produce the display list to paint for `dom` at `viewport`.
    fn build_scene(&mut self, dom: &Dom, viewport: Size) -> DisplayList;
}

/// The default **constrained-tier** scene builder: paints the `Dom`'s author-resolved
/// inline styles via [`canopy_paint::build_scene`]. Stateless.
#[derive(Default, Clone, Copy)]
pub struct LiteSceneBuilder;

impl SceneBuilder for LiteSceneBuilder {
    fn build_scene(&mut self, dom: &Dom, viewport: Size) -> DisplayList {
        build_scene(dom, viewport)
    }
}

/// A headless host: a retained [`Dom`], a [`SceneBuilder`] `B`, and a [`Renderer`] `R`.
///
/// Drive it by [`apply`](Host::apply)ing op batches and then
/// [`paint`](Host::paint)ing a viewport. [`Host::new`] uses the constrained-tier
/// [`LiteSceneBuilder`]; [`with_scene_builder`](Host::with_scene_builder) injects a
/// capable-tier builder instead.
pub struct Host<B: SceneBuilder, R: Renderer> {
    dom: Dom,
    scene: B,
    renderer: R,
}

impl<R: Renderer> Host<LiteSceneBuilder, R> {
    /// New constrained-tier host wrapping `renderer` over an empty tree (paints inline
    /// styles via [`LiteSceneBuilder`]).
    pub fn new(renderer: R) -> Self {
        Self {
            dom: Dom::new(),
            scene: LiteSceneBuilder,
            renderer,
        }
    }
}

impl<B: SceneBuilder, R: Renderer> Host<B, R> {
    /// New host with an explicit `scene` builder — the capable-tier entry point (e.g. a
    /// Stylo-backed builder) over an empty tree.
    pub fn with_scene_builder(scene: B, renderer: R) -> Self {
        Self {
            dom: Dom::new(),
            scene,
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
    /// Asks the [`SceneBuilder`] to turn the `Dom` into a [`DisplayList`], hands it to
    /// the renderer, and presents the frame. Renderer or present failures propagate as
    /// [`HostError`].
    pub fn paint(&mut self, viewport: Size) -> Result<(), HostError> {
        let scene = self.scene.build_scene(&self.dom, viewport);
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

    /// Borrow the scene builder (e.g. to inspect a capable engine's state).
    pub fn scene_builder(&self) -> &B {
        &self.scene
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

    #[test]
    fn host_paints_through_an_injected_scene_builder() {
        use canopy_traits::{DisplayItem, DisplayList, Point, Rect};

        // A stub capable-style builder that ignores the Dom and paints one known rect —
        // proving Host::paint routes through the injected SceneBuilder, not the default
        // lite build_scene path. (A real capable builder would run Stylo here.)
        struct StubScene;
        impl SceneBuilder for StubScene {
            fn build_scene(&mut self, _dom: &Dom, viewport: Size) -> DisplayList {
                DisplayList {
                    items: alloc::vec![DisplayItem::Rect {
                        rect: Rect {
                            origin: Point { x: 0.0, y: 0.0 },
                            size: viewport,
                        },
                        color: Color {
                            r: 1,
                            g: 2,
                            b: 3,
                            a: 255,
                        },
                        radius: 0.0,
                    }],
                }
            }
        }

        let clear = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let mut host = Host::with_scene_builder(StubScene, SoftwareRenderer::new(8, 8, clear));
        host.paint(Size { w: 8.0, h: 8.0 }).unwrap();
        assert_eq!(
            host.renderer().buffer().pixel(4, 4),
            [1, 2, 3, 255],
            "painted the injected builder's rect, not the lite default"
        );
    }
}
