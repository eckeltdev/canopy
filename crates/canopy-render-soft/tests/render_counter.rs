//! Full vertical slice, headless: a signal-driven counter is authored with
//! `canopy-view`, applied to a host `Dom`, laid out and painted by `canopy-paint`,
//! and rasterized to a real pixel buffer — verified by asserting pixel colors and
//! by writing a viewable PPM. No GPU, no window.

use canopy_dom::{Dom, ROOT};
use canopy_paint::{build_scene, BG, FG, HEIGHT};
use canopy_protocol::{ElementTag, EventKind, EventPayload};
use canopy_render_soft::SoftwareRenderer;
use canopy_traits::{Color, OpSink, Renderer, Size};
use canopy_view::App;

const COLUMN: ElementTag = ElementTag::new(1);
const CLICK: EventKind = EventKind::new(1);

const BLACK: Color = Color {
    r: 0,
    g: 0,
    b: 0,
    a: 255,
};
const COL_BG: [u8; 4] = [0x20, 0x28, 0x30, 255];
const LABEL_FG: [u8; 4] = [0xff, 0xd0, 0x40, 255];

fn render(app: &App, dom: &Dom) -> SoftwareRenderer {
    let mut r = SoftwareRenderer::new(200, 100, BLACK);
    r.render(&build_scene(dom, Size { w: 200.0, h: 100.0 }))
        .unwrap();
    let _ = app; // keep the app alive alongside the dom it produced
    r
}

/// Scan a rectangular pixel region for any pixel matching `want`.
fn region_has(
    r: &SoftwareRenderer,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    want: [u8; 4],
) -> bool {
    (y0..y1).any(|y| (x0..x1).any(|x| r.buffer().pixel(x, y) == want))
}

#[test]
fn signal_counter_renders_and_reacts_to_pixels() {
    let app = App::new();

    // Author: a column with a styled, bound label.
    let col = app.element(COLUMN);
    app.append(ROOT, col);
    app.style(col, BG, "#202830");
    let label = app.text("");
    app.append(col, label);
    app.style(label, FG, "#ffd040");
    app.style(label, HEIGHT, "20");

    let count = app.runtime().signal(0i32);
    {
        let count = count.clone();
        app.bind_text(label, move || format!("Count: {}", count.get()));
    }
    let on_click = {
        let count = count.clone();
        app.on(col, CLICK, move |_| count.update(|n| *n += 1))
    };

    // Mount + paint.
    let mut dom = Dom::new();
    dom.apply(&app.take_batch(0)).unwrap();
    let r = render(&app, &dom);

    // "Count: 0" is 8 chars, 20px tall, drawn with the baked font at the origin.
    // The glyphs are scale-2 ink on the column background, so individual pixels
    // are either fg ink or column bg — scan the label box for at least one ink
    // pixel rather than asserting a solid fill.
    assert!(
        region_has(&r, 0, 0, 80, 20, LABEL_FG),
        "label box should contain foreground ink"
    );
    assert_eq!(
        r.buffer().pixel(180, 10),
        COL_BG,
        "past the label but inside the column background"
    );
    assert_eq!(
        r.buffer().pixel(100, 60),
        [0, 0, 0, 255],
        "below the column = cleared"
    );

    // Write a viewable artifact (colored boxes).
    let path = std::env::temp_dir().join("canopy_counter.ppm");
    std::fs::write(&path, r.buffer().to_ppm()).unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() > 0);

    // React: a click writes the signal; the flush emits one SetText; re-paint.
    app.dispatch(on_click, EventPayload::None);
    dom.apply(&app.take_batch(1)).unwrap();
    assert_eq!(dom.text_of(label), Some("Count: 1"));

    let r2 = render(&app, &dom);
    assert!(
        region_has(&r2, 0, 0, 80, 20, LABEL_FG),
        "label still painted after update"
    );
}
