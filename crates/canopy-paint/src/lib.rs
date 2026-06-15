//! Canopy scene builder: turns the host [`Dom`] into a renderer-agnostic
//! [`DisplayList`] that any [`canopy_traits::Renderer`] can paint.
//!
//! This is the host-side "style + layout + paint-tree-walk" stage. The M1
//! implementation here is deliberately trivial — a vertical **stack** layout that
//! reads a handful of inline style properties — so the whole pipeline (op-stream →
//! `Dom` → `DisplayList` → pixels) is exercised and testable *before* the real
//! engines are wired. `Stylo` (style) and `Taffy` (layout) drop in behind the
//! `StyleEngine` / `LayoutEngine` traits without changing this crate's output type.
//!
//! Text nodes are emitted as [`canopy_traits::DisplayItem::Text`] runs, which the
//! software renderer paints with a baked 8x8 bitmap font; the optional text
//! background still emits a [`canopy_traits::DisplayItem::Rect`] behind the run.
//! Shaped runs ([`canopy_traits::DisplayItem::Glyphs`]) arrive with the capable-tier
//! Parley text backend.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::ToString;
use alloc::vec::Vec;

use canopy_dom::{Dom, ROOT};
use canopy_protocol::{NodeId, PropId};
use canopy_traits::{Color, DisplayItem, DisplayList, Point, Rect, Size};

/// Background color, e.g. `#202830`.
pub const BG: PropId = PropId::new(1);
/// Foreground / text color, e.g. `#ffd040`.
pub const FG: PropId = PropId::new(2);
/// Explicit width in integer pixels.
pub const WIDTH: PropId = PropId::new(3);
/// Explicit height in integer pixels.
pub const HEIGHT: PropId = PropId::new(4);
/// Gap between stacked children, in integer pixels.
pub const GAP: PropId = PropId::new(5);

const TEXT_ADVANCE: f32 = 8.0;
const TEXT_HEIGHT: f32 = 16.0;
const DEFAULT_FG: Color = Color {
    r: 0xe6,
    g: 0xe6,
    b: 0xe6,
    a: 255,
};

fn parse_color(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    Some(Color {
        r: u8::from_str_radix(&hex[0..2], 16).ok()?,
        g: u8::from_str_radix(&hex[2..4], 16).ok()?,
        b: u8::from_str_radix(&hex[4..6], 16).ok()?,
        a: 255,
    })
}

// Sizes are integer-pixel strings; integer parsing is `core`-safe (float parsing
// historically was not), which keeps this crate honestly `no_std`.
fn parse_dim(s: &str) -> Option<f32> {
    s.parse::<u32>().ok().map(|v| v as f32)
}

fn style_color(dom: &Dom, node: NodeId, prop: PropId) -> Option<Color> {
    dom.style(node, prop).and_then(parse_color)
}

fn style_dim(dom: &Dom, node: NodeId, prop: PropId) -> Option<f32> {
    dom.style(node, prop).and_then(parse_dim)
}

fn rect_item(origin: Point, w: f32, h: f32, color: Color) -> DisplayItem {
    DisplayItem::Rect {
        rect: Rect {
            origin,
            size: Size { w, h },
        },
        color,
    }
}

/// Build a display list for the whole tree within `viewport`.
pub fn build_scene(dom: &Dom, viewport: Size) -> DisplayList {
    let mut items = Vec::new();
    let mut y = 0.0;
    for &root in dom.children(ROOT) {
        let size = layout_node(dom, root, Point { x: 0.0, y }, viewport.w, &mut items);
        y += size.h;
    }
    DisplayList { items }
}

fn layout_node(
    dom: &Dom,
    id: NodeId,
    origin: Point,
    avail_w: f32,
    out: &mut Vec<DisplayItem>,
) -> Size {
    let node = match dom.node(id) {
        Some(n) => n,
        None => return Size::default(),
    };

    // Text leaf: a baked-font run, with an optional background rect behind it.
    if let Some(text) = node.text.as_deref() {
        let h = style_dim(dom, id, HEIGHT).unwrap_or(TEXT_HEIGHT);
        // Advance scales with the requested cell height; 8px per char at 16px tall.
        let advance = TEXT_ADVANCE * (h / TEXT_HEIGHT);
        let w = style_dim(dom, id, WIDTH).unwrap_or(text.chars().count() as f32 * advance);
        if let Some(bg) = style_color(dom, id, BG) {
            out.push(rect_item(origin, w, h, bg));
        }
        let fg = style_color(dom, id, FG).unwrap_or(DEFAULT_FG);
        out.push(DisplayItem::Text {
            origin,
            text: text.to_string(),
            color: fg,
            size: h,
        });
        return Size { w, h };
    }

    // Element: stack children vertically. Reserve the element's background slot so
    // it paints behind its children.
    let gap = style_dim(dom, id, GAP).unwrap_or(0.0);
    let width = style_dim(dom, id, WIDTH).unwrap_or(avail_w);
    let bg_index = out.len();

    let mut cy = origin.y;
    let mut max_w: f32 = 0.0;
    let mut first = true;
    for &child in dom.children(id) {
        if !first {
            cy += gap;
        }
        first = false;
        let size = layout_node(dom, child, Point { x: origin.x, y: cy }, width, out);
        cy += size.h;
        if size.w > max_w {
            max_w = size.w;
        }
    }

    let height = style_dim(dom, id, HEIGHT).unwrap_or(cy - origin.y);
    let w = width.max(max_w);
    if let Some(bg) = style_color(dom, id, BG) {
        out.insert(bg_index, rect_item(origin, w, height, bg));
    }
    Size { w, h: height }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_core::Emitter;
    use canopy_dom::Dom;
    use canopy_protocol::ElementTag;
    use canopy_traits::OpSink;

    #[test]
    fn stacks_children_and_colors_from_styles() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, BG, "#202830");
        let a = e.create_text("ab"); // 2 chars -> 16px wide
        e.append(col, a);
        e.set_inline_style(a, HEIGHT, "20");
        e.set_inline_style(a, FG, "#ffd040");

        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();

        let scene = build_scene(&dom, Size { w: 100.0, h: 50.0 });
        // First item is the column background (inserted behind), spanning the
        // viewport width and the child's height.
        match &scene.items[0] {
            DisplayItem::Rect { rect, color } => {
                assert_eq!(
                    *color,
                    Color {
                        r: 0x20,
                        g: 0x28,
                        b: 0x30,
                        a: 255
                    }
                );
                assert_eq!(rect.size.w, 100.0);
                assert_eq!(rect.size.h, 20.0);
            }
            _ => panic!("expected the column background rect first"),
        }
        // The text leaf emits a Text run carrying the content, the foreground color,
        // and the requested cell height — not a placeholder foreground rect.
        let fg = Color {
            r: 0xff,
            g: 0xd0,
            b: 0x40,
            a: 255,
        };
        let text_item = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Text {
                    origin,
                    text,
                    color,
                    size,
                } => Some((*origin, text.clone(), *color, *size)),
                _ => None,
            })
            .expect("text run");
        assert_eq!(text_item.1, "ab");
        assert_eq!(text_item.2, fg);
        assert_eq!(text_item.3, 20.0);
        // No foreground-colored Rect is emitted for the text anymore.
        assert!(
            !scene.items.iter().any(|i| matches!(
                i,
                DisplayItem::Rect { color, .. } if *color == fg
            )),
            "text must not emit a foreground placeholder rect"
        );
    }
}
