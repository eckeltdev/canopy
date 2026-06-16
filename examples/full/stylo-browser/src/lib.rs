//! Full-tier (**Stylo**) demo — a tiny HTML/CSS browser. Feed *arbitrary* HTML
//! (markup **and** embedded `<style>` rules together) straight into the real
//! Servo-**Stylo** engine ([`canopy_style_stylo`]) and render the result with the
//! lite CPU software rasterizer.
//!
//! Where the sibling `stylo` example hand-builds an arena tree and hot-reloads a
//! separate `styles.css`, this one is **HTML-driven**: [`StyloEngine::from_html`]
//! parses the page (harvesting its `<style>` blocks as the author stylesheet),
//! runs the whole-tree cascade, lays it out with Taffy, and paints it — so editing
//! `page.html` re-flows the entire document, structure and style alike.
//!
//! Two binaries drive this: `src/bin/render.rs` (a headless still → PPM) and
//! `src/bin/window.rs` (a live winit window that **hot-reloads `page.html`** — edit
//! the page, press a key, and the whole document is re-parsed, re-cascaded, and
//! redrawn).

use canopy_render_soft::Buffer;
use canopy_style_stylo::StyloEngine;
use canopy_traits::Size;

/// Logical window width — the page lays out within this and reflows on resize.
pub const VIEW_W: usize = 720;
/// Logical window height.
pub const VIEW_H: usize = 560;

/// The editable page, read at runtime so editing + saving hot-reloads the window.
/// Both the markup and the embedded CSS live here, so a single edit re-flows the
/// whole document.
pub const PAGE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/page.html");

/// A tiny fallback document, used if `page.html` cannot be read, so the app still
/// renders something rather than a blank window.
const FALLBACK_HTML: &str = "<style>body{background:#0c0d10;color:#c9d1d9;padding:16px}\
h1{color:#58a6ff;font-size:24px;padding:8px}</style>\
<body><h1>page.html not found</h1></body>";

/// Read `page.html` from disk (a small fallback document on failure, so the app
/// still runs).
#[must_use]
pub fn load_html() -> String {
    std::fs::read_to_string(PAGE_PATH).unwrap_or_else(|_| FALLBACK_HTML.to_string())
}

/// Build a [`StyloEngine`] from arbitrary `html`: parse the markup, harvest its
/// `<style>` blocks as the author CSS, and run the real Stylo cascade. Rebuilt
/// wholesale on each hot-reload (cheap; the page is tiny).
#[must_use]
pub fn build(html: &str) -> StyloEngine {
    StyloEngine::from_html(html)
}

/// Render `engine` into a fresh [`Buffer`] at a `w` × `h` viewport. The engine runs
/// layout (which resolves styles) and paints backgrounds + text into the buffer,
/// which it clears to white first.
#[must_use]
pub fn render_to_buffer(engine: &mut StyloEngine, w: usize, h: usize) -> Buffer {
    engine.render(Size {
        w: w.max(1) as f32,
        h: h.max(1) as f32,
    })
}
