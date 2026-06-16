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
//! Our engine exposes, per element, the absolute border-box `Rect` (origin + size)
//! AND — via [`StyloEngine::element_layout_detail`] — the padding / border / margin
//! [`Edges`](canopy_style_stylo::Edges) Taffy resolved. So:
//!   * `data-expected-width` / `data-expected-height` -> box size. CHECKED.
//!   * `data-offset-x` / `data-offset-y` -> box origin relative to the offset
//!     parent. We approximate with origin relative to the element's parent box
//!     (Blitz does the same simplification). CHECKED.
//!   * `data-expected-padding-{top,right,bottom,left}` -> the element's resolved
//!     padding edge. CHECKED (±1px).
//!   * `data-expected-margin-{top,right,bottom,left}` -> the element's resolved
//!     margin edge. CHECKED (±1px).
//!   * client-* / scroll-* / bounding-* / display -> NOT recoverable from the
//!     box-model breakdown we expose. We treat the presence of such an attribute as
//!     an UNSUPPORTED assertion and FAIL the element (honest: we genuinely can't
//!     verify it), matching Blitz, which errors on those variants.

use std::collections::HashMap;

use canopy_style_stylo::html::parse_html_with_css;
use canopy_style_stylo::{Edges, StyloEngine};
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
    // `element_layout_detail` gives us, per element slab, the absolute border-box
    // rect PLUS the padding/border/margin edge widths Taffy resolved — so we can
    // check `data-expected-padding-*` / `data-expected-margin-*`, not just size.
    let details = engine.element_layout_detail(Size {
        w: WIDTH,
        h: HEIGHT,
    });
    let boxes: HashMap<usize, Rect> = details.iter().map(|d| (d.slab, d.rect)).collect();
    let edges: HashMap<usize, (Edges, Edges)> = details
        .iter()
        .map(|d| (d.slab, (d.padding, d.margin)))
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

        // Resolved padding / margin edges for this element (default zero if, e.g.,
        // it didn't lay out — that path already pushed a "no box" error above).
        let (padding, margin) = edges.get(slab).copied().unwrap_or_default();

        for (name, value) in attrs {
            let check = check_attr(name, value, rect, parent_origin, &padding, &margin);
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
    padding: &Edges,
    margin: &Edges,
) -> Option<Result<(), String>> {
    // Parse the expected number; non-numeric expectations (rare) are unsupported.
    let parse = |v: &str| -> Result<f32, String> {
        v.trim()
            .parse::<f32>()
            .map_err(|_| format!("{name}: non-numeric expected value {v:?}"))
    };

    // Helper: compare an expected value against one resolved edge.
    let edge_check = |edge_name: &str, expected: &str, actual: f32| -> Result<(), String> {
        parse(expected).and_then(|exp| {
            if within_tolerance(exp, actual) {
                Ok(())
            } else {
                Err(format!("{edge_name}: expected {exp} got {actual}"))
            }
        })
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

        // Padding / margin edges — now CHECKED against the resolved box-model
        // breakdown from `element_layout_detail` (±1px).
        "data-expected-padding-top" => Some(edge_check(name, value, padding.top)),
        "data-expected-padding-right" => Some(edge_check(name, value, padding.right)),
        "data-expected-padding-bottom" => Some(edge_check(name, value, padding.bottom)),
        "data-expected-padding-left" => Some(edge_check(name, value, padding.left)),
        "data-expected-margin-top" => Some(edge_check(name, value, margin.top)),
        "data-expected-margin-right" => Some(edge_check(name, value, margin.right)),
        "data-expected-margin-bottom" => Some(edge_check(name, value, margin.bottom)),
        "data-expected-margin-left" => Some(edge_check(name, value, margin.left)),

        // Supported by the spec's checkLayout but NOT by the box-model breakdown we
        // expose. Be honest: we cannot verify these, so they count as failures
        // (matching Blitz, which returns an "Unsupported assertion" error).
        _ if name == "data-expected-client-width"
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `data-expected-padding-*` is now CHECKED: a `padding:10px` box whose
    /// expectations name 10px on every side PASSES (no longer "unsupported").
    #[test]
    fn padding_edges_checked_and_pass() {
        let html = "<style>#t{width:100px;height:50px;padding:10px}</style>\
            <body><div id=\"t\" \
            data-expected-padding-top=\"10\" data-expected-padding-right=\"10\" \
            data-expected-padding-bottom=\"10\" data-expected-padding-left=\"10\"></div></body>";
        match run_attr_test(html) {
            AttrOutcome::Pass => {}
            AttrOutcome::Fail(msg) => panic!("expected pass, got fail: {msg}"),
            AttrOutcome::NoTarget => panic!("expected a checked target, got NoTarget"),
        }
    }

    /// A WRONG padding expectation FAILS with a real mismatch message (proving the
    /// edge is actually compared, not silently accepted).
    #[test]
    fn padding_edge_mismatch_fails() {
        let html = "<style>#t{width:100px;height:50px;padding:10px}</style>\
            <body><div id=\"t\" data-expected-padding-top=\"25\"></div></body>";
        match run_attr_test(html) {
            AttrOutcome::Fail(msg) => assert!(
                msg.contains("data-expected-padding-top"),
                "fail message should name the padding edge, got: {msg}"
            ),
            other => panic!("expected a Fail, got {:?}", outcome_label(&other)),
        }
    }

    /// `data-expected-margin-*` is now CHECKED: a `margin:7px` box whose expectations
    /// name 7px on every side PASSES.
    #[test]
    fn margin_edges_checked_and_pass() {
        // A block sibling layout: the element keeps its own margin edges.
        let html = "<style>#t{width:80px;height:30px;margin:7px}</style>\
            <body><div id=\"t\" \
            data-expected-margin-top=\"7\" data-expected-margin-right=\"7\" \
            data-expected-margin-bottom=\"7\" data-expected-margin-left=\"7\"></div></body>";
        match run_attr_test(html) {
            AttrOutcome::Pass => {}
            AttrOutcome::Fail(msg) => panic!("expected pass, got fail: {msg}"),
            AttrOutcome::NoTarget => panic!("expected a checked target, got NoTarget"),
        }
    }

    fn outcome_label(o: &AttrOutcome) -> &'static str {
        match o {
            AttrOutcome::Pass => "Pass",
            AttrOutcome::Fail(_) => "Fail",
            AttrOutcome::NoTarget => "NoTarget",
        }
    }
}
