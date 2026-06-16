//! Headless renderer for the Canopy **welcome** app.
//!
//! Builds the welcome screen ([`canopy_lite_welcome::build`] — one `rsx!` tree of logo,
//! heading, tagline, counter card, footer pills), seeds the counter to a lively nonzero
//! value so the static shot looks alive, and rasterizes one frame with **real
//! antialiased glyphs** ([`canopy_render_text::render_dom`]). The frame is written out
//! as a PPM. No window, no GPU.
//!
//! Usage: `cargo run --no-default-features --bin render [out.ppm] [count]`
//!   - `out.ppm` — output path (default `welcome.ppm` in the manifest dir).
//!   - `count`   — the counter's starting value (default `3`, so the button reads
//!     "count is 3" in the screenshot).

use canopy_traits::{Color, OpSink, Size};
use canopy_ui::prelude::Dom;
use canopy_lite_welcome::{build, VIEW_H, VIEW_W};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "welcome.ppm".to_string());
    // Default to 3 so the flagship screenshot shows a non-trivial counter value.
    let start: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(3);

    let mut welcome = build();
    // Settle the logo's entrance animation to its full-size end state — a static shot
    // should show the grown canopy, not a mid-sprout frame.
    welcome.settle();
    // Seed the counter and flush so the bound label emits its "count is N" SetText
    // before we drain the mount batch.
    welcome.count.set(start);
    welcome.ui.runtime().flush();

    let mut dom = Dom::new();
    dom.apply(&welcome.ui.take_batch(0)).expect("apply ops");

    // The dark canvas clear color (Catppuccin base). The `.canvas` element also paints
    // this, but clearing to it keeps any sub-pixel gaps on-palette.
    let clear = Color {
        r: 0x1e,
        g: 0x1e,
        b: 0x2e,
        a: 255,
    };
    let viewport = Size {
        w: VIEW_W,
        h: VIEW_H,
    };

    // Lay out via Taffy and rasterize the DisplayList with sharp, antialiased glyphs
    // (this sizes + clears the buffer and returns it).
    let buf = canopy_render_text::render_dom(&dom, viewport, clear);

    std::fs::write(&path, buf.to_ppm()).expect("write ppm");
    println!(
        "wrote {path} ({}x{}), counter = {start}",
        VIEW_W as u32, VIEW_H as u32
    );
}
