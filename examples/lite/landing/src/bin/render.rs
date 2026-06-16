//! Headless **CPU** renderer for the Canopy landing: builds the shared page
//! ([`canopy_landing_ui`]), settles the entrance animation, and software-rasterizes one
//! frame with real antialiased glyphs to a PPM. No window, no GPU — the lite path.
//!
//! Usage: `cargo run --no-default-features --bin render [out.ppm] [width] [height]`
//! The `width`/`height` show the page is responsive: the `.stage` is `100% x 100%`,
//! so it fills whatever viewport it is laid out for.

use canopy_landing_ui::{build, VIEW_H, VIEW_W};
use canopy_traits::{Color, OpSink, Size};
use canopy_ui::prelude::Dom;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "landing.ppm".to_string());
    let w: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(VIEW_W);
    let h: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(VIEW_H);

    let mut landing = build();
    landing.settle();

    let mut dom = Dom::new();
    dom.apply(&landing.ui.take_batch(0)).expect("apply ops");

    // The near-black canvas color; the `.stage` paints it too, but clearing to it keeps
    // any sub-pixel gaps on-palette.
    let clear = Color {
        r: 0x06,
        g: 0x06,
        b: 0x08,
        a: 255,
    };
    let viewport = Size { w, h };
    let buf = canopy_render_text::render_dom(&dom, viewport, clear);

    std::fs::write(&path, buf.to_ppm()).expect("write ppm");
    println!(
        "wrote {path} ({}x{}) — rasterized on the CPU",
        w as u32, h as u32
    );
}
