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
use canopy_traits::{
    BoxShadow, Color, DisplayItem, GradientAxis, GradientDirection, LinearGradient, Point, Rect,
    Size,
};

use crate::text_measure::{self, RasterizedRun};
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

/// `src` over `dst` with straight alpha `a` (0..=255), rounded.
///
/// `out = src·a + dst·(255−a)`, all integer, with `+127` for round-to-nearest so
/// the blend is symmetric. The same primitive `canopy_render_text::composite_coverage`
/// uses, copied here (this crate doesn't depend on that one).
#[inline]
fn blend(src: u8, dst: u8, a: u32) -> u8 {
    let inv = 255 - a;
    let v = (u32::from(src) * a + u32::from(dst) * inv + 127) / 255;
    v as u8
}

/// Composite a straight-alpha coverage mask onto `buffer` in `ink`, top-left at
/// `origin`.
///
/// `coverage` is row-major, one byte per pixel, exactly `width * height` bytes (the
/// [`RasterizedRun`] layout). Each byte is the fractional ink coverage: `0` leaves
/// the destination untouched, `255` overwrites it with `ink`, anything between
/// blends `ink` over the existing pixel (`out = ink·a + dst·(1 − a)`). The ink's own
/// alpha folds into the coverage, so an opacity-faded text run stays faded. This is a
/// port of `canopy_render_text::composite_coverage` — the antialiasing the 1-bit
/// baked font can't express comes from these partial-coverage edge pixels.
fn composite_coverage(
    buffer: &mut Buffer,
    origin: Point,
    coverage: &[u8],
    width: u32,
    height: u32,
    ink: Color,
) {
    let ox = origin.x.max(0.0) as usize;
    let oy = origin.y.max(0.0) as usize;
    let w = width as usize;
    let h = height as usize;

    for row in 0..h {
        let py = oy + row;
        if py >= buffer.height() {
            break;
        }
        let row_base = row * w;
        for col in 0..w {
            let px = ox + col;
            if px >= buffer.width() {
                break;
            }
            let cov = coverage[row_base + col];
            if cov == 0 {
                continue; // fully transparent: leave the destination as-is.
            }
            // Fold the ink's own alpha into the coverage.
            let a = (u32::from(cov) * u32::from(ink.a)) / 255;
            if a == 0 {
                continue;
            }
            let out = if a >= 255 {
                [ink.r, ink.g, ink.b, 255]
            } else {
                let dst = buffer.pixel(px, py);
                [
                    blend(ink.r, dst[0], a),
                    blend(ink.g, dst[1], a),
                    blend(ink.b, dst[2], a),
                    blend(255, dst[3], a),
                ]
            };
            // We already composited against the destination, so store straight
            // (never back through `fill_rect`, which would alpha-blend again).
            buffer.set_pixel(px, py, out);
        }
    }
}

/// Linearly interpolate between two straight-alpha colors at `t` in `[0, 1]`.
#[inline]
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color {
        r: mix(a.r, b.r),
        g: mix(a.g, b.g),
        b: mix(a.b, b.b),
        a: mix(a.a, b.a),
    }
}

/// Fill `rect` with a two-stop [`LinearGradient`], one scanline (vertical) or one
/// column-strip (horizontal) at a time.
///
/// Each strip is a flat [`Buffer::fill_rect`] in the interpolated stop color, so the
/// gradient composites over whatever is behind it (alpha-blended). The interpolation
/// parameter runs `0 → 1` from the axis start edge to the end edge. `opacity` fades
/// every strip's alpha uniformly (the element-opacity model).
fn fill_linear_gradient(buffer: &mut Buffer, rect: Rect, grad: LinearGradient, opacity: f32) {
    let w = rect.size.w.max(0.0);
    let h = rect.size.h.max(0.0);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    match grad.axis {
        GradientAxis::Vertical => {
            let rows = h.ceil() as usize;
            let denom = (rows.max(1) as f32 - 1.0).max(1.0);
            for row in 0..rows {
                let t = row as f32 / denom;
                let c = with_opacity(lerp_color(grad.start, grad.end, t), opacity);
                buffer.fill_rect(
                    Rect {
                        origin: Point {
                            x: rect.origin.x,
                            y: rect.origin.y + row as f32,
                        },
                        size: Size { w, h: 1.0 },
                    },
                    c,
                );
            }
        }
        GradientAxis::Horizontal => {
            let cols = w.ceil() as usize;
            let denom = (cols.max(1) as f32 - 1.0).max(1.0);
            for col in 0..cols {
                let t = col as f32 / denom;
                let c = with_opacity(lerp_color(grad.start, grad.end, t), opacity);
                buffer.fill_rect(
                    Rect {
                        origin: Point {
                            x: rect.origin.x + col as f32,
                            y: rect.origin.y,
                        },
                        size: Size { w: 1.0, h },
                    },
                    c,
                );
            }
        }
    }
}

/// Paint a soft outset [`BoxShadow`] behind `box_rect`.
///
/// The shadow is a rect the size of the element's border-box, translated by the
/// shadow's `(dx, dy)`, with a `blur`-wide feathered falloff on every side. We
/// approximate the Gaussian blur with a cheap distance ramp: the solid shadow core
/// is the offset border-box inset by the blur, and a `blur`-px border around it
/// fades the shadow alpha linearly to 0 — enough to read as a soft drop shadow on a
/// CPU buffer without a real separable blur. `opacity` fades the whole shadow.
fn paint_box_shadow(buffer: &mut Buffer, box_rect: Rect, shadow: BoxShadow, opacity: f32) {
    if shadow.color.a == 0 {
        return;
    }
    let blur = shadow.blur.max(0.0);
    // The shadow's outer bounds: the border-box, offset, then inflated by `blur`.
    let ox = box_rect.origin.x + shadow.dx;
    let oy = box_rect.origin.y + shadow.dy;
    let bw = box_rect.size.w;
    let bh = box_rect.size.h;

    let x0 = (ox - blur).floor();
    let y0 = (oy - blur).floor();
    let x1 = (ox + bw + blur).ceil();
    let y1 = (oy + bh + blur).ceil();

    let base = with_opacity(shadow.color, opacity);
    if base.a == 0 {
        return;
    }

    // The solid (inner) rect where the shadow is at full strength.
    let core_x0 = ox;
    let core_y0 = oy;
    let core_x1 = ox + bw;
    let core_y1 = oy + bh;

    let px_start_x = x0.max(0.0) as usize;
    let px_start_y = y0.max(0.0) as usize;
    let px_end_x = (x1.max(0.0) as usize).min(buffer.width());
    let px_end_y = (y1.max(0.0) as usize).min(buffer.height());

    for py in px_start_y..px_end_y {
        for px in px_start_x..px_end_x {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            // Distance OUTSIDE the solid core (0 inside, grows toward the blur edge).
            let dx = (core_x0 - fx).max(fx - core_x1).max(0.0);
            let dy = (core_y0 - fy).max(fy - core_y1).max(0.0);
            let dist = (dx * dx + dy * dy).sqrt();
            // Inside the core => full alpha; within `blur` => linear falloff; beyond
            // => nothing.
            let falloff = if blur <= 0.0 {
                if dist <= 0.0 {
                    1.0
                } else {
                    0.0
                }
            } else {
                (1.0 - dist / blur).clamp(0.0, 1.0)
            };
            if falloff <= 0.0 {
                continue;
            }
            let a = (u32::from(base.a) as f32 * falloff).round() as u32;
            if a == 0 {
                continue;
            }
            let a = a.min(255);
            let dst = buffer.pixel(px, py);
            let out = [
                blend(base.r, dst[0], a),
                blend(base.g, dst[1], a),
                blend(base.b, dst[2], a),
                blend(255, dst[3], a),
            ];
            buffer.set_pixel(px, py, out);
        }
    }
}

/// Rasterize one **box-model** [`DisplayItem`] (as produced by
/// [`push_box_items`](crate::display_list::push_box_items)) into `buffer`.
///
/// Colors arrive already faded by the element's opacity (the shared producer applies it),
/// so the gradient/shadow helpers are reused with `opacity == 1.0`. `Text`/`Glyphs` items
/// are intentionally skipped here — `render` draws text in its own real-glyph/Ahem pass,
/// which a flat `DisplayItem::Text` cannot reproduce.
fn rasterize_box_item(buffer: &mut Buffer, item: &DisplayItem) {
    match item {
        DisplayItem::Rect {
            rect,
            color,
            radius,
        } => buffer.fill_round_rect(*rect, *color, *radius),
        DisplayItem::Border {
            rect,
            color,
            width,
            radius,
        } => buffer.stroke_rect(*rect, *color, *width, *radius),
        DisplayItem::Gradient {
            rect,
            stops,
            direction,
        } => {
            // Reconstruct the two-stop gradient the helper takes; colors are pre-faded.
            let s = stops.as_slice();
            let grad = LinearGradient {
                start: s.first().map_or(Color::default(), |st| st.color),
                end: s.last().map_or(Color::default(), |st| st.color),
                axis: match direction {
                    GradientDirection::Vertical => GradientAxis::Vertical,
                    GradientDirection::Horizontal => GradientAxis::Horizontal,
                },
            };
            fill_linear_gradient(buffer, *rect, grad, 1.0);
        }
        DisplayItem::Shadow {
            rect,
            color,
            blur,
            offset,
        } => {
            let shadow = BoxShadow {
                dx: offset.x,
                dy: offset.y,
                blur: *blur,
                color: *color,
            };
            paint_box_shadow(buffer, *rect, shadow, 1.0);
        }
        // Text/Glyphs are drawn by `render`'s real-glyph pass; future variants skipped.
        _ => {}
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

        // Reused per element: the box-model item buffer.
        let mut items: Vec<DisplayItem> = Vec::new();

        // DFS order is parent-before-child, the correct back-to-front order for
        // opaque backgrounds.
        for (&slab, &rect) in order.iter().zip(rects.iter()) {
            let Some(style) = self.computed_style_for(slab) else {
                continue;
            };

            // Box model (shadow → background → gradient → border) from the SHARED
            // producer — the *same* items the GPU `build_display_list` scene uses — then
            // rasterized with the CPU primitives. One source of truth, so the CPU and GPU
            // tiers cannot diverge on box ordering or geometry.
            items.clear();
            crate::display_list::push_box_items(&mut items, rect, &style);
            for item in &items {
                rasterize_box_item(&mut buffer, item);
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
                    // Non-Ahem text: shape + rasterize REAL antialiased glyphs via
                    // cosmic-text/swash (the same shared FontSystem the measure path
                    // uses), then alpha-over composite the coverage mask in the
                    // foreground color — gray feathered edges, not a 1-bit baked
                    // blit. Family `""` selects the bundled sans fallback, the face
                    // every non-Ahem request resolves to in our deterministic font
                    // set. The rasterized run is the tight INK bbox; the measure path
                    // sizes the line box at `font_size`, so center the ink within
                    // that line box (vertical shift) the way `canopy-render-text`
                    // does, keeping the glyphs from riding the box's top edge.
                    let run: RasterizedRun =
                        text_measure::rasterize_run(&text, style.font_size, "");
                    if run.width > 0 && run.height > 0 {
                        let vshift = ((style.font_size - run.height as f32) * 0.5).max(0.0);
                        let origin = Point {
                            x: rect.origin.x,
                            y: rect.origin.y + vshift,
                        };
                        composite_coverage(
                            &mut buffer,
                            origin,
                            &run.coverage,
                            run.width,
                            run.height,
                            fg,
                        );
                    }
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

    /// PART 1 (real glyphs) — non-Ahem text renders REAL antialiased glyphs.
    ///
    /// Black text on the white clear background: every glyph edge feathers through
    /// the gray midtones. The headline assertion is that *intermediate* pixels
    /// (strictly between pure black ink and pure white background, on all channels)
    /// exist — a 1-bit baked font can never produce those. This is the proof the CPU
    /// text path now rasterizes cosmic-text/swash coverage, not blocky cells.
    #[test]
    fn non_ahem_text_renders_antialiased_glyphs() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        // No font-family => the bundled sans fallback (NOT Ahem). Big size for plenty
        // of curved/slanted edges. Black on white so AA edges are gray.
        let boxed = doc.add_element(html, "div", None, &[]);
        engine
            .document_mut()
            .set_inline_style(boxed, "font-size:32px; color:#000000");
        engine.document_mut().add_text(boxed, "Ag");

        let buffer = engine.render(Size { w: 160.0, h: 64.0 });
        let data = buffer.data();
        let (w, h) = (160usize, 64usize);

        let (mut full_ink, mut pure_bg, mut intermediate) = (0usize, 0usize, 0usize);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let (r, g, b) = (data[i], data[i + 1], data[i + 2]);
                if r == 255 && g == 255 && b == 255 {
                    pure_bg += 1;
                } else if r == 0 && g == 0 && b == 0 {
                    full_ink += 1;
                } else {
                    intermediate += 1;
                }
            }
        }
        println!(
            "non-Ahem 'Ag'@32: full_ink={full_ink}, intermediate(AA)={intermediate}, bg={pure_bg}"
        );

        assert!(full_ink > 0, "expected fully-inked black glyph cores");
        assert!(pure_bg > 0, "expected untouched white background");
        // THE antialiasing assertion: gray edge pixels a 1-bit font cannot make.
        assert!(
            intermediate > 0,
            "expected antialiased intermediate-gray edge pixels; got 0 (blocky/baked?)"
        );
    }

    /// PART 1 (real glyphs) — antialiased glyph edges feather over a PAINTED
    /// background, not just the clear color.
    ///
    /// White text over a solid mid-gray box: a real composite reads the destination
    /// (the gray box) under each edge, so some edge pixel lands strictly between the
    /// box gray (64) and full white ink (255). That's only possible if the coverage
    /// mask blends over what was already painted, exactly the `composite_coverage`
    /// contract.
    #[test]
    fn non_ahem_text_feathers_over_painted_background() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let boxed = doc.add_element(html, "div", None, &[]);
        engine.document_mut().set_inline_style(
            boxed,
            "width:160px;height:64px;background:#404040;font-size:32px;color:#ffffff",
        );
        engine.document_mut().add_text(boxed, "Ag");

        let buffer = engine.render(Size { w: 160.0, h: 64.0 });
        let data = buffer.data();
        let (w, h) = (160usize, 64usize);

        // Some pixel strictly between the box gray (0x40 = 64) and full ink (255):
        // an AA edge blended over the box, not the white clear color.
        let mut feathered = false;
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let r = data[i];
                if r > 64 && r < 255 {
                    feathered = true;
                }
            }
        }
        assert!(
            feathered,
            "an AA glyph edge must blend over the painted gray box (64 < r < 255)"
        );
    }

    /// PART 2 (gradient) — a `linear-gradient(to bottom, red, blue)` background
    /// fills the box with a vertical red→blue ramp.
    ///
    /// The top rows are dominated by red, the bottom rows by blue, and a mid row is a
    /// genuine blend (both channels present) — proving the two-stop gradient
    /// interpolates down the box rather than painting a flat fill.
    #[test]
    fn vertical_linear_gradient_ramps_red_to_blue() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let boxed = doc.add_element(html, "div", None, &[]);
        engine.document_mut().set_inline_style(
            boxed,
            "width:80px;height:80px;background:linear-gradient(to bottom, #ff0000, #0000ff)",
        );

        let buffer = engine.render(Size { w: 100.0, h: 100.0 });
        let data = buffer.data();
        let w = 100usize;
        let px = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            (data[i], data[i + 1], data[i + 2])
        };

        let (tr, _tg, tb) = px(40, 2); // near top
        let (mr, _mg, mb) = px(40, 40); // middle
        let (br, _bg, bb) = px(40, 78); // near bottom

        // Top is red-dominant, bottom is blue-dominant.
        assert!(
            tr > tb + 100,
            "top should be red-dominant, got r={tr} b={tb}"
        );
        assert!(
            bb > br + 100,
            "bottom should be blue-dominant, got r={br} b={bb}"
        );
        // The middle is a real blend (both red and blue meaningfully present, and
        // the red is between the endpoints).
        assert!(
            mr > 40 && mb > 40 && mr < tr && mb < bb,
            "middle should blend red+blue, got r={mr} b={mb}"
        );
    }

    /// PART 2 (gradient) — a `linear-gradient(to right, …)` ramps horizontally.
    ///
    /// Same proof on the other axis: left edge red-dominant, right edge blue-dominant.
    #[test]
    fn horizontal_linear_gradient_ramps_left_to_right() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let boxed = doc.add_element(html, "div", None, &[]);
        engine.document_mut().set_inline_style(
            boxed,
            "width:80px;height:80px;background:linear-gradient(to right, #ff0000, #0000ff)",
        );

        let buffer = engine.render(Size { w: 100.0, h: 100.0 });
        let data = buffer.data();
        let w = 100usize;
        let px = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            (data[i], data[i + 1], data[i + 2])
        };

        let (lr, _lg, lb) = px(2, 40); // near left
        let (rr, _rg, rb) = px(78, 40); // near right
        assert!(
            lr > lb + 100,
            "left should be red-dominant, got r={lr} b={lb}"
        );
        assert!(
            rb > rr + 100,
            "right should be blue-dominant, got r={rr} b={rb}"
        );
    }

    /// PART 2 (shadow) — a `box-shadow` paints a soft dark halo OUTSIDE the box.
    ///
    /// A small box with a blurred, offset shadow over a white background: just
    /// beyond the box's bottom-right corner (where the positive offset pushes the
    /// shadow) the pixels must be darkened below pure white — and the falloff must
    /// fade (an inner shadow pixel is darker than one farther out). Proves the soft
    /// outset shadow renders behind/around the box.
    #[test]
    fn box_shadow_paints_soft_halo_outside_box() {
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let boxed = doc.add_element(html, "div", None, &[]);
        engine.document_mut().set_inline_style(
            boxed,
            "width:40px;height:40px;background:#ffffff;box-shadow:6px 6px 6px #000000",
        );

        let buffer = engine.render(Size { w: 120.0, h: 120.0 });
        let data = buffer.data();
        let w = 120usize;

        // Find the box origin (it carries a UA body margin of 8px).
        let rects = engine.layout(Size { w: 120.0, h: 120.0 });
        let order = engine.element_dfs_order();
        let mut box_rect = None;
        for (&slab, &rect) in order.iter().zip(rects.iter()) {
            if let NodeKind::Element { name, .. } = &engine.doc.nodes[slab].kind {
                if name.local.as_ref() == "div" {
                    box_rect = Some(rect);
                }
            }
        }
        let rect = box_rect.expect("the div should be laid out");
        let (rx, ry) = (rect.origin.x as usize, rect.origin.y as usize);
        let (rw, rh) = (rect.size.w as usize, rect.size.h as usize);

        let lum = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            data[i] as u32 + data[i + 1] as u32 + data[i + 2] as u32
        };

        // A pixel just past the bottom-right corner, inside the shadow's offset core,
        // must be darkened below pure white (765 = 255*3).
        let inner = lum(rx + rw + 2, ry + rh + 2);
        assert!(
            inner < 765,
            "shadow should darken just past the box's bottom-right, got luminance {inner}"
        );
        // And the shadow fades outward: a pixel farther out is lighter (closer to
        // white) than the inner one.
        let outer = lum(rx + rw + 11, ry + rh + 11);
        assert!(
            outer > inner,
            "shadow must fade with distance: inner {inner} should be darker than outer {outer}"
        );
        // The white background far from the box is untouched.
        let far = lum(rx + rw + 40, ry + rh + 40);
        assert_eq!(far, 765, "background far from the shadow stays white");
    }
}
