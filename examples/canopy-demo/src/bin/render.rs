//! Headless renderer for the Canopy demo: builds the UI, lays it out with Taffy,
//! paints one frame with the software rasterizer, and writes it as a binary PPM.
//! No window, no GPU, no UI dependencies — so it runs anywhere and is how the demo
//! is screenshotted.
//!
//! Usage: `cargo run --no-default-features --bin render [out.ppm] [count]`

use canopy_demo::{build, VIEW_H, VIEW_W};
use canopy_dom::Dom;
use canopy_layout_taffy::build_scene;
use canopy_render_soft::SoftwareRenderer;
use canopy_traits::{Color, OpSink, Renderer, Size};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "canopy_demo.ppm".to_string());
    let start: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let demo = build();
    if start != 0 {
        demo.count.set(start);
        demo.app.runtime().flush();
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

    std::fs::write(&path, renderer.buffer().to_ppm()).expect("write ppm");
    println!("wrote {path} ({}x{}, Taffy layout)", VIEW_W as u32, VIEW_H as u32);
}
