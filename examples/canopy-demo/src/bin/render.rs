//! Headless renderer for the Canopy demo: builds the UI, paints one frame with the
//! software rasterizer, and writes it as a binary PPM. No window, no GPU, no UI
//! dependencies — so it runs anywhere and is how the demo is screenshotted.
//!
//! Usage: `cargo run --no-default-features --bin render [out.ppm] [count]`

use canopy_demo::{build, VIEW_H, VIEW_W};
use canopy_host::Host;
use canopy_render_soft::SoftwareRenderer;
use canopy_traits::{Color, Size};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "canopy_demo.ppm".to_string());
    let start: i32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let demo = build();
    // Optionally start the counter somewhere lively, exercising the reactive path.
    if start != 0 {
        demo.count.set(start);
        demo.app.runtime().flush();
    }

    let clear = Color {
        r: 0x1e,
        g: 0x1e,
        b: 0x2e,
        a: 255,
    };
    let mut host = Host::new(SoftwareRenderer::new(VIEW_W as usize, VIEW_H as usize, clear));
    host.apply(&demo.app.take_batch(0)).expect("apply ops");
    host.paint(Size {
        w: VIEW_W,
        h: VIEW_H,
    })
    .expect("paint");

    std::fs::write(&path, host.renderer().buffer().to_ppm()).expect("write ppm");
    println!("wrote {path} ({}x{})", VIEW_W as u32, VIEW_H as u32);
}
