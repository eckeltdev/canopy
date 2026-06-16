//! REF test runner — ported from Blitz's `ref_test.rs`.
//!
//! A reftest declares `<link rel="match" href="...-ref.html">`. We render BOTH
//! the test and its reference to RGBA at the same size with the SAME renderer,
//! then compare (Blitz's ladder):
//!   1. **reject blank** — if the test image is entirely zero/transparent, FAIL
//!      (a blank render can't legitimately match anything meaningful here).
//!   2. **exact equality** — byte-identical buffers PASS immediately.
//!   3. **perceptual diff** — else compare per pixel; if the fraction of pixels
//!      whose max channel difference exceeds a small threshold is over a
//!      tolerance, FAIL. (Blitz uses `dify` at 0.1; we use a self-contained
//!      max-channel-diff comparison to avoid the extra dependency, with an
//!      equivalent intent.)

use std::fs;
use std::path::Path;

use canopy_style_stylo::StyloEngine;
use canopy_traits::Size;

/// Result of a REF test.
pub enum RefOutcome {
    /// Test and reference matched (exactly or within perceptual tolerance).
    Pass,
    /// They differed beyond tolerance, or the test render was blank.
    Fail(String),
    /// Could not run (e.g. the referenced file was missing/unreadable).
    Skip(String),
}

/// Per-pixel max-channel difference above which two pixels are "different".
const CHANNEL_DIFF_THRESHOLD: u8 = 16;
/// Fraction of differing pixels above which the images are considered a mismatch.
/// 0.1% — generous enough to absorb antialiasing noise, strict enough to catch
/// real layout/paint divergence.
const DIFF_FRACTION_TOLERANCE: f64 = 0.001;

/// Run one reftest from the test's HTML source and absolute path.
pub fn run_ref_test(test_html: &str, test_path: &Path, width: f32, height: f32) -> RefOutcome {
    // Extract the first `rel="match"` href.
    let Some(href) = extract_match_href(test_html) else {
        return RefOutcome::Skip("no rel=match link found".to_string());
    };

    // Resolve the reference path relative to the test file's directory.
    let Some(ref_path) = resolve_ref_path(test_path, &href) else {
        return RefOutcome::Skip(format!("could not resolve ref path {href:?}"));
    };
    let ref_html = match fs::read_to_string(&ref_path) {
        Ok(s) => s,
        Err(e) => {
            return RefOutcome::Skip(format!("ref file {} unreadable: {e}", ref_path.display()))
        }
    };

    let viewport = Size {
        w: width,
        h: height,
    };

    // Render both to RGBA with the same engine/renderer.
    let (test_rgba, tw, th) = StyloEngine::from_html(test_html).render_to_rgba(viewport);
    let (ref_rgba, rw, rh) = StyloEngine::from_html(&ref_html).render_to_rgba(viewport);

    if (tw, th) != (rw, rh) {
        return RefOutcome::Fail(format!(
            "dimension mismatch: test {tw}x{th} vs ref {rw}x{rh}"
        ));
    }

    // 1. reject blank test render.
    if test_rgba.iter().all(|&b| b == 0) {
        return RefOutcome::Fail("test render is entirely blank".to_string());
    }

    // 2. exact-equality fast path.
    if test_rgba == ref_rgba {
        return RefOutcome::Pass;
    }

    // 3. perceptual diff: count pixels whose max channel diff exceeds threshold.
    let total_pixels = (tw * th).max(1);
    let mut diff_pixels = 0usize;
    for (tp, rp) in test_rgba.chunks_exact(4).zip(ref_rgba.chunks_exact(4)) {
        let max_diff = tp
            .iter()
            .zip(rp.iter())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        if max_diff > CHANNEL_DIFF_THRESHOLD {
            diff_pixels += 1;
        }
    }

    let fraction = diff_pixels as f64 / total_pixels as f64;
    if fraction <= DIFF_FRACTION_TOLERANCE {
        RefOutcome::Pass
    } else {
        RefOutcome::Fail(format!(
            "{diff_pixels}/{total_pixels} pixels differ ({:.3}% > {:.3}% tolerance)",
            fraction * 100.0,
            DIFF_FRACTION_TOLERANCE * 100.0
        ))
    }
}

/// Extract the `href` of the first `<link rel="match" ...>` (either attribute
/// order; single or double quotes). Lightweight string scan, no HTML parse.
fn extract_match_href(html: &str) -> Option<String> {
    let bytes = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find("rel=") {
        let rel_abs = search_from + rel;
        // Find the enclosing <link ...> tag bounds.
        let tag_start = lower[..rel_abs].rfind('<').unwrap_or(rel_abs);
        let tag_end = lower[rel_abs..]
            .find('>')
            .map(|e| rel_abs + e)
            .unwrap_or(bytes.len());
        let tag = &lower[tag_start..tag_end];
        let orig_tag = &html[tag_start..tag_end];
        if tag.contains("link") && tag_contains_rel_match(tag) {
            if let Some(href) = extract_attr(orig_tag, "href") {
                return Some(href);
            }
        }
        search_from = tag_end.max(rel_abs + 1);
    }
    None
}

/// True if a `<link>` tag's text has `rel` equal to (or containing) `match`.
fn tag_contains_rel_match(tag_lower: &str) -> bool {
    extract_attr(tag_lower, "rel")
        .map(|v| v.split_ascii_whitespace().any(|t| t == "match"))
        .unwrap_or(false)
}

/// Extract the value of `attr` from a tag string (quotes optional).
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let key = format!("{attr}=");
    let at = lower.find(&key)?;
    let rest = &tag[at + key.len()..];
    let rest = rest.trim_start();
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (quote, body) = match bytes[0] {
        b'"' => ('"', &rest[1..]),
        b'\'' => ('\'', &rest[1..]),
        _ => ('\0', rest),
    };
    let end = if quote == '\0' {
        body.find(|c: char| c.is_whitespace() || c == '>')
            .unwrap_or(body.len())
    } else {
        body.find(quote).unwrap_or(body.len())
    };
    Some(body[..end].to_string())
}

/// Resolve a reference href against the test file's directory. Handles relative
/// hrefs (the common case) and root-relative `/...` (resolved against the WPT
/// root, inferred by stripping `css/<suite>/...` from the test path is brittle,
/// so we only handle the relative case and bail on absolute hrefs).
fn resolve_ref_path(test_path: &Path, href: &str) -> Option<std::path::PathBuf> {
    if href.starts_with("http://") || href.starts_with("https://") {
        return None;
    }
    let dir = test_path.parent()?;
    if let Some(rooted) = href.strip_prefix('/') {
        // Root-relative: walk up from the test dir to a plausible WPT root (the
        // ancestor that contains the leading path segment of `rooted`).
        let first_seg = rooted.split('/').next().unwrap_or("");
        let mut anc = Some(dir);
        while let Some(d) = anc {
            if d.join(first_seg).exists() {
                return Some(d.join(rooted));
            }
            anc = d.parent();
        }
        return None;
    }
    Some(dir.join(href))
}
