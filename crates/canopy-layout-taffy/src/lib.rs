//! Canopy layout via the **real** [Taffy] flexbox engine.
//!
//! This crate is a drop-in alternative to [`canopy_paint::layout`]: same
//! signature, same output types ([`DisplayList`] + [`LayoutResult`]). Where
//! `canopy-paint` hand-rolls a single-pass flex, this crate maps each element's
//! inline styles onto a Taffy [`Style`], builds a [`TaffyTree`] mirroring the
//! [`Dom`] subtree under every [`ROOT`] child, runs `compute_layout` against the
//! viewport, and converts Taffy's per-node *relative* boxes into the **absolute**
//! [`Rect`]s Canopy expects.
//!
//! The display list is built by Canopy, not Taffy: backgrounds paint behind their
//! children (one [`DisplayItem::Rect`] per element with a [`canopy_paint::BG`]
//! color) and text leaves paint as [`DisplayItem::Text`] runs sized to the baked
//! 8px font. Geometry — and only geometry — comes from Taffy.
//!
//! Style mapping (all inline, reusing the `canopy-paint` [`PropId`] consts):
//! - [`DIRECTION`] `"row"`/`"column"` -> [`FlexDirection::Row`]/`Column` (default `Column`).
//! - [`GAP`] -> Taffy `gap` on both axes (length px).
//! - [`PADDING`] -> uniform Taffy `padding` (length px) on all four sides.
//! - [`WIDTH`]/[`HEIGHT`] -> `size` of [`Dimension::length`] when set, else `auto`.
//!
//! Text leaves get a **fixed** Taffy size from the baked-font metrics, identical to
//! `canopy-paint`: `scale = max(1, height_px / 8)` (integer; `height_px` from
//! [`HEIGHT`] or `16`), `width = chars * 8 * scale`, `height = 8 * scale`.
//!
//! [Taffy]: https://docs.rs/taffy
//! Stays `no_std` + `alloc`: Taffy is pulled with `default-features = false` and
//! only `flexbox`/`alloc`/`taffy_tree`, and every pixel value is parsed as an
//! integer (no `f32::floor`/`parse` from `std`).
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::ToString;
use alloc::vec::Vec;

use canopy_dom::{Dom, ROOT};
use canopy_paint::{BG, DIRECTION, FG, GAP, HEIGHT, PADDING, RADIUS, WIDTH};
use canopy_protocol::{NodeId, PropId};
use canopy_traits::{Color, DisplayItem, DisplayList, LayoutResult, Point, Rect, Size};

use taffy::prelude::length;
use taffy::{
    AvailableSpace, Dimension, FlexDirection, LengthPercentage, Rect as TaffyRect,
    Size as TaffySize, Style, TaffyTree,
};

/// Baked-font cell advance at scale 1, in pixels.
const TEXT_ADVANCE: u32 = 8;
/// Baked-font cell height the renderer rasterizes at scale 1.
const TEXT_CELL: u32 = 8;
/// Default text cell height when no [`HEIGHT`] is set.
const TEXT_HEIGHT: u32 = 16;
/// Default foreground ink (light gray) when no [`FG`] is set.
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

/// Raw integer-pixel value of `prop` on `node`, if set and parseable. Pixel styles
/// are whole-number strings, so integer parsing keeps this crate honestly `no_std`
/// (float parsing was historically `std`-only).
fn style_px(dom: &Dom, node: NodeId, prop: PropId) -> Option<u32> {
    dom.style(node, prop).and_then(|s| s.parse::<u32>().ok())
}

fn style_color(dom: &Dom, node: NodeId, prop: PropId) -> Option<Color> {
    dom.style(node, prop).and_then(parse_color)
}

/// The node's corner [`RADIUS`] in logical px (default `0.0` = square). Geometry
/// comes from Taffy, but the corner radius is a *paint* property, so it is read
/// straight off the Dom here and threaded onto the emitted background rect.
fn style_radius(dom: &Dom, node: NodeId) -> f32 {
    style_px(dom, node, RADIUS).unwrap_or(0) as f32
}

/// Baked-font fixed pixel size for a text leaf: `scale = max(1, height_px / 8)`,
/// `width = chars * 8 * scale`, `height = 8 * scale`. Integer math throughout —
/// the same metrics `canopy-paint` uses, so the two engines agree on text size.
fn text_size(dom: &Dom, id: NodeId, text: &str) -> Size {
    let requested_px = style_px(dom, id, HEIGHT).unwrap_or(TEXT_HEIGHT);
    let scale = (requested_px / TEXT_CELL).max(1);
    let h = TEXT_CELL * scale;
    let advance = TEXT_ADVANCE * scale;
    let w = style_px(dom, id, WIDTH).unwrap_or(text.chars().count() as u32 * advance);
    Size {
        w: w as f32,
        h: h as f32,
    }
}

/// Whether `rect` contains `point` (top/left inclusive, bottom/right exclusive).
fn rect_contains(rect: &Rect, point: Point) -> bool {
    point.x >= rect.origin.x
        && point.y >= rect.origin.y
        && point.x < rect.origin.x + rect.size.w
        && point.y < rect.origin.y + rect.size.h
}

/// Build the Taffy [`Style`] for one element from its inline styles.
fn element_style(dom: &Dom, id: NodeId) -> Style {
    let dir = match dom.style(id, DIRECTION) {
        Some("row") => FlexDirection::Row,
        _ => FlexDirection::Column,
    };
    let gap = style_px(dom, id, GAP).unwrap_or(0) as f32;
    let pad = style_px(dom, id, PADDING).unwrap_or(0) as f32;
    let width = style_px(dom, id, WIDTH)
        .map(|w| Dimension::length(w as f32))
        .unwrap_or(Dimension::auto());
    let height = style_px(dom, id, HEIGHT)
        .map(|h| Dimension::length(h as f32))
        .unwrap_or(Dimension::auto());
    Style {
        flex_direction: dir,
        gap: TaffySize {
            width: length(gap),
            height: length(gap),
        },
        padding: TaffyRect {
            left: LengthPercentage::length(pad),
            right: LengthPercentage::length(pad),
            top: LengthPercentage::length(pad),
            bottom: LengthPercentage::length(pad),
        },
        size: TaffySize { width, height },
        ..Default::default()
    }
}

/// Recursively build a Taffy node mirroring `id`, returning its Taffy key. Text
/// leaves become fixed-size leaves; elements recurse over their children.
fn build_node(dom: &Dom, id: NodeId, tree: &mut TaffyTree<NodeId>) -> taffy::NodeId {
    if let Some(text) = dom.node(id).and_then(|n| n.text.as_deref()) {
        let size = text_size(dom, id, text);
        let style = Style {
            size: TaffySize {
                width: Dimension::length(size.w),
                height: Dimension::length(size.h),
            },
            ..Default::default()
        };
        // `new_leaf_with_context` stashes the Canopy NodeId so the post-layout walk
        // can map Taffy keys back to Dom nodes without a side table.
        return tree.new_leaf_with_context(style, id).unwrap();
    }

    let children: Vec<taffy::NodeId> = dom
        .children(id)
        .iter()
        .map(|&c| build_node(dom, c, tree))
        .collect();
    let key = tree
        .new_leaf_with_context(element_style(dom, id), id)
        .unwrap();
    if !children.is_empty() {
        tree.set_children(key, &children).unwrap();
    }
    key
}

/// Walk the computed Taffy tree, accumulating parent offsets into absolute rects
/// and recording `(NodeId, Rect)` in back-to-front tree order (parents before
/// children).
fn collect_rects(
    tree: &TaffyTree<NodeId>,
    key: taffy::NodeId,
    parent_origin: Point,
    rects: &mut Vec<(NodeId, Rect)>,
) {
    let layout = tree.layout(key).unwrap();
    let origin = Point {
        x: parent_origin.x + layout.location.x,
        y: parent_origin.y + layout.location.y,
    };
    let rect = Rect {
        origin,
        size: Size {
            w: layout.size.width,
            h: layout.size.height,
        },
    };
    if let Some(&id) = tree.get_node_context(key) {
        rects.push((id, rect));
    }
    for child in tree.children(key).unwrap() {
        collect_rects(tree, child, origin, rects);
    }
}

/// Lay the whole tree out within `viewport` using Taffy, producing both the
/// back-to-front [`DisplayList`] and a [`LayoutResult`] with an **absolute**
/// [`Rect`] for every node (elements *and* text), in tree order.
///
/// Same signature and output contract as [`canopy_paint::layout`]: each top-level
/// node is laid out against the viewport as available space and stacked down the
/// y axis. Geometry comes from Taffy; the display list (backgrounds behind
/// children, baked-font text runs) is built here from the Dom + the absolute rects.
pub fn layout(dom: &Dom, viewport: Size) -> (DisplayList, LayoutResult) {
    let mut rects: Vec<(NodeId, Rect)> = Vec::new();
    let mut y = 0.0_f32;
    for &root in dom.children(ROOT) {
        let mut tree: TaffyTree<NodeId> = TaffyTree::new();
        let key = build_node(dom, root, &mut tree);
        tree.compute_layout(
            key,
            TaffySize {
                width: AvailableSpace::Definite(viewport.w),
                height: AvailableSpace::Definite(viewport.h),
            },
        )
        .unwrap();
        collect_rects(&tree, key, Point { x: 0.0, y }, &mut rects);
        // Stack top-level siblings down the viewport, mirroring `canopy-paint`.
        let used_h = tree.layout(key).unwrap().size.height;
        y += used_h;
    }

    let items = build_display_list(dom, &rects);
    (DisplayList { items }, LayoutResult { rects })
}

/// Build a display list for the whole tree within `viewport`.
///
/// A thin wrapper over [`layout`] that discards the [`LayoutResult`]; kept at this
/// exact signature so renderer hosts continue to compile unchanged.
pub fn build_scene(dom: &Dom, viewport: Size) -> DisplayList {
    layout(dom, viewport).0
}

/// Build the [`DisplayList`] from the Dom and the absolute rects.
///
/// `rects` is in back-to-front tree order (parents before children), so iterating
/// it forward naturally paints each element's background *behind* its descendants.
/// Each element with a [`BG`] color emits a filled [`DisplayItem::Rect`]; each text
/// node emits a [`DisplayItem::Text`] run with its [`FG`] color (or a default light
/// gray) and a cell height equal to its rect height.
fn build_display_list(dom: &Dom, rects: &[(NodeId, Rect)]) -> Vec<DisplayItem> {
    let mut items = Vec::new();
    for &(id, rect) in rects {
        let Some(node) = dom.node(id) else { continue };
        if let Some(text) = node.text.as_deref() {
            if let Some(bg) = style_color(dom, id, BG) {
                items.push(DisplayItem::Rect {
                    rect,
                    color: bg,
                    radius: style_radius(dom, id),
                });
            }
            let fg = style_color(dom, id, FG).unwrap_or(DEFAULT_FG);
            items.push(DisplayItem::Text {
                origin: rect.origin,
                text: text.to_string(),
                color: fg,
                size: rect.size.h,
            });
        } else if let Some(bg) = style_color(dom, id, BG) {
            items.push(DisplayItem::Rect {
                rect,
                color: bg,
                radius: style_radius(dom, id),
            });
        }
    }
    items
}

/// Return the topmost node whose absolute rect contains `point`, or `None`.
///
/// `layout.rects` is in back-to-front tree order (parents before children, earlier
/// siblings before later), so scanning from the end yields the visually topmost hit.
pub fn hit_test(layout: &LayoutResult, point: Point) -> Option<NodeId> {
    layout
        .rects
        .iter()
        .rev()
        .find(|(_, rect)| rect_contains(rect, point))
        .map(|(id, _)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_core::Emitter;
    use canopy_dom::Dom;
    use canopy_protocol::ElementTag;
    use canopy_traits::OpSink;

    fn dom_from(e: Emitter) -> Dom {
        let mut e = e;
        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();
        dom
    }

    #[test]
    fn row_places_second_child_at_first_width_plus_gap() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let row = e.create_element(ElementTag::new(1));
        e.append(col, row);
        e.set_inline_style(row, DIRECTION, "row");
        e.set_inline_style(row, GAP, "10");
        // Two explicitly-sized child boxes so the geometry is exact.
        let a = e.create_element(ElementTag::new(2));
        e.append(row, a);
        e.set_inline_style(a, WIDTH, "30");
        e.set_inline_style(a, HEIGHT, "20");
        let b = e.create_element(ElementTag::new(2));
        e.append(row, b);
        e.set_inline_style(b, WIDTH, "40");
        e.set_inline_style(b, HEIGHT, "20");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 200.0, h: 100.0 });

        let a_rect = lay.rects.iter().find(|(id, _)| *id == a).unwrap().1;
        let b_rect = lay.rects.iter().find(|(id, _)| *id == b).unwrap().1;
        assert_eq!(a_rect.size.w, 30.0);
        // Second child starts at first child's width + the gap.
        assert_eq!(b_rect.origin.x, a_rect.size.w + 10.0);
        assert_eq!(b_rect.origin.x, 40.0);
        // Start-aligned on the cross axis: no vertical offset.
        assert_eq!(a_rect.origin.y, 0.0);
        assert_eq!(b_rect.origin.y, 0.0);
    }

    #[test]
    fn padding_insets_a_child() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, PADDING, "5");
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, WIDTH, "20");
        e.set_inline_style(child, HEIGHT, "10");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 100.0, h: 100.0 });

        let child_rect = lay.rects.iter().find(|(id, _)| *id == child).unwrap().1;
        // Child is inset by the padding on both axes.
        assert_eq!(child_rect.origin, Point { x: 5.0, y: 5.0 });
    }

    #[test]
    fn text_node_gets_baked_font_size() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let t = e.create_text("ab"); // 2 chars
        e.append(col, t);
        e.set_inline_style(t, HEIGHT, "20"); // 20/8 = scale 2 -> 16px tall
        e.set_inline_style(t, FG, "#ffd040");

        let dom = dom_from(e);
        let (scene, lay) = layout(&dom, Size { w: 100.0, h: 50.0 });

        // Baked-font metrics: scale = 20/8 = 2 -> 2 chars * 8 * 2 = 32 wide, 16 tall.
        let t_rect = lay.rects.iter().find(|(id, _)| *id == t).unwrap().1;
        assert_eq!(t_rect.size, Size { w: 32.0, h: 16.0 });

        // The text leaf emits a Text run carrying the content, foreground, and the
        // snapped cell height.
        let text_item = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Text {
                    text, color, size, ..
                } => Some((text.clone(), *color, *size)),
                _ => None,
            })
            .expect("text run");
        assert_eq!(text_item.0, "ab");
        assert_eq!(
            text_item.1,
            Color {
                r: 0xff,
                g: 0xd0,
                b: 0x40,
                a: 255
            }
        );
        assert_eq!(text_item.2, 16.0);
    }

    #[test]
    fn background_paints_behind_children() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, BG, "#202830");
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, WIDTH, "30");
        e.set_inline_style(child, HEIGHT, "20");
        e.set_inline_style(child, BG, "#ffffff");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });

        // The column background is emitted before the child's background.
        let col_bg = Color {
            r: 0x20,
            g: 0x28,
            b: 0x30,
            a: 255,
        };
        let child_bg = Color {
            r: 0xff,
            g: 0xff,
            b: 0xff,
            a: 255,
        };
        let col_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Rect { color, .. } if *color == col_bg))
            .unwrap();
        let child_idx = scene
            .items
            .iter()
            .position(|i| matches!(i, DisplayItem::Rect { color, .. } if *color == child_bg))
            .unwrap();
        assert!(col_idx < child_idx, "parent background must paint first");
    }

    #[test]
    fn radius_style_flows_onto_the_emitted_rect() {
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, BG, "#313244");
        e.set_inline_style(card, WIDTH, "40");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, RADIUS, "8");

        let dom = dom_from(e);
        let (scene, _) = layout(&dom, Size { w: 100.0, h: 100.0 });

        let radius = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Rect { radius, .. } => Some(*radius),
                _ => None,
            })
            .expect("background rect");
        assert_eq!(radius, 8.0);
    }

    #[test]
    fn hit_test_finds_deepest_node_and_misses_outside() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_inline_style(col, WIDTH, "100");
        e.set_inline_style(col, HEIGHT, "100");
        let child = e.create_element(ElementTag::new(2));
        e.append(col, child);
        e.set_inline_style(child, WIDTH, "30");
        e.set_inline_style(child, HEIGHT, "20");

        let dom = dom_from(e);
        let (_, lay) = layout(&dom, Size { w: 100.0, h: 100.0 });

        // A point inside the child resolves to the child (topmost), not the column.
        assert_eq!(hit_test(&lay, Point { x: 10.0, y: 10.0 }), Some(child));
        // Inside the column but outside the child resolves to the column.
        assert_eq!(hit_test(&lay, Point { x: 80.0, y: 80.0 }), Some(col));
        // Past everything resolves to nothing.
        assert_eq!(hit_test(&lay, Point { x: 500.0, y: 500.0 }), None);
    }
}
