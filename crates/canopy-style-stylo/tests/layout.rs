//! Layout regression tests: run each shared layout fixture through the real Stylo→Taffy
//! layout pass and assert key box geometry. No browser; runs in normal CI. The
//! `layout_oracle.rs` test checks the same fixtures against a real browser.

mod common;

use common::{layout_fixtures, resolve_layout_stylo, LayoutBox};

/// `b` is within 1px of `(x, y, w, h)`.
fn approx(b: LayoutBox, x: f32, y: f32, w: f32, h: f32) -> bool {
    (b.x - x).abs() < 1.0 && (b.y - y).abs() < 1.0 && (b.w - w).abs() < 1.0 && (b.h - h).abs() < 1.0
}

#[test]
fn layout_regression() {
    for (name, css, tree, viewport) in layout_fixtures() {
        let b = resolve_layout_stylo(css, &tree, viewport);
        match name {
            // flex row, width 200, two `flex:1` children -> 100px each, side by side.
            "flex_row_grow" => {
                assert!(
                    approx(b[1], 0.0, 0.0, 100.0, 100.0),
                    "{name} c1 = {:?}",
                    b[1]
                );
                assert!(
                    approx(b[2], 100.0, 0.0, 100.0, 100.0),
                    "{name} c2 = {:?}",
                    b[2]
                );
            }
            // 20px padding offsets the child to (20,20); its own size is 100x40.
            "block_padding" => {
                assert!(
                    approx(b[1], 20.0, 20.0, 100.0, 40.0),
                    "{name} child = {:?}",
                    b[1]
                )
            }
            // justify-content:center on a 200-wide row centers the 40-wide child at x=80.
            "justify_center" => {
                assert!(
                    approx(b[1], 80.0, 0.0, 40.0, 50.0),
                    "{name} child = {:?}",
                    b[1]
                )
            }
            // margin-left:30 pushes the child's left edge to x=30.
            "margin_left" => {
                assert!(
                    approx(b[1], 30.0, 0.0, 50.0, 30.0),
                    "{name} child = {:?}",
                    b[1]
                )
            }
            other => panic!("unhandled layout fixture {other}"),
        }
    }
}
