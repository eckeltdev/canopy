//! Cascade regression tests: run each shared fixture through the real Stylo cascade and
//! assert the resolved [`ComputedStyle`] against known-good constants. No browser, no
//! renderer — runs in normal CI. The browser-oracle test (`browser_oracle.rs`) checks the
//! same fixtures against a real browser.

mod common;

use canopy_traits::{Color, Display};
use common::{fixtures, resolve_stylo};

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 255 }
}

#[test]
fn cascade_regression() {
    for (name, css, tree) in fixtures() {
        let s = resolve_stylo(css, &tree);
        match name {
            // .page{color:red} ancestor; the mid div and the leaf both inherit red.
            "inheritance" => {
                assert_eq!(s[1].color, rgb(255, 0, 0), "{name}: mid inherits red");
                assert_eq!(s[2].color, rgb(255, 0, 0), "{name}: leaf inherits red");
            }
            // <div class=x id=y>: #y beats .x beats div -> blue.
            "specificity_id_class_type" => {
                assert_eq!(s[0].color, rgb(0, 0, 255), "{name}: id wins -> blue");
            }
            // .a.b (0,2,0) beats .a (0,1,0) -> green.
            "specificity_two_classes" => {
                assert_eq!(s[0].color, rgb(0, 255, 0), "{name}: .a.b wins -> green");
            }
            // .card .title gets the bg (node 3, nested); a .title outside stays transparent.
            "descendant_combinator" => {
                assert_eq!(
                    s[3].background,
                    rgb(0x11, 0x22, 0x33),
                    "{name}: .title under .card has bg"
                );
                assert_eq!(
                    s[4].background.a, 0,
                    "{name}: .title outside .card transparent"
                );
            }
            // font-size/padding/display read straight off the box.
            "value_extraction" => {
                assert!((s[0].font_size - 24.0).abs() < 0.5, "{name}: font-size 24");
                assert!((s[0].padding - 8.0).abs() < 0.5, "{name}: padding 8");
                assert_eq!(s[0].display, Display::Flex, "{name}: display flex");
            }
            // font-size inherits onto the child; the child's own color applies.
            "font_size_inherits" => {
                assert!(
                    (s[1].font_size - 20.0).abs() < 0.5,
                    "{name}: child inherits 20px"
                );
                assert_eq!(s[1].color, rgb(0x33, 0x66, 0x99), "{name}: child color");
            }
            other => panic!("unhandled fixture {other}"),
        }
    }
}
