//! L3 **PAINT**: rasterize the cascaded + laid-out Stylo tree to pixels.
//!
//! The crate's L1 (cascade, [`StyloEngine::resolve_styles`]) and L2
//! ([`StyloEngine::layout`]) stages produce, for every element in DFS order, a
//! flat [`ComputedStyle`](canopy_traits::ComputedStyle) and an absolute
//! border-box [`Rect`](canopy_traits::Rect). This stage zips those two streams and
//! paints them into a [`canopy_render_soft::Buffer`] — the same CPU rasterizer the
//! rest of the host uses — so the full style → layout → paint path is exercised end
//! to end without a GPU or a window.
//!
//! ## Paint order
//!
//! Backgrounds are painted **back-to-front**. [`StyloEngine::element_dfs_order`] is
//! pre-order (parent before child), which is exactly the right order for opaque
//! backgrounds: a child's box is painted *after* (i.e. on top of) its ancestor's, so
//! a nested element correctly draws over its parent. Text for an element is blitted
//! immediately after that element's background, at the box origin, using the
//! element's resolved foreground `color` and `font_size`.

use canopy_render_soft::Buffer;
use canopy_traits::{Color, Point, Rect, Size};

use crate::{NodeKind, StyloEngine};

/// Scale a color's alpha by `opacity` (clamped to `[0,1]`), leaving RGB intact.
///
/// This is the seam's `opacity` model: every painted color's alpha is multiplied
/// by the element's opacity before it hits the (alpha-blending) buffer, so an
/// `opacity:0.5` box composites its background/border/text at half strength over
/// whatever is behind it. `opacity == 1.0` returns the color unchanged.
fn with_opacity(c: Color, opacity: f32) -> Color {
    let o = opacity.clamp(0.0, 1.0);
    Color {
        r: c.r,
        g: c.g,
        b: c.b,
        a: (c.a as f32 * o).round() as u8,
    }
}

/// Shrink `rect` by `edge` logical px on every side (the content/padding box
/// inside a uniform `edge`-wide border frame). The size is clamped at `0.0`, so a
/// border thicker than the box yields an empty interior instead of an inverted
/// (negative-size) rect.
fn inset_rect(rect: Rect, edge: f32) -> Rect {
    Rect {
        origin: Point {
            x: rect.origin.x + edge,
            y: rect.origin.y + edge,
        },
        size: Size {
            w: (rect.size.w - 2.0 * edge).max(0.0),
            h: (rect.size.h - 2.0 * edge).max(0.0),
        },
    }
}

/// Paint a run of **Ahem** text as solid 1em squares.
///
/// Ahem is the WPT test font in which every glyph is a filled `1em` square. For a
/// string of `N` characters at `font_size` `s` px, this fills `N` adjacent
/// `s`-by-`s` squares in `color`, advancing `s` px per character from `origin` —
/// the exact geometry [`text_measure`](crate::text_measure) sizes an Ahem leaf to,
/// so the painted ink and the laid-out box agree (and an Ahem reftest matches).
///
/// Whitespace characters advance the pen but paint nothing (Ahem's space glyph is
/// empty), so a single `' '` between words leaves a 1em gap rather than a filled
/// square. Only one line is drawn (the seam paints single-line runs at the box
/// origin); a fractional `font_size` rounds the square down to whole pixels via
/// [`Buffer::fill_rect`].
fn paint_ahem_text(buffer: &mut Buffer, origin: Point, text: &str, color: Color, font_size: f32) {
    let s = font_size.max(0.0);
    if s == 0.0 {
        return;
    }
    for (i, ch) in text.chars().enumerate() {
        // Whitespace advances but draws no square (matches Ahem's empty space glyph).
        if ch.is_whitespace() {
            continue;
        }
        let x = origin.x + i as f32 * s;
        buffer.fill_rect(
            Rect {
                origin: Point { x, y: origin.y },
                size: Size { w: s, h: s },
            },
            color,
        );
    }
}

impl StyloEngine {
    /// Render the cascaded + laid-out tree into an RGBA8 [`Buffer`] of size
    /// `viewport`.
    ///
    /// Runs [`layout`](StyloEngine::layout) (which itself resolves styles), then for
    /// each `(slab, rect)` pair reads the element's flat
    /// [`ComputedStyle`](canopy_traits::ComputedStyle) and paints:
    ///   * its background as a filled rect (only when `background.a > 0`), and
    ///   * its text — only when the element is a **text-bearing leaf** (its children
    ///     are all [`NodeKind::Text`], with non-whitespace content) — blitted at the
    ///     box origin in the element's foreground `color` at `font_size`.
    ///
    /// The buffer is cleared to opaque white first so painted boxes sit on a defined,
    /// reftest-stable background.
    pub fn render(&mut self, viewport: Size) -> Buffer {
        let rects = self.layout(viewport);
        let order = self.element_dfs_order();

        let mut buffer = Buffer::new(viewport.w as usize, viewport.h as usize);
        buffer.clear(Color {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        });

        // DFS order is parent-before-child, the correct back-to-front order for
        // opaque backgrounds.
        for (&slab, &rect) in order.iter().zip(rects.iter()) {
            let Some(style) = self.computed_style_for(slab) else {
                continue;
            };

            let bw = style.border_width.max(0.0);
            let has_border = bw > 0.0 && style.border_color.a > 0;
            let radius = style.border_radius.max(0.0);

            // Border + background. The border is a *frame*: fill the full border-box
            // in the border color (rounded to `radius`), then fill the inset
            // content/padding box in the background — so the border color survives
            // only in the `bw`-wide ring between the two rects. With no border we
            // fill the background directly. Both fills run through `with_opacity`,
            // so the element's `opacity` fades every painted pixel uniformly.
            if has_border {
                buffer.fill_round_rect(
                    rect,
                    with_opacity(style.border_color, style.opacity),
                    radius,
                );
                // Inset rect: shrink by `bw` on every edge (clamped so a border
                // thicker than the box collapses to an empty interior rather than
                // wrapping to a huge size).
                let inset = inset_rect(rect, bw);
                if style.background.a > 0 && inset.size.w > 0.0 && inset.size.h > 0.0 {
                    // Inner corner radius shrinks with the border, but never below 0.
                    let inner_r = (radius - bw).max(0.0);
                    buffer.fill_round_rect(
                        inset,
                        with_opacity(style.background, style.opacity),
                        inner_r,
                    );
                }
            } else if style.background.a > 0 {
                // No border: paint the background directly (skip fully transparent
                // fills so they don't clobber what is behind them at alpha 0).
                buffer.fill_round_rect(rect, with_opacity(style.background, style.opacity), radius);
            }

            // Text: only a *text-bearing leaf* (an element whose children are all
            // Text — never a container that also has element children) renders text,
            // at the box origin in the element's foreground color + font size, faded
            // by opacity. Using the same leaf rule as layout/measurement
            // (`leaf_text`) is what kills the white-square artifact: the HTML parser
            // inserts whitespace-only text nodes (e.g. `"\n      "`) BETWEEN block
            // children, and the old `direct_text_child` grabbed that inter-element
            // whitespace; the leading newline then rasterized as a baked-font TOFU
            // box (a stray light square) at every container's top-left corner.
            if let Some(text) = self.leaf_text(slab) {
                let fg = with_opacity(style.color, style.opacity);
                if style.is_ahem {
                    // Ahem renders each glyph as a SOLID 1em square. The baked bitmap
                    // font can't reproduce that, so for `font-family: Ahem` we draw
                    // the metrics directly: `N` chars at `font_size` S px become `N`
                    // adjacent S-by-S filled squares advancing by S from the box
                    // origin — exactly the geometry `text_measure` sizes the box to
                    // (so paint and layout agree, and Ahem reftests match).
                    paint_ahem_text(&mut buffer, rect.origin, &text, fg, style.font_size);
                } else {
                    buffer.blit_text(rect.origin, &text, fg, style.font_size);
                }
            }
        }

        buffer
    }

    /// Render to raw RGBA bytes plus dimensions, for later reftest pixel comparison.
    ///
    /// Returns `(rgba, width, height)` where `rgba` is row-major RGBA8 — the buffer's
    /// [`data`](Buffer::data) copied out.
    pub fn render_to_rgba(&mut self, viewport: Size) -> (Vec<u8>, usize, usize) {
        let buffer = self.render(viewport);
        let w = viewport.w as usize;
        let h = viewport.h as usize;
        (buffer.data().to_vec(), w, h)
    }

    /// The paintable text of a **text-bearing leaf**: an element whose children are
    /// *all* [`NodeKind::Text`] (no element children), concatenated. Returns `None`
    /// for a container (any element child), for an element with no text, or when the
    /// concatenated text is whitespace-only.
    ///
    /// This is the paint-side mirror of layout's
    /// [`direct_text_of`](StyloEngine::direct_text_of): both agree that a *container*
    /// has no text of its own to draw/measure. Critically, this is what suppresses
    /// the white-square artifact — the HTML parser leaves whitespace-only text nodes
    /// (e.g. `"\n      "`) between block children, and rendering that inter-element
    /// whitespace drew the leading newline as a baked-font TOFU box (a stray light
    /// square) at each container's top-left corner.
    fn leaf_text(&self, slab: usize) -> Option<String> {
        let node = self.doc.nodes.get(slab)?;
        if !node.is_element() {
            return None;
        }
        let mut text = String::new();
        for &child in &node.children {
            match &self.doc.nodes[child].kind {
                NodeKind::Text(s) => text.push_str(s),
                // An element child makes this a container, not a text leaf.
                NodeKind::Element { .. } | NodeKind::Document => return None,
            }
        }
        // Whitespace-only content paints nothing (and a stray newline would TOFU).
        if text.trim().is_empty() {
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

    /// THE smoke test: a `width:100px; height:50px; background:#ff0000` box, rendered
    /// into a 120×60 viewport, must put a red pixel inside the box.
    #[test]
    fn renders_a_red_box() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        // Root element wrapper (matches the cascade's "first element child of node 0"
        // root rule), then the styled box under it.
        let html = doc.add_element(0, "html", None, &[]);
        let boxed = doc.add_element(html, "div", None, &[]);
        engine
            .document_mut()
            .set_inline_style(boxed, "width:100px; height:50px; background:#ff0000");

        let buffer = engine.render(Size { w: 120.0, h: 60.0 });
        let data = buffer.data();
        let w = 120usize;

        // A pixel well inside the 100×50 box at the origin.
        let (x, y) = (10usize, 10usize);
        let i = (y * w + x) * 4;
        let (r, g, b, a) = (data[i], data[i + 1], data[i + 2], data[i + 3]);
        assert!(
            r >= 250 && g <= 5 && b <= 5,
            "pixel at ({x},{y}) should be red, got rgba=({r},{g},{b},{a})"
        );

        // And a pixel outside the box keeps the white clear color.
        let (ox, oy) = (110usize, 55usize);
        let oi = (oy * w + ox) * 4;
        assert_eq!(
            (data[oi], data[oi + 1], data[oi + 2]),
            (255, 255, 255),
            "pixel outside the box should be the white background"
        );
    }

    /// The enriched-seam smoke test: a `width:100px; height:50px; background:#0000ff;
    /// border:4px solid #ff0000; border-radius:8px` box must paint a RED border frame
    /// at the edge and BLUE inside it.
    #[test]
    fn renders_bordered_box() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let boxed = doc.add_element(html, "div", None, &[]);
        engine.document_mut().set_inline_style(
            boxed,
            "width:100px;height:50px;background:#0000ff;border:4px solid #ff0000;border-radius:8px",
        );

        let buffer = engine.render(Size { w: 120.0, h: 60.0 });
        let data = buffer.data();
        let w = 120usize;
        let px = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            (data[i], data[i + 1], data[i + 2], data[i + 3])
        };

        // The 4px border ring: a pixel ~2px in from the top-left edge, away from the
        // 8px-radius rounded corner, is on the border frame -> RED.
        let (br, bg, bb, _ba) = px(20, 2);
        assert!(
            br >= 250 && bg <= 5 && bb <= 5,
            "border pixel at (20,2) should be red, got ({br},{bg},{bb})"
        );

        // Well inside the inset content box -> BLUE.
        let (ir, ig, ib, _ia) = px(50, 25);
        assert!(
            ib >= 250 && ir <= 5 && ig <= 5,
            "interior pixel at (50,25) should be blue, got ({ir},{ig},{ib})"
        );

        // The rounded corner at (0,0) is carved away (radius 8) -> stays white.
        let (cr, cg, cb, _ca) = px(0, 0);
        assert_eq!(
            (cr, cg, cb),
            (255, 255, 255),
            "the rounded top-left corner should keep the white background"
        );
    }

    /// Opacity multiplies every painted color's alpha: a half-opaque opaque-blue box
    /// over white composites to a lighter blue (red/green channels lifted toward
    /// white), proving `opacity` is applied.
    #[test]
    fn opacity_fades_background() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let boxed = doc.add_element(html, "div", None, &[]);
        engine.document_mut().set_inline_style(
            boxed,
            "width:100px;height:50px;background:#0000ff;opacity:0.5",
        );

        let buffer = engine.render(Size { w: 120.0, h: 60.0 });
        let data = buffer.data();
        let w = 120usize;
        let i = (10 * w + 10) * 4;
        let (r, g, b) = (data[i], data[i + 1], data[i + 2]);
        // 0.5 * blue over white ≈ (128, 128, 255): blue stays high, red/green ~half.
        assert!(
            b >= 250 && (100..=160).contains(&r) && (100..=160).contains(&g),
            "opacity:0.5 blue over white should be ~(128,128,255), got ({r},{g},{b})"
        );
    }

    /// PART 1 — Ahem text renders as solid 1em squares. `font-family:Ahem` text
    /// "XXX" at 20px must paint THREE adjacent solid 20×20 squares in the foreground
    /// color from the box origin (Ahem metrics), NOT baked-font glyphs.
    #[test]
    fn ahem_text_renders_solid_squares() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        // An auto-sized leaf so the box leaves the text's measured size (3*20=60 wide,
        // 20 tall). Foreground red so the squares are unambiguous over white.
        let boxed = doc.add_element(html, "div", None, &[]);
        engine
            .document_mut()
            .set_inline_style(boxed, "font-family:Ahem; font-size:20px; color:#ff0000");
        engine.document_mut().add_text(boxed, "XXX");

        let buffer = engine.render(Size { w: 120.0, h: 60.0 });
        let data = buffer.data();
        let w = 120usize;
        let px = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            (data[i], data[i + 1], data[i + 2])
        };

        // The box origin is (0,0) (html has no UA margin; div none either). Each of
        // the 3 glyphs is a solid 20×20 red square: sample each square's center.
        for (k, cx) in [10usize, 30, 50].into_iter().enumerate() {
            let (r, g, b) = px(cx, 10);
            assert_eq!(
                (r, g, b),
                (255, 0, 0),
                "Ahem square #{k} center ({cx},10) should be solid red, got ({r},{g},{b})"
            );
        }

        // The squares are SOLID (filled), not glyph outlines: a near-corner pixel of
        // the first square is also red.
        let (r, g, b) = px(1, 1);
        assert_eq!(
            (r, g, b),
            (255, 0, 0),
            "Ahem square #0 should be fully filled to its corner, got ({r},{g},{b})"
        );

        // Just past the 3rd square (x >= 60) is outside the run -> white background.
        let (r, g, b) = px(62, 10);
        assert_eq!(
            (r, g, b),
            (255, 255, 255),
            "past the 3rd Ahem square should be the white background, got ({r},{g},{b})"
        );
    }

    /// PART 2 — the white-square artifact is gone. The HTML parser inserts
    /// whitespace-only text nodes (with leading newlines) between block children;
    /// the old paint grabbed that inter-element whitespace and rasterized the newline
    /// as a baked-font TOFU box — a stray light square at every container's top-left.
    /// A container with a dark background and indented (whitespace-bearing) markup
    /// must now paint NO stray light pixel at its origin.
    #[test]
    fn no_white_square_artifact_on_container() {
        // `.box` is a dark container whose markup is INDENTED, so the parser leaves a
        // leading "\n      " text node before its (block) child. The container itself
        // is therefore NOT a text leaf — it must draw no glyphs at all. Its only child
        // is a separate dark `.child` block with no text, so the whole container area
        // (background + child) is uniformly dark. The foreground text color is
        // near-white (#e0e0e0) — exactly what made the old whitespace-newline TOFU box
        // visible at the container's top-left corner.
        let html = "<style>\
            .box{background:#101418;color:#e0e0e0;padding:4px}\
            .child{background:#101418;width:40px;height:20px}\
            </style>\
            <body><div class=\"box\">\n      <div class=\"child\"></div>\n    </div></body>";
        let mut engine = StyloEngine::from_html(html);
        let buffer = engine.render(Size { w: 200.0, h: 120.0 });
        let data = buffer.data();
        let w = 200usize;

        // Find the `.box` container's border-box so we can probe its top-left corner.
        let rects = engine.layout(Size { w: 200.0, h: 120.0 });
        let order = engine.element_dfs_order();
        let mut box_origin = None;
        for (&slab, &rect) in order.iter().zip(rects.iter()) {
            if let NodeKind::Element { classes, .. } = &engine.doc.nodes[slab].kind {
                if classes.iter().any(|c| c.as_ref() == "box") {
                    box_origin = Some((rect.origin.x as usize, rect.origin.y as usize));
                }
            }
        }
        let (ox, oy) = box_origin.expect(".box container should be laid out");

        // Scan the container's top-left corner region (where the old TOFU box landed,
        // ~8×8 at the content origin). A dark container with NO text leaf of its own
        // must paint nothing light there — every pixel stays dark.
        for y in oy..(oy + 12).min(120) {
            for x in ox..(ox + 12).min(w) {
                let i = (y * w + x) * 4;
                let (r, g, b) = (data[i], data[i + 1], data[i + 2]);
                // The TOFU was the near-white text color (#e0e0e0). Assert nothing in
                // this corner is light: every channel must stay below 0x80.
                assert!(
                    r < 0x80 || g < 0x80 || b < 0x80,
                    "stray light (TOFU) pixel at ({x},{y}) = ({r},{g},{b}); white-square artifact regressed"
                );
            }
        }
    }
}
