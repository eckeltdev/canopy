//! Build a Canopy [`DisplayList`] from the cascaded + laid-out Stylo tree.
//!
//! This is the **retained-scene** sibling of [`crate::paint`]. Where `paint.rs`
//! rasterizes the tree straight into a [`canopy_render_soft::Buffer`] on the CPU,
//! this stage lowers the exact same `(slab, ComputedStyle, Rect)` streams into the
//! Canopy-owned [`DisplayList`] of [`DisplayItem`]s — the *backend-neutral* scene the
//! constrained [`canopy_layout_taffy::build_scene`] also produces. Because it is the
//! same shape, any [`canopy_traits::Renderer`] can rasterize it: the CPU software
//! path, or — the point of this module — the GPU `canopy-render-vello` path on Metal.
//!
//! ## What it emits (matching `canopy-layout-taffy::build_display_list`)
//!
//! Walking [`StyloEngine::element_dfs_order`] (pre-order, parent-before-child, the
//! correct back-to-front order for opaque backgrounds), for each element it emits, in
//! this order:
//!
//! * a [`DisplayItem::Rect`] for a **non-transparent background** (`background.a > 0`),
//!   carrying the element's `border_radius` as the corner radius — same as the Taffy
//!   path's per-element background fill; and
//! * a [`DisplayItem::Text`] for an element with **direct text** (a leaf whose only
//!   children are text), at the box origin, in the element's resolved foreground
//!   `color` at its `font_size`, sized to the box width with left alignment.
//!
//! Every emitted color is faded by the element's `opacity` (a straight alpha
//! multiplier), exactly as `paint.rs` and the Taffy path do, so a translucent subtree
//! lowers to translucent primitives the renderer blends. At full opacity (the common
//! case) the bytes are unchanged.
//!
//! Borders are intentionally *not* lowered here: the [`DisplayItem`] vocabulary has no
//! frame primitive (the Taffy path emits no borders either), so this matches the
//! constrained scene exactly. The CPU [`crate::paint`] path, which owns its own
//! rasterizer, still draws borders.

use canopy_traits::{Color, DisplayItem, DisplayList, Size};

use crate::{NodeKind, StyloEngine};

/// Scale a straight-alpha color's alpha by `opacity` (clamped to `[0, 1]`), leaving
/// RGB intact. Mirrors `paint::with_opacity` and the Taffy path's `fade`, so the
/// lowered scene fades identically to how the CPU path paints. `opacity >= 1.0`
/// returns the color unchanged (the common, fully-opaque case).
fn fade(c: Color, opacity: f32) -> Color {
    let o = opacity.clamp(0.0, 1.0);
    if o >= 1.0 {
        return c;
    }
    Color {
        r: c.r,
        g: c.g,
        b: c.b,
        // Round-to-nearest, matching `paint::with_opacity`'s `.round()`.
        a: (c.a as f32 * o).round() as u8,
    }
}

impl StyloEngine {
    /// Build a Canopy [`DisplayList`] for the whole document laid out within
    /// `viewport`.
    ///
    /// Runs [`layout`](StyloEngine::layout) (which resolves the cascade first), then
    /// walks the elements in DFS order and lowers each to display items: a background
    /// [`DisplayItem::Rect`] (when the background is non-transparent) and a
    /// [`DisplayItem::Text`] run for a direct-text leaf. The result is the **same**
    /// [`DisplayList`] shape `canopy-layout-taffy::build_scene` produces, so a
    /// [`canopy_traits::Renderer`] — notably the GPU `canopy-render-vello` rasterizer
    /// — can paint it without knowing it came from Stylo.
    ///
    /// This does **not** touch [`render`](StyloEngine::render)/[`crate::paint`]: it is
    /// a parallel lowering that consumes the same cascade + layout streams.
    pub fn build_display_list(&mut self, viewport: Size) -> DisplayList {
        // `layout` resolves styles, builds the Taffy tree, and returns the absolute
        // border-box rect per element in DFS order. `element_dfs_order` returns the
        // matching slab ids in the same order, so we zip them by index.
        let rects = self.layout(viewport);
        let order = self.element_dfs_order();

        let mut items = Vec::new();
        // DFS order is parent-before-child, the correct back-to-front order: a child's
        // background is pushed after (drawn over) its ancestor's.
        for (&slab, &rect) in order.iter().zip(rects.iter()) {
            let Some(style) = self.computed_style_for(slab) else {
                continue;
            };
            let opacity = style.opacity;

            // Background: emit a filled (optionally rounded) rect only when the
            // background is non-transparent, faded by the element's opacity. This is
            // the per-element fill the Taffy path emits; borders have no display-item
            // primitive, so they are not lowered here.
            if style.background.a > 0 {
                items.push(DisplayItem::Rect {
                    rect,
                    color: fade(style.background, opacity),
                    radius: style.border_radius.max(0.0),
                });
            }

            // Text: a direct-text leaf emits one Text run at the box origin, in the
            // element's resolved foreground `color` at its `font_size`, faded by
            // opacity. `box_w` is the laid-out box width and `align` is left/start
            // (0.0) — the constrained scene's default; the renderer measures the run
            // and applies any alignment slack itself.
            if let Some(text) = self.direct_text_str(slab) {
                items.push(DisplayItem::Text {
                    origin: rect.origin,
                    text,
                    color: fade(style.color, opacity),
                    size: style.font_size,
                    box_w: rect.size.w,
                    align: 0.0,
                });
            }
        }

        DisplayList { items }
    }

    /// The concatenated text of an element's **direct** [`NodeKind::Text`] children,
    /// or `None` when the element has none. Mirrors `paint::direct_text_child` (kept
    /// crate-private to each module). Returns the text even for an element that also
    /// has element children, concatenating runs of adjacent text.
    fn direct_text_str(&self, slab: usize) -> Option<String> {
        let node = self.doc.nodes.get(slab)?;
        let mut text = String::new();
        for &child in &node.children {
            if let NodeKind::Text(s) = &self.doc.nodes[child].kind {
                text.push_str(s);
            }
        }
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_traits::Size;

    /// `build_display_list` on a small styled tree emits, in DFS order, a background
    /// [`DisplayItem::Rect`] for the styled box and a [`DisplayItem::Text`] run for its
    /// text — the exact shape `canopy-layout-taffy::build_scene` produces.
    #[test]
    fn lowers_a_background_rect_and_a_text_run() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        // Root element wrapper (the cascade's "first element child of node 0" root),
        // then a styled box carrying a background, a foreground color, a font size,
        // and a direct text child.
        let html = doc.add_element(0, "html", None, &[]);
        let boxed = doc.add_element(html, "div", None, &[]);
        engine.document_mut().set_inline_style(
            boxed,
            "width:100px;height:50px;background:#ff0000;color:#00ff00;font-size:20px",
        );
        engine.document_mut().add_text(boxed, "hi");

        let scene = engine.build_display_list(Size { w: 120.0, h: 60.0 });

        // A red background rect for the box.
        let bg = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Rect { color, .. }
                    if color.r >= 250 && color.g <= 5 && color.b <= 5 =>
                {
                    Some(*color)
                }
                _ => None,
            })
            .expect("a red background Rect was emitted");
        assert_eq!(
            (bg.r, bg.g, bg.b, bg.a),
            (0xff, 0x00, 0x00, 0xff),
            "the background rect carries the box's opaque red"
        );

        // A green text run carrying the content, color, and font size.
        let (run_text, run_color, run_size) = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Text {
                    text, color, size, ..
                } => Some((text.clone(), *color, *size)),
                _ => None,
            })
            .expect("a Text run was emitted for the direct-text leaf");
        assert_eq!(run_text, "hi");
        assert_eq!(
            (run_color.r, run_color.g, run_color.b),
            (0x00, 0xff, 0x00),
            "the text run carries the element's resolved green foreground"
        );
        assert_eq!(
            run_size, 20.0,
            "the text run's size is the resolved font-size"
        );
    }

    /// A transparent background emits **no** Rect — only `background.a > 0` lowers.
    /// (The root `html`/`div` with no `background` has a transparent computed
    /// background, so a plain text-only tree emits no background rect for it.)
    #[test]
    fn transparent_background_emits_no_rect() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let leaf = doc.add_element(html, "div", None, &[]);
        // No background set anywhere; just a text leaf.
        engine
            .document_mut()
            .set_inline_style(leaf, "color:#ffffff;font-size:16px");
        engine.document_mut().add_text(leaf, "x");

        let scene = engine.build_display_list(Size { w: 100.0, h: 40.0 });
        let rects = scene
            .items
            .iter()
            .filter(|i| matches!(i, DisplayItem::Rect { .. }))
            .count();
        assert_eq!(rects, 0, "no element has a non-transparent background");
        // The text run is still emitted.
        assert!(
            scene
                .items
                .iter()
                .any(|i| matches!(i, DisplayItem::Text { .. })),
            "the text leaf still lowers to a Text run"
        );
    }

    /// `border-radius` flows onto the emitted background rect's corner radius, the
    /// same field the Taffy path threads through.
    #[test]
    fn border_radius_flows_onto_the_rect() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let card = doc.add_element(html, "div", None, &[]);
        engine.document_mut().set_inline_style(
            card,
            "width:40px;height:40px;background:#313244;border-radius:8px",
        );

        let scene = engine.build_display_list(Size { w: 100.0, h: 100.0 });
        let radius = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Rect { radius, .. } => Some(*radius),
                _ => None,
            })
            .expect("a background rect");
        assert_eq!(
            radius, 8.0,
            "border-radius lowers onto the rect's corner radius"
        );
    }

    /// `opacity` fades the emitted background rect's alpha, matching the CPU paint
    /// path and the Taffy `fade`.
    #[test]
    fn opacity_fades_the_emitted_rect_alpha() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let card = doc.add_element(html, "div", None, &[]);
        engine.document_mut().set_inline_style(
            card,
            "width:40px;height:40px;background:#313244;opacity:0.5",
        );

        let scene = engine.build_display_list(Size { w: 100.0, h: 100.0 });
        let color = scene
            .items
            .iter()
            .find_map(|i| match i {
                DisplayItem::Rect { color, .. } => Some(*color),
                _ => None,
            })
            .expect("a background rect");
        // 255 * 0.5 rounds to 128; RGB untouched (straight-alpha fade).
        assert_eq!(color.a, 128, "alpha is scaled to ~half by opacity");
        assert_eq!((color.r, color.g, color.b), (0x31, 0x32, 0x44));
    }
}
