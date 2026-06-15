//! Headless renderer for the Canopy demo: builds the app UI (Taffy + CSS + a typed
//! field), then hosts the untrusted wasm plugin in a bordered panel — all rasterized
//! by the software renderer to a PPM. No window, no GPU.
//!
//! Usage: `cargo run --no-default-features --bin render [out.ppm] [count]`

use canopy_demo::{build, run_plugin, VIEW_H, VIEW_W};
use canopy_dom::Dom;
use canopy_input::Key;
use canopy_layout_taffy::build_scene;
use canopy_plugin_panel::render_panel;
use canopy_render_soft::SoftwareRenderer;
use canopy_traits::{Color, OpSink, Point, Rect, Renderer, Size};

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
    let mut renderer = SoftwareRenderer::new(VIEW_W as usize, VIEW_H as usize, clear);
    renderer
        .render(&build_scene(&dom, viewport))
        .expect("render");

    // --- Host the untrusted wasm plugin in a panel on the right. ---
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
    let buf = renderer.buffer_mut();
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
            render_panel(buf, inner, host.dom());
            println!(
                "untrusted plugin built {} nodes into the panel",
                host.dom().node_count()
            );
        }
        None => buf.blit_text(Point { x: 402.0, y: 92.0 }, "(plugin failed)", label, 16.0),
    }

    std::fs::write(&path, renderer.buffer().to_ppm()).expect("write ppm");
    println!("wrote {path} ({}x{})", VIEW_W as u32, VIEW_H as u32);
}
