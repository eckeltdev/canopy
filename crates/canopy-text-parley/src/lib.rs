//! Canopy's capable-tier text engine: shape and **rasterize real, antialiased
//! glyphs** into pixels with [`cosmic-text`] (which shapes with `rustybuzz`/
//! `swash` and rasterizes with `swash`).
//!
//! This is the `std` replacement for the 8×8 baked bitmap font
//! (`canopy-text-baked`) used on constrained tiers. Where the baked font emits
//! 1-bit-per-pixel monospace cells, this engine produces a full antialiased
//! 8-bit **alpha-coverage** mask the caller can blit with any ink color.
//!
//! # Determinism
//!
//! For reproducible measurement and rasterization across machines, the engine
//! **bundles its own font** ([DejaVu Sans Mono], embedded via [`include_bytes!`])
//! and loads *only* that font — no system fonts, no platform fallback. The font
//! file and its permissive license live under `fonts/` in this crate.
//!
//! # Output
//!
//! [`TextEngine::rasterize`] returns [`Glyphs`]: the run's pixel [`Size`] plus a
//! tightly-packed **8-bit alpha-coverage** bitmap, row-major, one byte per pixel,
//! `width * height` bytes long. `0` is fully transparent (background), `255` is
//! full ink. The caller composites it over a surface using its own ink color —
//! this is straight (non-premultiplied) coverage, not RGBA. To get an RGBA pixel:
//! `(color.r, color.g, color.b, coverage)`.
//!
//! [`cosmic-text`]: https://docs.rs/cosmic-text
//! [DejaVu Sans Mono]: https://dejavu-fonts.github.io/

use canopy_traits::{Color, Size};
use cosmic_text::{
    Attrs, Buffer, CacheKeyFlags, Color as CtColor, Family, FontSystem, Metrics, Shaping,
    SwashCache,
};

/// The bundled, permissively-licensed font (DejaVu Sans Mono). Embedding it makes
/// shaping and rasterization deterministic and independent of the host's fonts.
const FONT_BYTES: &[u8] = include_bytes!("../fonts/DejaVuSansMono.ttf");

/// The family name of [`FONT_BYTES`], used so shaping always selects the bundled
/// face rather than a system font.
const FONT_FAMILY: &str = "DejaVu Sans Mono";

/// A rasterized text run: the run's pixel size plus an 8-bit alpha-coverage mask.
///
/// `coverage` is row-major, one byte per pixel, exactly `width * height` bytes.
/// `0` = transparent background, `255` = full ink. The mask carries **coverage
/// only** (straight alpha, no color); composite it with the caller's ink color.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Glyphs {
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Row-major 8-bit alpha coverage, `width * height` bytes.
    pub coverage: Vec<u8>,
}

impl Glyphs {
    /// The run's pixel size as a [`Size`] (integer pixels expressed as `f32`).
    #[must_use]
    pub fn size(&self) -> Size {
        Size {
            w: self.width as f32,
            h: self.height as f32,
        }
    }

    /// Number of pixels with non-zero ink coverage.
    #[must_use]
    pub fn ink_pixels(&self) -> usize {
        self.coverage.iter().filter(|&&c| c != 0).count()
    }

    /// Expand the coverage mask into straight-alpha RGBA bytes using `color` as
    /// the ink, returning `width * height * 4` bytes (`R, G, B, A` per pixel,
    /// `A` = coverage). Convenience for callers that want RGBA directly.
    #[must_use]
    pub fn to_rgba(&self, color: Color) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.coverage.len() * 4);
        for &c in &self.coverage {
            out.push(color.r);
            out.push(color.g);
            out.push(color.b);
            // Fold the requested alpha into the coverage so a translucent ink
            // color stays translucent.
            out.push(((u32::from(c) * u32::from(color.a)) / 255) as u8);
        }
        out
    }
}

/// An empty font-fallback list: keeps the engine deterministic by refusing to
/// reach for any system font when a glyph is missing from the bundled face.
#[derive(Debug, Default)]
struct NoFallback;

impl cosmic_text::Fallback for NoFallback {
    fn common_fallback(&self) -> &[&'static str] {
        &[]
    }

    fn forbidden_fallback(&self) -> &[&'static str] {
        &[]
    }

    fn script_fallback(&self, _script: unicode_script::Script, _locale: &str) -> &[&'static str] {
        &[]
    }
}

/// A real text engine: shapes and rasterizes antialiased glyphs from the bundled
/// font.
///
/// Holds a [`FontSystem`] (font database + shaping caches) and a [`SwashCache`]
/// (rasterized-glyph cache), so repeated `measure`/`rasterize` calls reuse work.
pub struct TextEngine {
    font_system: FontSystem,
    swash: SwashCache,
}

impl TextEngine {
    /// Build a text engine over the bundled font only.
    ///
    /// Loads [`FONT_BYTES`] into a fresh font database, points every generic
    /// family (monospace/sans/serif) at the bundled face, and installs an empty
    /// platform fallback — so results never depend on the host's installed fonts.
    #[must_use]
    pub fn new() -> Self {
        let mut db = cosmic_text::fontdb::Database::new();
        db.load_font_data(FONT_BYTES.to_vec());
        // Point the generic families at the one face we shipped, so even a request
        // for "sans-serif" resolves deterministically to the bundled font.
        db.set_monospace_family(FONT_FAMILY);
        db.set_sans_serif_family(FONT_FAMILY);
        db.set_serif_family(FONT_FAMILY);

        let font_system =
            FontSystem::new_with_locale_and_db_and_fallback("en-US".to_string(), db, NoFallback);

        Self {
            font_system,
            swash: SwashCache::new(),
        }
    }

    /// Lay `text` out at `px` pixels and return the tight pixel size of the run.
    ///
    /// Width is the laid-out advance width (rounded up); height is the line
    /// height for `px`. Both are reported as integer pixels in a [`Size`].
    /// An empty string measures to a zero-width run of one line.
    #[must_use]
    pub fn measure(&mut self, text: &str, px: f32) -> Size {
        let buffer = self.layout(text, px);
        // Widest laid-out advance over all runs (single line here, but be safe).
        let mut width: f32 = 0.0;
        let mut line_count: u32 = 0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            line_count += 1;
        }
        let line_height = px * LINE_HEIGHT_FACTOR;
        let height = line_height * line_count.max(1) as f32;
        Size {
            w: width.ceil(),
            h: height.ceil(),
        }
    }

    /// Shape and **rasterize** `text` at `px` pixels into an antialiased
    /// coverage mask.
    ///
    /// The returned [`Glyphs`] holds a row-major 8-bit alpha-coverage bitmap whose
    /// `width`/`height` are the **tight ink bounding box** of the run (leading and
    /// top blank space is trimmed), not the full layout box — use [`measure`] for
    /// the full advance/line box when reserving space. `color` is accepted for API
    /// symmetry; the coverage mask itself is color-independent, so the caller may
    /// blit it with any ink (see [`Glyphs::to_rgba`]). Pixels outside any glyph
    /// stay `0`. An all-whitespace or empty run yields a sized, all-zero mask.
    ///
    /// [`measure`]: Self::measure
    #[must_use]
    pub fn rasterize(&mut self, text: &str, px: f32, color: Color) -> Glyphs {
        let _ = color; // coverage is color-independent; kept for API symmetry.
        let buffer = self.layout(text, px);

        // First pass: collect placed glyph coverage so we can find the run bounds.
        // A glyph's swash image is positioned relative to the glyph pen origin via
        // its `placement`, which `with_pixels` already folds into the (x, y) it
        // hands us (those are offsets from the physical glyph origin).
        struct Px {
            x: i32,
            y: i32,
            a: u8,
        }
        let mut pixels: Vec<Px> = Vec::new();
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;

        // The baseline for the (single) line, in run-local pixel space.
        let line_height = px * LINE_HEIGHT_FACTOR;

        let runs: Vec<_> = buffer
            .layout_runs()
            .map(|run| {
                let glyphs: Vec<_> = run
                    .glyphs
                    .iter()
                    .map(|g| g.physical((0.0, run.line_y), 1.0))
                    .collect();
                glyphs
            })
            .collect();

        for run_glyphs in &runs {
            for pg in run_glyphs {
                let base = CtColor::rgba(255, 255, 255, 255);
                self.swash
                    .with_pixels(&mut self.font_system, pg.cache_key, base, |ox, oy, c| {
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

        // No ink at all (e.g. all-whitespace or empty): report a sized-but-empty
        // mask matching the laid-out advance so callers still reserve space.
        if pixels.is_empty() {
            let mut width: f32 = 0.0;
            for run in buffer.layout_runs() {
                width = width.max(run.line_w);
            }
            let w = width.ceil() as u32;
            let h = line_height.ceil().max(1.0) as u32;
            return Glyphs {
                width: w,
                height: h,
                coverage: vec![0u8; (w as usize) * (h as usize)],
            };
        }

        let width = (max_x - min_x + 1) as u32;
        let height = (max_y - min_y + 1) as u32;
        let mut coverage = vec![0u8; (width as usize) * (height as usize)];
        for p in &pixels {
            let lx = (p.x - min_x) as u32;
            let ly = (p.y - min_y) as u32;
            let idx = (ly * width + lx) as usize;
            // Multiple glyphs never overlap here, but `max` is safe if they do.
            let slot = &mut coverage[idx];
            *slot = (*slot).max(p.a);
        }

        Glyphs {
            width,
            height,
            coverage,
        }
    }

    /// Build a shaped [`Buffer`] for `text` at `px` pixels using the bundled font.
    fn layout(&mut self, text: &str, px: f32) -> Buffer {
        let metrics = Metrics::new(px, px * LINE_HEIGHT_FACTOR);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        // No wrap width: a single, un-wrapped run.
        buffer.set_size(&mut self.font_system, None, None);
        let attrs = Attrs::new()
            .family(Family::Name(FONT_FAMILY))
            .cache_key_flags(CacheKeyFlags::empty());
        buffer.set_text(&mut self.font_system, text, &attrs, Shaping::Advanced);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer
    }
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Line height as a multiple of font size. 1.2 is the conventional default and
/// keeps measured run heights stable regardless of the glyphs present.
const LINE_HEIGHT_FACTOR: f32 = 1.2;

#[cfg(test)]
mod tests {
    use super::*;

    fn white() -> Color {
        Color {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        }
    }

    #[test]
    fn measure_hi_is_plausible() {
        let mut eng = TextEngine::new();
        let size = eng.measure("Hi", 16.0);
        println!("measure(\"Hi\", 16.0) = {size:?}");
        assert!(size.w > 0.0, "width must be positive, got {}", size.w);
        assert!(size.h > 0.0, "height must be positive, got {}", size.h);
        // Two monospace cells at 16px should be clearly wider than one glyph and
        // not absurdly wide.
        assert!(
            size.w >= 8.0 && size.w <= 64.0,
            "width {} out of plausible range",
            size.w
        );
        assert!(
            size.h >= 8.0 && size.h <= 48.0,
            "height {} out of plausible range",
            size.h
        );
    }

    #[test]
    fn rasterize_hi_has_ink_and_background() {
        let mut eng = TextEngine::new();
        let g = eng.rasterize("Hi", 16.0, white());
        println!(
            "rasterize(\"Hi\", 16.0, white) -> {}x{} ({} bytes), ink_pixels = {}",
            g.width,
            g.height,
            g.coverage.len(),
            g.ink_pixels()
        );
        assert!(g.width > 0 && g.height > 0, "bitmap must have area");
        assert_eq!(
            g.coverage.len(),
            (g.width as usize) * (g.height as usize),
            "coverage must be width*height bytes"
        );
        let ink = g.ink_pixels();
        let total = g.coverage.len();
        assert!(ink > 0, "expected some ink pixels, got 0");
        assert!(
            ink < total,
            "expected some background pixels: ink {ink} of {total}"
        );

        // Antialiasing: a real rasterizer produces partial-coverage pixels, not
        // just 0/255. Confirm at least one mid-gray pixel exists.
        let partial = g.coverage.iter().filter(|&&c| c > 0 && c < 255).count();
        println!("partial-coverage (antialiased) pixels = {partial}");
        assert!(
            partial > 0,
            "expected antialiased (partial) coverage pixels"
        );

        // Render a tiny ASCII preview so the output is inspectable.
        print_preview(&g);
    }

    #[test]
    fn rasterize_to_rgba_folds_coverage_into_alpha() {
        let mut eng = TextEngine::new();
        let g = eng.rasterize("Hi", 16.0, white());
        let rgba = g.to_rgba(white());
        assert_eq!(rgba.len(), g.coverage.len() * 4);
        // Some alpha bytes must be non-zero (ink) and some zero (background).
        let nonzero_alpha = rgba.chunks_exact(4).filter(|px| px[3] != 0).count();
        assert!(nonzero_alpha > 0);
        assert!(nonzero_alpha < g.coverage.len());
    }

    #[test]
    fn whitespace_measures_but_has_no_ink() {
        let mut eng = TextEngine::new();
        let size = eng.measure(" ", 16.0);
        assert!(size.w > 0.0, "a space should still advance");
        let g = eng.rasterize(" ", 16.0, white());
        // Space produces no ink but should still report a sized mask.
        assert_eq!(g.ink_pixels(), 0);
        assert!(g.width > 0 && g.height > 0);
    }

    #[test]
    fn empty_string_is_safe() {
        let mut eng = TextEngine::new();
        let size = eng.measure("", 16.0);
        assert!(size.h > 0.0);
        let g = eng.rasterize("", 16.0, white());
        assert_eq!(g.ink_pixels(), 0);
    }

    #[test]
    fn larger_size_yields_more_ink() {
        let mut eng = TextEngine::new();
        let small = eng.rasterize("A", 12.0, white()).ink_pixels();
        let large = eng.rasterize("A", 48.0, white()).ink_pixels();
        println!("ink('A',12)={small}  ink('A',48)={large}");
        assert!(large > small, "bigger font should have more ink");
    }

    /// Print a compact ASCII-art view of the coverage mask for visual inspection.
    fn print_preview(g: &Glyphs) {
        const RAMP: &[u8] = b" .:-=+*#%@";
        println!("--- coverage preview {}x{} ---", g.width, g.height);
        for y in 0..g.height {
            let mut line = String::new();
            for x in 0..g.width {
                let c = g.coverage[(y * g.width + x) as usize];
                let idx = (usize::from(c) * (RAMP.len() - 1)) / 255;
                line.push(RAMP[idx] as char);
            }
            println!("{line}");
        }
        println!("--- end preview ---");
    }
}
