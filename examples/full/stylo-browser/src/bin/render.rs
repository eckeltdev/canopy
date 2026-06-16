//! Headless full-tier (Stylo) browser render: parse `page.html` (arbitrary HTML +
//! embedded CSS) through the real Stylo engine, lay it out, and rasterize one frame
//! to a PPM. No window.
//!
//! Run: `cargo +nightly run --no-default-features --bin render [out.ppm] [width] [height]`.

use canopy_full_stylo_browser::{build, load_html, render_to_buffer, VIEW_H, VIEW_W};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .unwrap_or_else(|| "stylo-browser.ppm".to_string());
    let w: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(VIEW_W);
    let h: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(VIEW_H);

    let mut engine = build(&load_html());
    let buf = render_to_buffer(&mut engine, w, h);
    std::fs::write(&path, buf.to_ppm()).expect("write ppm");
    println!("wrote {path} ({w}x{h}) — arbitrary HTML/CSS styled by Stylo, rasterized on the CPU");
}
