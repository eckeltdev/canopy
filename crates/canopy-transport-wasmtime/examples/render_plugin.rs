//! Run the untrusted wasm plugin in the sandbox and rasterize the UI it built.
//!
//! This is the whole pitch in one file: an untrusted `.wasm` blob — granted exactly
//! one host import and nothing else (no filesystem, no network, capped memory/CPU) —
//! produces a UI, and the host turns the retained tree into pixels. The guest never
//! touched a renderer, a window, or the OS; it only emitted the op-stream it was
//! allowed to.
//!
//! Usage: `cargo run -p canopy-transport-wasmtime --example render_plugin [out.ppm]`

use canopy_layout_taffy::build_scene;
use canopy_render_soft::SoftwareRenderer;
use canopy_traits::{Color, Renderer, Size};
use canopy_transport_wasmtime::PluginHost;

fn main() {
    // The wasm path is baked in at compile time by build.rs.
    let wasm = std::fs::read(env!("CANOPY_PLUGIN_WASM")).expect("read guest wasm");

    let mut host = PluginHost::new().expect("create sandbox");
    host.run(&wasm).expect("run untrusted plugin");

    let viewport = Size { w: 480.0, h: 96.0 };
    let clear = Color {
        r: 0x1e,
        g: 0x1e,
        b: 0x2e,
        a: 255,
    };
    let mut renderer = SoftwareRenderer::new(viewport.w as usize, viewport.h as usize, clear);
    renderer
        .render(&build_scene(host.dom(), viewport))
        .expect("render");

    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "plugin.ppm".to_string());
    std::fs::write(&path, renderer.buffer().to_ppm()).expect("write ppm");
    println!(
        "untrusted plugin built {} nodes; wrote {path}",
        host.dom().node_count()
    );
}
