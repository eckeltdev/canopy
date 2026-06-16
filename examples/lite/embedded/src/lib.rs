//! Canopy **on bare metal** — the lite tier's headline proof.
//!
//! This crate is `#![no_std]` (plus `alloc`), and it runs the **entire Canopy render
//! pipeline** — build a UI op-stream, apply it to a retained tree, lay it out, and
//! software-rasterize it into a pixel buffer — with **no `std`, no OS, no GPU, and no
//! allocator of its own**. It is exactly the code a microcontroller firmware would call
//! to draw a screen; here a tiny host binary ([`src/bin/render.rs`]) calls the same
//! functions and writes the frame to a PPM so you can see it on a desktop.
//!
//! The whole point is the constraint. Prove the pipeline compiles for a real bare-metal
//! Cortex-M target:
//!
//! ```text
//! rustup target add thumbv7em-none-eabi
//! cargo +nightly build --lib --target thumbv7em-none-eabi
//! ```
//!
//! That builds *this library* — the no_std pipeline — for a target with no operating
//! system. (`--lib` is deliberate: a library using `alloc` does not need to name a
//! global allocator or panic handler — the final firmware binary provides those — so it
//! cross-compiles cleanly as a build-proof. The `render` binary uses `std` only to write
//! the output file, so it is built for the host.)
//!
//! The pipeline, end to end, all no_std:
//! 1. [`build_batch`] — a [`canopy_core::Emitter`] records create/append/style ops into
//!    a compact byte batch (the same op-stream the WASM guests emit).
//! 2. [`build_dom`] — a [`canopy_dom::Dom`] applies the batch into a retained tree.
//! 3. [`canopy_paint::build_scene`] — the lite tier's hand-rolled flex lays the tree out
//!    into a [`DisplayList`](canopy_traits::DisplayList) of rects + text runs.
//! 4. [`canopy_render_soft::SoftwareRenderer`] — rasterizes that scene (rounded rects +
//!    the baked bitmap font) into an RGBA8 [`Buffer`].

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

use canopy_core::Emitter;
use canopy_dom::{Dom, ROOT};
use canopy_paint::{build_scene, BG, DIRECTION, FG, GAP, HEIGHT, PADDING, RADIUS, WIDTH};
use canopy_protocol::ElementTag;
use canopy_render_soft::SoftwareRenderer;
use canopy_traits::{Color, OpSink, Renderer, Size};

/// Logical frame size the demo scene is composed for.
pub const VIEW_W: usize = 480;
/// Logical frame height.
pub const VIEW_H: usize = 240;

/// The near-black canvas clear color (matches the `.canvas` background below).
pub const CLEAR: Color = Color {
    r: 0x10,
    g: 0x12,
    b: 0x18,
    a: 255,
};

/// Build the demo UI as a Canopy op-batch: a dark canvas with a title, a little row of
/// rounded color bars (a tiny chart), and a footer line. Returns the encoded ops — the
/// exact bytes a guest would hand the host. Pure `no_std` + `alloc`.
#[must_use]
pub fn build_batch() -> Vec<u8> {
    let mut e = Emitter::new();

    // The canvas: a padded dark column.
    let canvas = e.create_element(ElementTag::new(1));
    e.append(ROOT, canvas);
    e.set_inline_style(canvas, DIRECTION, "column");
    e.set_inline_style(canvas, WIDTH, "480");
    e.set_inline_style(canvas, HEIGHT, "240");
    e.set_inline_style(canvas, PADDING, "24");
    e.set_inline_style(canvas, GAP, "18");
    e.set_inline_style(canvas, BG, "#101218");

    // Title line (baked font; its `height` is the glyph size).
    let title = e.create_text("Canopy on bare metal");
    e.append(canvas, title);
    e.set_inline_style(title, HEIGHT, "16");
    e.set_inline_style(title, FG, "#e6e6ea");

    // A row of rounded color bars of varying heights — proves rects + radius + flex.
    let row = e.create_element(ElementTag::new(1));
    e.append(canvas, row);
    e.set_inline_style(row, DIRECTION, "row");
    e.set_inline_style(row, GAP, "14");

    // (background, height) per bar.
    let bars = [
        ("#a6e3a1", "120"),
        ("#94e2d5", "84"),
        ("#89b4fa", "150"),
        ("#f5c2e7", "64"),
        ("#fab387", "104"),
    ];
    for (color, h) in bars {
        let bar = e.create_element(ElementTag::new(1));
        e.append(row, bar);
        e.set_inline_style(bar, WIDTH, "56");
        e.set_inline_style(bar, HEIGHT, h);
        e.set_inline_style(bar, BG, color);
        e.set_inline_style(bar, RADIUS, "8");
    }

    // Footer line.
    let footer = e.create_text("no_std + alloc, software-rasterized");
    e.append(canvas, footer);
    e.set_inline_style(footer, HEIGHT, "12");
    e.set_inline_style(footer, FG, "#6c7086");

    e.take_batch(0)
}

/// Apply the op-batch into a retained [`Dom`]. Pure `no_std` + `alloc`.
#[must_use]
pub fn build_dom() -> Dom {
    let mut dom = Dom::new();
    dom.apply(&build_batch()).expect("apply ops");
    dom
}

/// **The whole no_std pipeline in one call**: build → apply → lay out → software-rasterize,
/// returning the raw RGBA8 framebuffer (`width * height * 4` bytes, row-major). This is the
/// buffer a device would blit straight to its display panel.
#[must_use]
pub fn render_rgba(width: usize, height: usize) -> Vec<u8> {
    let dom = build_dom();
    let viewport = Size {
        w: width as f32,
        h: height as f32,
    };
    let scene = build_scene(&dom, viewport);

    let mut renderer = SoftwareRenderer::new(width, height, CLEAR);
    renderer.render(&scene).expect("software render");
    renderer.buffer().data().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_dom::ROOT;

    #[test]
    fn dom_has_the_scene_structure() {
        let dom = build_dom();
        // The canvas is the single top-level child of ROOT, and it has three children:
        // title, the bar row, and the footer.
        let roots = dom.children(ROOT);
        assert_eq!(roots.len(), 1, "one canvas under ROOT");
        let canvas = roots[0];
        assert_eq!(
            dom.children(canvas).len(),
            3,
            "title + bar row + footer under the canvas"
        );
    }

    #[test]
    fn pipeline_rasterizes_a_nontrivial_frame() {
        let rgba = render_rgba(VIEW_W, VIEW_H);
        assert_eq!(
            rgba.len(),
            VIEW_W * VIEW_H * 4,
            "RGBA8 framebuffer is width*height*4 bytes"
        );

        // The frame is not a flat clear: the colored bars paint pixels that differ from
        // the canvas color. Count pixels that are clearly not the near-black background.
        let clear = [CLEAR.r, CLEAR.g, CLEAR.b];
        let lit = rgba
            .chunks_exact(4)
            .filter(|px| {
                let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
                d(px[0], clear[0]) + d(px[1], clear[1]) + d(px[2], clear[2]) > 60
            })
            .count();
        assert!(
            lit > 5_000,
            "expected the bars + text to light up many pixels, got {lit}"
        );
    }
}
