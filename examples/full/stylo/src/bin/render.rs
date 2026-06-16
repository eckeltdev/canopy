//! Headless full-tier (Stylo) render: build the styled tree, resolve it through the real
//! Stylo cascade, print the resolved styles, and rasterize one frame to a PPM. No window.
//!
//! Run: `cargo +nightly run --no-default-features --bin render [out.ppm] [width] [height]`.

use canopy_full_stylo::{build, load_styles, VIEW_H, VIEW_W};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "stylo.ppm".to_string());
    let w: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(VIEW_W);
    let h: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(VIEW_H);

    let mut scene = build(&load_styles());
    scene.print_cascade();

    let buf = scene.render(w, h);
    std::fs::write(&path, buf.to_ppm()).expect("write ppm");
    println!("\nwrote {path} ({w}x{h}) — styled by Stylo, rasterized on the CPU");
}
