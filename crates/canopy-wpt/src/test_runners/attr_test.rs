//! ATTR / `checkLayout` test runner — ported from Blitz's `attr_test.rs`.
//!
//! A WPT `checkLayout` page asserts layout by tagging elements with
//! `data-expected-width` / `data-expected-height` / `data-expected-padding-*` /
//! `data-expected-margin-*` / `data-offset-x` / `data-offset-y` and calling
//! `checkLayout('selector')` on load. The harness walks the matched subtree and
//! checks every element carrying such attributes.
//!
//! We don't have a CSS selector engine on the arena, but we don't need one: we
//! retain the `data-*` attributes per element (slab id) at parse time, so we
//! simply check **every element that carries a `data-expected-*` / `data-offset-*`
//! attribute**. That is exactly the set `checkLayout` ends up asserting against
//! (the selector just scopes the subtree; in practice these pages put the
//! expectations only on the elements they mean to check). The test PASSES iff
//! every such assertion holds within ±1px (Blitz's `assert_with_tolerance`).
//!
//! ## What we can and cannot check
//!
//! Our engine exposes each element's ABSOLUTE border-box `Rect` (origin + size).
//! So:
//!   * `data-expected-width` / `data-expected-height` -> box size. CHECKED.
//!   * `data-offset-x` / `data-offset-y` -> box origin relative to the offset
//!     parent. We approximate with origin relative to the element's parent box
//!     (Blitz does the same simplification). CHECKED.
//!   * `data-expected-padding-*` / `data-expected-margin-*` / client-* / scroll-*
//!     -> NOT exposed by our `Rect` (border-box only). We treat the presence of
//!     such an attribute as an UNSUPPORTED assertion and FAIL the element (honest:
//!     we genuinely can't verify it), matching Blitz, which errors on the
//!     client/scroll/bounding variants.

use std::collections::HashMap;

use canopy_style_stylo::html::parse_html_with_css;
use canopy_style_stylo::StyloEngine;
use canopy_traits::{Rect, Size};

use crate::{HEIGHT, WIDTH};

/// Result of an ATTR test.
pub enum AttrOutcome {
    /// Every assertion held within tolerance.
    Pass,
    /// At least one assertion failed; message lists the first few mismatches.
    Fail(String),
    /// No element carried a checkable `data-expected-*` / `data-offset-*` attr.
    NoTarget,
}

/// ±1px tolerance, exactly as Blitz's `assert_with_tolerance`.
fn within_tolerance(expected: f32, actual: f32) -> bool {
    (actual - expected).abs() < 1.0
}

/// Run one `checkLayout` test from its raw HTML source.
pub fn run_attr_test(html: &str) -> AttrOutcome {
    let (doc, css, data_attrs) = parse_html_with_css(html);
    if data_attrs.is_empty() {
        return AttrOutcome::NoTarget;
    }

    // Record each element's parent slab id BEFORE we move `doc` into the engine,
    // so we can compute offsets relative to the parent box.
    let parent_of: HashMap<usize, Option<usize>> = doc
        .nodes
        .iter()
        .enumerate()
        .map(|(id, n)| (id, n.parent))
        .collect();

    let mut engine = StyloEngine::with_document(doc, &css);
    let boxes: HashMap<usize, Rect> = engine
        .element_layout(Size {
            w: WIDTH,
            h: HEIGHT,
        })
        .into_iter()
        .collect();

    let mut errors: Vec<String> = Vec::new();
    let mut checked_any = false;

    for (slab, attrs) in &data_attrs {
        // Only elements that actually have a checkable expectation count.
        let has_checkable = attrs.keys().any(|k| {
            k.starts_with("data-expected-")
                || k == "data-offset-x"
                || k == "data-offset-y"
                || k == "data-total-x"
                || k == "data-total-y"
        });
        if !has_checkable {
            continue;
        }

        let Some(rect) = boxes.get(slab) else {
            // Element had expectations but produced no box (e.g. display:none,
            // or text-only / non-laid-out). Count as a failed assertion.
            errors.push(format!(
                "element #{slab}: expected a box but none was laid out"
            ));
            checked_any = true;
            continue;
        };

        // Parent border-box origin, for offset checks.
        let parent_origin = parent_of
            .get(slab)
            .copied()
            .flatten()
            .and_then(|pid| boxes.get(&pid))
            .map(|p| (p.origin.x, p.origin.y))
            .unwrap_or((0.0, 0.0));

        for (name, value) in attrs {
            let check = check_attr(name, value, rect, parent_origin);
            match check {
                Some(Ok(())) => checked_any = true,
                Some(Err(msg)) => {
                    checked_any = true;
                    errors.push(msg);
                }
                None => { /* not a check attribute */ }
            }
        }
    }

    if !checked_any {
        return AttrOutcome::NoTarget;
    }

    if errors.is_empty() {
        AttrOutcome::Pass
    } else {
        // Cap the message length so the report stays readable.
        let shown: Vec<String> = errors.iter().take(4).cloned().collect();
        let more = errors.len().saturating_sub(shown.len());
        let mut msg = shown.join("; ");
        if more > 0 {
            msg.push_str(&format!("; (+{more} more)"));
        }
        AttrOutcome::Fail(msg)
    }
}

/// Check one `data-*` attribute against the element's box. Returns:
///   * `None` — not a check attribute (ignore).
///   * `Some(Ok)` — supported and within tolerance.
///   * `Some(Err)` — supported but mismatched, OR unsupported (we can't verify).
fn check_attr(
    name: &str,
    value: &str,
    rect: &Rect,
    parent_origin: (f32, f32),
) -> Option<Result<(), String>> {
    // Parse the expected number; non-numeric expectations (rare) are unsupported.
    let parse = |v: &str| -> Result<f32, String> {
        v.trim()
            .parse::<f32>()
            .map_err(|_| format!("{name}: non-numeric expected value {v:?}"))
    };

    match name {
        "data-expected-width" => Some(parse(value).and_then(|exp| {
            if within_tolerance(exp, rect.size.w) {
                Ok(())
            } else {
                Err(format!(
                    "data-expected-width: expected {exp} got {}",
                    rect.size.w
                ))
            }
        })),
        "data-expected-height" => Some(parse(value).and_then(|exp| {
            if within_tolerance(exp, rect.size.h) {
                Ok(())
            } else {
                Err(format!(
                    "data-expected-height: expected {exp} got {}",
                    rect.size.h
                ))
            }
        })),
        "data-offset-x" => Some(parse(value).and_then(|exp| {
            let actual = rect.origin.x - parent_origin.0;
            if within_tolerance(exp, actual) {
                Ok(())
            } else {
                Err(format!("data-offset-x: expected {exp} got {actual}"))
            }
        })),
        "data-offset-y" => Some(parse(value).and_then(|exp| {
            let actual = rect.origin.y - parent_origin.1;
            if within_tolerance(exp, actual) {
                Ok(())
            } else {
                Err(format!("data-offset-y: expected {exp} got {actual}"))
            }
        })),

        // Supported by the spec's checkLayout but NOT by our border-box-only
        // `Rect`. Be honest: we cannot verify these, so they count as failures
        // (matching Blitz, which returns an "Unsupported assertion" error).
        _ if name.starts_with("data-expected-padding-")
            || name.starts_with("data-expected-margin-")
            || name == "data-expected-client-width"
            || name == "data-expected-client-height"
            || name == "data-expected-scroll-width"
            || name == "data-expected-scroll-height"
            || name == "data-expected-bounding-client-rect-width"
            || name == "data-expected-bounding-client-rect-height"
            || name == "data-total-x"
            || name == "data-total-y"
            || name == "data-expected-display" =>
        {
            Some(Err(format!("unsupported assertion: {name}")))
        }

        // Any other data-* (e.g. data-test, data-description) is not a check.
        _ => None,
    }
}
