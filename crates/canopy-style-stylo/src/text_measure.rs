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

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

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
