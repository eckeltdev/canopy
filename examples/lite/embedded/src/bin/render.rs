//! Host harness for the `#![no_std]` [`canopy_lite_embedded`] pipeline.
//!
//! All this binary does with `std` is **write a file**: it calls the no_std
//! [`render_rgba`](canopy_lite_embedded::render_rgba) — which builds, lays out, and
//! software-rasterizes the scene exactly as a microcontroller would — and encodes the
//! returned RGBA8 framebuffer as a PPM so you can view it on a desktop.
//!
//! Usage: `cargo run --bin render [out.ppm] [width] [height]`
//!
//! To prove the *library* is genuinely bare-metal, build it for an embedded target
//! (this binary is host-only and is not built there):
//!   `cargo +nightly build --lib --target thumbv7em-none-eabi`

use canopy_lite_embedded::{render_rgba, VIEW_H, VIEW_W};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "frame.ppm".to_string());
    let w: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(VIEW_W);
    let h: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(VIEW_H);

    // The no_std pipeline: build -> layout -> software-rasterize -> raw RGBA framebuffer.
    let rgba = render_rgba(w, h);

    std::fs::write(&path, to_ppm(&rgba, w, h)).expect("write ppm");
    println!("wrote {path} ({w}x{h}) — built + rasterized by the no_std pipeline");
}

/// Encode row-major RGBA8 pixels as a binary PPM (P6); the alpha byte is dropped.
fn to_ppm(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + width * height * 3);
    out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&px[0..3]);
    }
    out
}
