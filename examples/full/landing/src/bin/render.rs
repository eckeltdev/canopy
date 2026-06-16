//! Headless **GPU** renderer for the Canopy landing: builds the shared page
//! ([`canopy_landing_ui`]), settles the entrance animation, lays it out into a display
//! list, and rasterizes one frame on the GPU (Metal on this Mac) to a PPM. No window —
//! this proves the *GPU* path renders the exact same landing the CPU `canopy-lite-landing`
//! example does.
//!
//! Usage: `cargo run --no-default-features --bin render [out.ppm] [width] [height]`
//! The `width`/`height` show the page is responsive: the `.stage` is `100% x 100%`,
//! so it fills whatever viewport it is laid out for.
//!
//! Unlike the CPU bin (which calls `canopy_render_text::render_dom`), this builds the
//! scene with [`canopy_layout_taffy::build_scene`] and rasterizes it with
//! [`canopy_render_vello::try_render_to_rgba`] — the `wgpu`-backed GPU `Renderer`. If
//! no GPU adapter is available it prints a clear message and exits non-zero rather than
//! panicking.

use std::process::ExitCode;

use canopy_landing_ui::{build, VIEW_H, VIEW_W};
use canopy_layout_taffy::build_scene;
use canopy_traits::{Color, OpSink, Size};
use canopy_ui::prelude::Dom;

/// The near-black canvas color; the `.stage` paints it too, but clearing to it keeps
/// any sub-pixel gaps on-palette.
const CLEAR: Color = Color {
    r: 0x06,
    g: 0x06,
    b: 0x08,
    a: 255,
};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "landing.ppm".to_string());
    let w: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(VIEW_W);
    let h: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(VIEW_H);

    let mut landing = build();
    landing.settle();

    let mut dom = Dom::new();
    dom.apply(&landing.ui.take_batch(0)).expect("apply ops");

    let viewport = Size { w, h };
    // Lay the settled tree out into a display list (the same scene the CPU path
    // builds internally), then hand it to the GPU rasterizer.
    let scene = build_scene(&dom, viewport);

    // `try_render_to_rgba` returns `None` only when no GPU adapter exists; surface
    // that cleanly instead of panicking so CI without a GPU gets a readable message.
    let Some(rgba) = canopy_render_vello::try_render_to_rgba(&scene, viewport, CLEAR) else {
        eprintln!("no GPU adapter available — cannot rasterize the landing on the GPU");
        return ExitCode::FAILURE;
    };

    std::fs::write(&path, to_ppm(&rgba, w as u32, h as u32)).expect("write ppm");
    println!(
        "wrote {path} ({}x{}) — rasterized on the GPU",
        w as u32, h as u32
    );
    ExitCode::SUCCESS
}

/// Encode row-major RGBA8 pixels as a binary PPM (P6) — a tiny, viewable artifact,
/// matching `canopy_render_soft::Buffer::to_ppm` (the alpha byte is dropped).
fn to_ppm(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(15 + (width * height * 3) as usize);
    out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&px[0..3]);
    }
    out
}
