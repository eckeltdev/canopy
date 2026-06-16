//! Headless **GPU** render of the full-tier (Stylo) browser page.
//!
//! Parse `page.html` (arbitrary HTML + embedded CSS) through the real Servo-**Stylo**
//! engine, lower the cascaded + laid-out tree to a backend-neutral
//! [`canopy_traits::DisplayList`] via [`canopy_full_stylo_browser::build_display_list`]
//! (i.e. `StyloEngine::build_display_list`), then rasterize that scene on the **GPU**
//! (Metal on this Mac) with [`canopy_render_vello::try_render_to_rgba`] and write a PPM.
//!
//! This is the GPU twin of the CPU `render` bin: same page, same DisplayList shape, but
//! the pixels come off wgpu instead of the software rasterizer. It proves the Stylo
//! display-list path feeds the GPU renderer exactly like the constrained
//! `canopy-layout-taffy` path does.
//!
//! Run: `cargo +nightly run --features gpu --bin gpu_render [out.ppm] [width] [height]`
//!
//! `try_render_to_rgba` returns `None` only when no GPU adapter exists; this Mac has a
//! Metal GPU, so it returns `Some`. On a GPU-less machine the bin prints a clear message
//! and exits non-zero rather than panicking. It also fails loudly if the rasterized
//! frame is blank (every pixel equals the clear color), so a regression that produced an
//! empty scene is caught.

use std::process::ExitCode;

use canopy_full_stylo_browser::{build, build_display_list, load_html, VIEW_H, VIEW_W};
use canopy_traits::Color;

/// The near-black canvas the frame is cleared to (so sub-pixel gaps stay on-palette).
/// Chosen to match the page's dark `body` background; the non-blank check counts pixels
/// that differ from this.
const CLEAR: Color = Color {
    r: 0x06,
    g: 0x06,
    b: 0x08,
    a: 255,
};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .unwrap_or_else(|| "stylo-browser-gpu.ppm".to_string());
    let w: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(VIEW_W);
    let h: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(VIEW_H);

    // Parse + cascade + lay out the page, then lower it to the backend-neutral scene.
    let mut engine = build(&load_html());
    let scene = build_display_list(&mut engine, w, h);

    let viewport = canopy_traits::Size {
        w: w as f32,
        h: h as f32,
    };

    // Rasterize on the GPU. `try_render_to_rgba` returns `None` only when no GPU
    // adapter could be acquired; surface that cleanly instead of panicking.
    let Some(rgba) = canopy_render_vello::try_render_to_rgba(&scene, viewport, CLEAR) else {
        eprintln!("no GPU adapter available — cannot rasterize the Stylo page on the GPU");
        return ExitCode::FAILURE;
    };

    // Proof the frame is non-blank: count pixels that differ from the clear color. A
    // correctly-lowered page paints its dark panel chrome + light text over the clear,
    // so this must be well above zero.
    let non_clear = count_non_clear(&rgba, CLEAR);
    let total = w * h;
    if non_clear == 0 {
        eprintln!("GPU frame is blank — every pixel equals the clear color (no scene rasterized)");
        return ExitCode::FAILURE;
    }

    std::fs::write(&path, to_ppm(&rgba, w as u32, h as u32)).expect("write ppm");
    println!(
        "wrote {path} ({w}x{h}) — Stylo page rasterized on the GPU; \
         {non_clear}/{total} pixels differ from the clear color"
    );
    ExitCode::SUCCESS
}

/// Count RGBA8 pixels whose RGB differs from `clear` (the alpha byte is ignored, since
/// the readback is opaque). A non-zero count proves the GPU drew the scene rather than
/// leaving a blank clear.
fn count_non_clear(rgba: &[u8], clear: Color) -> usize {
    rgba.chunks_exact(4)
        .filter(|px| px[0] != clear.r || px[1] != clear.g || px[2] != clear.b)
        .count()
}

/// Encode row-major RGBA8 pixels as a binary PPM (P6) — a tiny, viewable artifact
/// (the alpha byte is dropped). Mirrors the CPU `render` bin's encoder.
fn to_ppm(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(15 + (width * height * 3) as usize);
    out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&px[0..3]);
    }
    out
}
