//! **Text measurement** for the full-tier layout: shape a text leaf's content
//! with [`cosmic-text`] so an auto-sized box can leave the size of its text.
//!
//! [`StyloEngine::layout`](crate::StyloEngine::layout) runs Taffy over the
//! cascaded tree. Taffy is a pure box-model engine: a *leaf* with no explicit
//! `width`/`height` would otherwise collapse to zero, because Taffy has no idea
//! what's inside it. The web's answer is the **measure function**: Taffy calls
//! back into the host for each such leaf, handing it the constraints, and the
//! host returns the intrinsic size of the leaf's content. This module is that
//! callback for text.
//!
//! ## Determinism (Ahem)
//!
//! Measurement that depends on the host's installed fonts is not reproducible.
//! So, exactly like the Web Platform Tests, we shape against **Ahem** — a
//! metrics-perfect test font where *every* glyph is a 1em square. At
//! `font-size: 20px` the string `"XXXXX"` measures to **exactly 100px** wide by
//! **20px** tall, on every machine. Ahem is loaded from
//! [`AHEM_PATH`] (`/tmp/wpt/fonts/Ahem.ttf`) under the family name
//! [`AHEM_FAMILY`] (`"Ahem"`), so author CSS `font-family: Ahem` resolves to it.
//! A bundled DejaVu sans face is also registered as the default/sans fallback so
//! non-Ahem text still measures to *something* deterministic.
//!
//! WPT reftests opt in with `font-family: Ahem` (often via a UA-level
//! `/fonts/ahem.css`); our [`MeasureContext::family`] carries the cascaded
//! family name straight from Stylo, so a `font-family: Ahem` declaration on the
//! element selects Ahem here with no special casing.
//!
//! ## Line box
//!
//! We set the cosmic-text line height **equal to the font size** (`Metrics::new(px, px)`),
//! so the measured height of a single line is exactly `px` — matching Ahem's
//! 1em ascent+descent box (and what WPT asserts). Width is the widest laid-out
//! run advance. If Taffy hands us a known width we wrap to it (multi-line);
//! otherwise we report the single-line intrinsic width.
//!
//! [`cosmic-text`]: https://docs.rs/cosmic-text

use std::sync::Mutex;

use cosmic_text::{
    Attrs, Buffer, Color as CtColor, Family, FontSystem, Metrics, Shaping, SwashCache,
};

/// Where the Ahem test font lives. Ahem is a metrics-perfect font: every glyph
/// is a 1em square, so `"XXXXX"` at 20px is exactly 100x20 — deterministic, and
/// exactly what the Web Platform Tests shape against.
pub const AHEM_PATH: &str = "/tmp/wpt/fonts/Ahem.ttf";

/// The family name Ahem is registered under. Author CSS `font-family: Ahem`
/// (as WPT reftests use) resolves to the loaded face by this name.
pub const AHEM_FAMILY: &str = "Ahem";

/// A bundled fallback sans face, so non-Ahem text still measures deterministically
/// (no dependency on the host's installed fonts). Reuses the DejaVu face that the
/// `canopy-text-parley` engine already ships in-tree.
const SANS_BYTES: &[u8] = include_bytes!("../../canopy-text-parley/fonts/DejaVuSansMono.ttf");
const SANS_FAMILY: &str = "Canopy Fallback Sans";

/// The **real** internal family name of [`SANS_BYTES`] (the bundled DejaVu Sans
/// Mono face). [`SANS_FAMILY`] above is only a *generic* mapping name wired via
/// `set_sans_serif_family`; a `Family::Name` lookup must use the face's actual
/// name or cosmic-text falls back to the first loaded font (Ahem — every glyph a
/// solid square). The rasterization path therefore selects the fallback face by
/// THIS name so non-Ahem text shapes against real glyphs, not Ahem squares.
const SANS_REAL_FAMILY: &str = "DejaVu Sans Mono";

/// Per-element measurement context attached to a Taffy leaf node.
///
/// This is the Taffy `TaffyTree` **node-context** type (replacing the unit type
/// `()` used for non-text leaves): the measure closure receives `Option<&MeasureContext>`
/// for each leaf and shapes [`text`](Self::text) at [`font_size`](Self::font_size)
/// in [`family`](Self::family) to return the leaf's intrinsic size.
#[derive(Clone, Debug)]
pub struct MeasureContext {
    /// The leaf's text content (its direct Text child's string).
    pub text: String,
    /// Cascaded `font-size`, in CSS pixels.
    pub font_size: f32,
    /// Cascaded first `font-family` name (e.g. `"Ahem"`). Empty -> fallback sans.
    pub family: String,
}

/// A shared, lazily-initialized [`FontSystem`] preloaded with Ahem (+ fallback).
///
/// Building a `FontSystem` parses every face, so we keep one process-global,
/// behind a `Mutex` (cosmic-text shaping needs `&mut`). The measure closure runs
/// single-threaded within one `compute_layout` call, so contention is nil; the
/// `Mutex` only guards the lazy init and satisfies `Sync`.
static FONTS: Mutex<Option<FontSystem>> = Mutex::new(None);

/// A shared, lazily-initialized [`SwashCache`] — the rasterized-glyph cache that
/// turns a shaped glyph into an 8-bit alpha-coverage bitmap.
///
/// Paired with [`FONTS`]: the L3 paint stage shapes a non-Ahem text run against the
/// shared [`FontSystem`] and rasterizes each glyph through this cache, exactly as
/// [`canopy_text_parley::TextEngine`](https://docs.rs/canopy-text-parley) does for
/// the capable-tier renderer. Behind its own `Mutex` for the same single-threaded
/// reason as `FONTS` (cosmic-text rasterization needs `&mut`).
static SWASH: Mutex<Option<SwashCache>> = Mutex::new(None);

/// A rasterized text run: the tight ink bounding box plus an 8-bit alpha-coverage
/// mask (row-major, one byte per pixel, `width * height` bytes; `0` = transparent,
/// `255` = full ink). This is the same coverage-mask contract
/// [`canopy_text_parley::Glyphs`](https://docs.rs/canopy-text-parley) hands back —
/// composite it over a surface in any ink color (alpha = coverage).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RasterizedRun {
    /// Tight ink-bbox width in pixels.
    pub width: u32,
    /// Tight ink-bbox height in pixels.
    pub height: u32,
    /// Row-major 8-bit alpha coverage, `width * height` bytes.
    pub coverage: Vec<u8>,
}

/// Shape `ctx.text` and return its intrinsic pixel [`taffy::Size`].
///
/// * `known_dimensions` — if `width` is `Some`, wrap to it (multi-line);
///   otherwise lay out a single un-wrapped line. A known `height` is returned
///   verbatim (the caller already decided it).
/// * `available_space` — intentionally NOT consulted to pick a wrap width. The
///   CSS `min-/max-/fit-content` *keywords* are resolved up-front in the layout
///   pass via [`intrinsic_width`], so this closure stays a faithful single-line
///   measurer: wrapping is driven ONLY by an explicit `known_dimensions.width`.
///   This matters because Taffy's own grid/flex auto-track sizing probes leaves
///   with both `MinContent` and `MaxContent` available space and a `None` known
///   width; honoring those here (e.g. wrapping to a definite available width, or
///   collapsing to widest-word under `MinContent`) changes the measured height
///   and regresses auto-track baseline/alignment reftests that depend on the
///   single-line content contribution.
///
/// Height is `line_height * line_count` with `line_height == font_size`, so a
/// single line of Ahem text at 20px is exactly 20px tall.
pub fn measure_text(
    known_dimensions: taffy::Size<Option<f32>>,
    _available_space: taffy::Size<taffy::AvailableSpace>,
    ctx: &MeasureContext,
) -> taffy::Size<f32> {
    // A known dimension is authoritative — return it (don't re-shape into it).
    if let (Some(w), Some(h)) = (known_dimensions.width, known_dimensions.height) {
        return taffy::Size {
            width: w,
            height: h,
        };
    }

    let px = ctx.font_size.max(0.0);
    if ctx.text.is_empty() || px == 0.0 {
        return taffy::Size {
            width: known_dimensions.width.unwrap_or(0.0),
            height: known_dimensions.height.unwrap_or(0.0),
        };
    }

    let mut guard = FONTS.lock().expect("text-measure font system poisoned");
    let font_system = guard.get_or_insert_with(build_font_system);

    // Line height == font size: a single Ahem line at `px` is exactly `px` tall.
    let metrics = Metrics::new(px, px);
    let mut buffer = Buffer::new(font_system, metrics);

    // Wrap to a known width if Taffy gave us one; else a single un-wrapped line.
    buffer.set_size(font_system, known_dimensions.width, None);

    let family = if ctx.family.is_empty() {
        Family::Name(SANS_FAMILY)
    } else {
        Family::Name(ctx.family.as_str())
    };
    let attrs = Attrs::new().family(family);
    buffer.set_text(font_system, &ctx.text, &attrs, Shaping::Advanced);
    buffer.shape_until_scroll(font_system, false);

    // Widest run advance; line count * line height for the box height.
    let mut width: f32 = 0.0;
    let mut line_count: u32 = 0;
    for run in buffer.layout_runs() {
        width = width.max(run.line_w);
        line_count += 1;
    }
    let height = px * line_count.max(1) as f32;

    taffy::Size {
        width: known_dimensions.width.unwrap_or(width),
        height: known_dimensions.height.unwrap_or(height),
    }
}

/// Shape `text` as a SINGLE un-wrapped line and return its advance width, in CSS
/// pixels. The primitive both the intrinsic helpers and (indirectly) the measure
/// closure build on. Standalone (no `available_space` branch) so callers from
/// inside [`measure_text`] cannot recurse.
fn single_line_width(text: &str, px: f32, family: &str) -> f32 {
    if text.is_empty() || px <= 0.0 {
        return 0.0;
    }
    let mut guard = FONTS.lock().expect("text-measure font system poisoned");
    let font_system = guard.get_or_insert_with(build_font_system);

    let metrics = Metrics::new(px, px);
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(font_system, None, None); // no wrap: one line

    let fam = if family.is_empty() {
        Family::Name(SANS_FAMILY)
    } else {
        Family::Name(family)
    };
    let attrs = Attrs::new().family(fam);
    buffer.set_text(font_system, text, &attrs, Shaping::Advanced);
    buffer.shape_until_scroll(font_system, false);

    let mut width = 0.0f32;
    for run in buffer.layout_runs() {
        width = width.max(run.line_w);
    }
    width
}

/// The widest unbreakable run (longest word) of `ctx.text`, in CSS pixels.
///
/// Soft-wrap opportunities in CSS are (at minimum) ASCII whitespace, so the
/// widest unbreakable run is the widest whitespace-delimited word, each measured
/// as its own un-wrapped line. This is the CSS **min-content** inline size.
///
/// We compute it by measuring words directly rather than asking cosmic-text to
/// wrap to width 0.0 — a zero-width wrap breaks at every *glyph*, which both
/// collapses the width to a single character and inflates the line count.
fn widest_word_width(ctx: &MeasureContext) -> f32 {
    let mut widest = 0.0f32;
    for word in ctx.text.split_whitespace() {
        widest = widest.max(single_line_width(word, ctx.font_size.max(0.0), &ctx.family));
    }
    widest
}

/// Pre-resolve the intrinsic content *width* of a text leaf, in CSS pixels.
///
/// Used by the layout pass to honor the CSS `min-content` / `max-content`
/// keywords on the inline axis. Taffy 0.11 cannot represent these keywords, and a
/// block child with an `auto` width is *stretched* to fill its container rather
/// than sized to content — so the layout pass calls this to compute a definite
/// width and pins it into the taffy style as a fixed length.
///
/// * `min_content == true`  -> widest unbreakable run (longest word).
/// * `min_content == false` -> single un-wrapped line (max-content).
pub fn intrinsic_width(ctx: &MeasureContext, min_content: bool) -> f32 {
    if min_content {
        widest_word_width(ctx)
    } else {
        single_line_width(&ctx.text, ctx.font_size.max(0.0), &ctx.family)
    }
}

/// Shape `text` at `px` pixels in `family` and **rasterize** it into an
/// antialiased 8-bit coverage mask (the L3 paint stage's real-glyph path).
///
/// Mirrors [`canopy_text_parley::TextEngine::rasterize`](https://docs.rs/canopy-text-parley):
/// it shapes a single un-wrapped line against the shared [`FONTS`] font system,
/// rasterizes each placed glyph through the shared [`SWASH`] cache, and accumulates
/// the per-pixel coverage into a tight ink-bbox bitmap. The returned
/// [`RasterizedRun`]'s `width`/`height` are the run's **ink bounding box** (leading
/// and top blank trimmed), so the caller composites it at the box origin and the
/// glyphs sit where their ink falls.
///
/// An empty / all-whitespace run (or a non-positive `px`) returns an empty
/// (zero-area) mask — there is nothing to composite. `family` is the cascaded first
/// `font-family` (empty -> the bundled sans fallback), the same selector the
/// measure path uses, so paint and layout shape against the same face.
pub fn rasterize_run(text: &str, px: f32, family: &str) -> RasterizedRun {
    if text.trim().is_empty() || px <= 0.0 {
        return RasterizedRun::default();
    }

    let mut font_guard = FONTS.lock().expect("text-measure font system poisoned");
    let font_system = font_guard.get_or_insert_with(build_font_system);
    let mut swash_guard = SWASH.lock().expect("text-measure swash cache poisoned");
    let swash = swash_guard.get_or_insert_with(SwashCache::new);

    // Line height == font size (mirrors the measure path): one tight line.
    let metrics = Metrics::new(px, px);
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(font_system, None, None); // no wrap: a single line

    // Empty family => the bundled DejaVu fallback by its REAL face name (NOT the
    // generic `SANS_FAMILY` mapping, which a `Family::Name` lookup won't resolve —
    // it would silently fall back to Ahem's solid squares). A non-Ahem author
    // family is requested by name and resolves through the same fallback.
    let fam = if family.is_empty() {
        Family::Name(SANS_REAL_FAMILY)
    } else {
        Family::Name(family)
    };
    let attrs = Attrs::new().family(fam);
    buffer.set_text(font_system, text, &attrs, Shaping::Advanced);
    buffer.shape_until_scroll(font_system, false);

    // First pass: collect every placed glyph pixel and the run's ink bounds. A
    // glyph's swash image is positioned relative to the glyph pen origin via its
    // `placement`, which `physical()`/`with_pixels` already fold into the (x, y).
    struct Px {
        x: i32,
        y: i32,
        a: u8,
    }
    let mut pixels: Vec<Px> = Vec::new();
    let (mut min_x, mut min_y) = (i32::MAX, i32::MAX);
    let (mut max_x, mut max_y) = (i32::MIN, i32::MIN);

    let runs: Vec<Vec<_>> = buffer
        .layout_runs()
        .map(|run| {
            run.glyphs
                .iter()
                .map(|g| g.physical((0.0, run.line_y), 1.0))
                .collect()
        })
        .collect();

    for run_glyphs in &runs {
        for pg in run_glyphs {
            let base = CtColor::rgba(255, 255, 255, 255);
            swash.with_pixels(font_system, pg.cache_key, base, |ox, oy, c| {
                let a = c.a();
                if a == 0 {
                    return;
                }
                let x = pg.x + ox;
                let y = pg.y + oy;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                pixels.push(Px { x, y, a });
            });
        }
    }

    // No ink (e.g. the run shaped to only blank glyphs): nothing to composite.
    if pixels.is_empty() {
        return RasterizedRun::default();
    }

    let width = (max_x - min_x + 1) as u32;
    let height = (max_y - min_y + 1) as u32;
    let mut coverage = vec![0u8; (width as usize) * (height as usize)];
    for p in &pixels {
        let lx = (p.x - min_x) as u32;
        let ly = (p.y - min_y) as u32;
        let idx = (ly * width + lx) as usize;
        // Glyphs don't overlap here, but `max` is safe if they ever do.
        let slot = &mut coverage[idx];
        *slot = (*slot).max(p.a);
    }

    RasterizedRun {
        width,
        height,
        coverage,
    }
}

/// Build the measurement [`FontSystem`]: load Ahem under [`AHEM_FAMILY`] and a
/// bundled DejaVu face as the sans/default fallback, with an empty platform
/// fallback so results never depend on the host's installed fonts.
fn build_font_system() -> FontSystem {
    let mut db = cosmic_text::fontdb::Database::new();

    // Ahem — the deterministic 1em-square test font (what WPT shapes against).
    // Load from disk; if it's missing we still register the fallback so non-Ahem
    // text measures, and Ahem requests degrade to it rather than panicking.
    match std::fs::read(AHEM_PATH) {
        Ok(bytes) => {
            db.load_font_data(bytes);
        }
        Err(e) => {
            // Non-fatal: keep going with the fallback face only.
            eprintln!("text-measure: could not load Ahem from {AHEM_PATH}: {e}");
        }
    }

    // Bundled fallback sans (in-tree DejaVu face), and point the generic families
    // at it so a bare `sans-serif`/`monospace`/`serif` request resolves here.
    db.load_font_data(SANS_BYTES.to_vec());
    db.set_sans_serif_family(SANS_FAMILY);
    db.set_monospace_family(SANS_FAMILY);
    db.set_serif_family(SANS_FAMILY);

    FontSystem::new_with_locale_and_db("en-US".to_string(), db)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `rasterize_run` produces a tight, antialiased coverage mask for non-Ahem
    /// text: real partial-coverage edge pixels (the AA a 1-bit font can't make), and
    /// it must NOT be the all-255 solid block Ahem would yield (the bug where a
    /// `Family::Name` lookup silently fell through to Ahem's squares).
    #[test]
    fn rasterize_run_is_antialiased_not_ahem_squares() {
        let run = rasterize_run("Ag", 32.0, "");
        assert!(run.width > 0 && run.height > 0, "must have ink area");
        assert_eq!(
            run.coverage.len(),
            (run.width as usize) * (run.height as usize),
            "coverage must be width*height bytes"
        );
        let ink = run.coverage.iter().filter(|&&c| c != 0).count();
        let partial = run.coverage.iter().filter(|&&c| c > 0 && c < 255).count();
        let total = run.coverage.len();
        println!(
            "rasterize_run('Ag',32) -> {}x{}: ink={ink} partial(AA)={partial} of {total}",
            run.width, run.height
        );
        assert!(ink > 0, "expected ink pixels");
        assert!(
            ink < total,
            "a solid all-ink block means Ahem squares leaked in (got {ink}/{total})"
        );
        assert!(
            partial > 0,
            "expected antialiased partial-coverage pixels, got 0"
        );
    }

    /// An all-whitespace or empty run rasterizes to an empty mask (nothing to paint).
    #[test]
    fn rasterize_run_empty_for_blank() {
        assert_eq!(rasterize_run("", 16.0, ""), RasterizedRun::default());
        assert_eq!(rasterize_run("   ", 16.0, ""), RasterizedRun::default());
        assert_eq!(rasterize_run("x", 0.0, ""), RasterizedRun::default());
    }

    /// Ahem is metrics-perfect: `"XXXXX"` at 20px is exactly 100x20.
    #[test]
    fn ahem_xxxxx_is_100_by_20() {
        let ctx = MeasureContext {
            text: "XXXXX".to_string(),
            font_size: 20.0,
            family: AHEM_FAMILY.to_string(),
        };
        let size = measure_text(
            taffy::Size {
                width: None,
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::MaxContent,
                height: taffy::AvailableSpace::MaxContent,
            },
            &ctx,
        );
        println!("measure XXXXX@20 Ahem = {size:?}");
        assert!(
            (size.width - 100.0).abs() <= 2.0,
            "width should be ~100, got {}",
            size.width
        );
        assert!(
            (size.height - 20.0).abs() <= 2.0,
            "height should be ~20, got {}",
            size.height
        );
    }

    /// max-content: "XX YY" at 20px Ahem is one un-wrapped line of 5 glyphs = 100px.
    #[test]
    fn max_content_single_line() {
        let ctx = MeasureContext {
            text: "XX YY".to_string(),
            font_size: 20.0,
            family: AHEM_FAMILY.to_string(),
        };
        let w = intrinsic_width(&ctx, false);
        println!("max-content width = {w}");
        assert!(
            (w - 100.0).abs() <= 2.0,
            "max-content should be ~100 (5 glyphs @ 20px), got {w}"
        );
    }

    /// min-content: "XX YY" at 20px Ahem collapses to the widest word = 2 glyphs = 40px.
    #[test]
    fn min_content_widest_word() {
        let ctx = MeasureContext {
            text: "XX YY".to_string(),
            font_size: 20.0,
            family: AHEM_FAMILY.to_string(),
        };
        let w = intrinsic_width(&ctx, true);
        println!("min-content width = {w}");
        assert!(
            (w - 40.0).abs() <= 2.0,
            "min-content should be ~40 (widest word, 2 glyphs @ 20px), got {w}"
        );
    }

    /// A known width is returned verbatim (the leaf was already sized).
    #[test]
    fn known_dimensions_pass_through() {
        let ctx = MeasureContext {
            text: "XXXXX".to_string(),
            font_size: 20.0,
            family: AHEM_FAMILY.to_string(),
        };
        let size = measure_text(
            taffy::Size {
                width: Some(42.0),
                height: Some(13.0),
            },
            taffy::Size {
                width: taffy::AvailableSpace::MaxContent,
                height: taffy::AvailableSpace::MaxContent,
            },
            &ctx,
        );
        assert_eq!(size.width, 42.0);
        assert_eq!(size.height, 13.0);
    }
}
