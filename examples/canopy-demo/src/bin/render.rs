//! Headless renderer for the Canopy demo: builds the app UI (Taffy + CSS + a typed
//! field), rasterizes it with **real antialiased glyphs**
//! ([`canopy_render_text::render_dom`]), then composites the untrusted wasm plugin
//! into a bordered panel on the right — all written out to a PPM. No window, no GPU.
//!
//! The base frame now comes from the capable-tier text renderer instead of the
//! baked-8x8 software path, so the headline UI text is sharp; the plugin panel and
//! its label still use the software [`canopy_render_soft::Buffer`] surface (the same
//! surface `render_dom` returns), so panel compositing is byte-for-byte unchanged.
//!
//! Usage: `cargo run --no-default-features --bin render [out.ppm] [count]`

use canopy_demo::{build, run_plugin, VIEW_H, VIEW_W};
use canopy_dom::Dom;
use canopy_input::Key;
use canopy_plugin_panel::render_panel;
use canopy_traits::{Color, OpSink, Point, Rect, Size};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "canopy_demo.ppm".to_string());
    let start: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let demo = build();
    if start != 0 {
        demo.count.set(start);
        demo.app.runtime().flush();
    }
    // Type into the focused field so the static shot shows real input.
    for c in "buy milk".chars() {
        demo.app.type_into(demo.input, Key::Char(c));
    }

    let mut dom = Dom::new();
    dom.apply(&demo.app.take_batch(0)).expect("apply ops");

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

    // Base frame: lay out via Taffy and rasterize the DisplayList with sharp,
    // antialiased cosmic-text glyphs (this clears to `clear` and returns the buffer).
    let mut buf = canopy_render_text::render_dom(&dom, viewport, clear);

    // --- Host the untrusted wasm plugin in a panel on the right. ---
    // Composited onto the same `Buffer` exactly as before; the panel's own text is
    // still the software baked font (the plugin DOM is painted by `render_panel`).
    let label = Color {
        r: 0xf3,
        g: 0x8b,
        b: 0xa8,
        a: 255,
    };
    let border = Color {
        r: 0x45,
        g: 0x47,
        b: 0x5a,
        a: 255,
    };
    let plugin_bg = Color {
        r: 0x18,
        g: 0x18,
        b: 0x25,
        a: 255,
    };
    buf.blit_text(
        Point { x: 392.0, y: 22.0 },
        "untrusted plugin:",
        label,
        16.0,
    );
    let frame = Rect {
        origin: Point { x: 392.0, y: 46.0 },
        size: Size { w: 300.0, h: 104.0 },
    };
    let inner = Rect {
        origin: Point { x: 394.0, y: 48.0 },
        size: Size { w: 296.0, h: 100.0 },
    };
    buf.fill_rect(frame, border);
    buf.fill_rect(inner, plugin_bg);
    match run_plugin() {
        Some(host) => {
            render_panel(&mut buf, inner, host.dom());
            println!(
                "untrusted plugin built {} nodes into the panel",
                host.dom().node_count()
            );
        }
        None => buf.blit_text(Point { x: 402.0, y: 92.0 }, "(plugin failed)", label, 16.0),
    }

    std::fs::write(&path, buf.to_ppm()).expect("write ppm");
    println!("wrote {path} ({}x{})", VIEW_W as u32, VIEW_H as u32);
}
