//! Headless renderer for the Canopy **landing**: builds the page, settles the entrance
//! animation, and rasterizes one frame with real antialiased glyphs to a PPM. No window.
//!
//! Usage: `cargo run --no-default-features --bin render [out.ppm]`

use canopy_traits::{Color, OpSink, Size};
use canopy_ui::prelude::Dom;
use canopy_landing::{build, VIEW_H, VIEW_W};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "landing.ppm".to_string());

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
    let viewport = Size {
        w: VIEW_W,
        h: VIEW_H,
    };
    let buf = canopy_render_text::render_dom(&dom, viewport, clear);

    std::fs::write(&path, buf.to_ppm()).expect("write ppm");
    println!("wrote {path} ({}x{})", VIEW_W as u32, VIEW_H as u32);
}
