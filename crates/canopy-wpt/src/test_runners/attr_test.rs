//! ATTR / `checkLayout` test runner — ported from Blitz's `attr_test.rs`.
//!
//! A WPT `checkLayout` page asserts layout by tagging elements with
//! `data-expected-width` / `data-expected-height` / `data-expected-padding-*` /
//! `data-expected-margin-*` / `data-offset-x` / `data-offset-y` and calling
//! `checkLayout('selector')` on load. The browser harness runs
//! `document.querySelectorAll(selector)` and then walks **each matched element and
//! its descendant subtree**, checking every element that carries a `data-expected-*`
//! / `data-offset-*` attribute.
//!
//! ## Selector matching (real, not "every element")
//!
//! We now parse the `selector` argument out of the `checkLayout(...)` call and run
//! a small CSS selector matcher over the parsed arena (`Document::element_infos`
//! gives us each element's tag / id / classes / parent as plain strings). The
//! matcher supports a **selector list** (`a, b`), the **descendant** (` `) and
//! **child** (`>`) combinators, and per-compound `tag`, `.class`, and `#id` simple
//! selectors (`*` is the universal tag). The set of elements we check is the union
//! of every matched element's subtree — exactly the set the browser `checkLayout`
//! ends up asserting against. (Previously we checked EVERY element carrying a
//! `data-expected-*` attr and ignored the selector; that over-checks, e.g. it would
//! assert against elements a `#target > div` selector deliberately scopes out.)
//!
//! ## offsetParent-relative offsets
//!
//! `data-offset-x` / `data-offset-y` are the element's `offsetLeft` / `offsetTop`,
//! i.e. its border-box origin **relative to its CSS `offsetParent`** — the nearest
//! ancestor whose computed `position` is not `static`, else the `<body>`. We resolve
//! that true offset parent (via the `is_positioned` flag on each element's layout
//! detail) rather than the element's direct parent (Blitz's old simplification,
//! which is only correct when the parent happens to be the offset parent).
//!
//! ## What we can and cannot check
//!
//! Our engine exposes, per element, the absolute border-box `Rect` (origin + size)
//! AND — via [`StyloEngine::element_layout_detail`] — the padding / border / margin
//! [`Edges`](canopy_style_stylo::Edges) Taffy resolved. So:
//!   * `data-expected-width` / `data-expected-height` -> box size. CHECKED.
//!   * `data-offset-x` / `data-offset-y` -> border-box origin relative to the true
//!     CSS offset parent. CHECKED.
//!   * `data-expected-padding-{top,right,bottom,left}` -> the element's resolved
//!     padding edge. CHECKED (±1px).
//!   * `data-expected-margin-{top,right,bottom,left}` -> the element's resolved
//!     margin edge. CHECKED (±1px).
//!   * client-* / scroll-* / bounding-* / display -> NOT recoverable from the
//!     box-model breakdown we expose. We treat the presence of such an attribute as
//!     an UNSUPPORTED assertion and FAIL the element (honest: we genuinely can't
//!     verify it), matching Blitz, which errors on those variants.

use std::collections::{HashMap, HashSet};

use canopy_style_stylo::html::parse_html_with_css;
use canopy_style_stylo::{Edges, ElementInfo, StyloEngine};
use canopy_traits::{Rect, Size};

use crate::{HEIGHT, WIDTH};

/// Result of an ATTR test.
pub enum AttrOutcome {
    /// Every assertion held within tolerance.
    Pass,
    /// At least one assertion failed; message lists the first few mismatches.
    Fail(String),
    /// No element carried a checkable `data-expected-*` / `data-offset-*` attr
    /// within the selector-matched subtrees.
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

    // The element identity (tag/id/classes/parent) for the selector matcher, plus
    // a quick parent lookup. Read straight off the arena BEFORE it moves into the
    // engine (the cascade doesn't change identity).
    let infos = doc.element_infos();
    let info_by_slab: HashMap<usize, ElementInfo> =
        infos.iter().map(|e| (e.slab, e.clone())).collect();
    let parent_of: HashMap<usize, Option<usize>> =
        infos.iter().map(|e| (e.slab, e.parent)).collect();

    // Parse `checkLayout('selector')`. If we can't find/parse a selector, fall back
    // to "check every element with expectations" (the old, over-broad behavior) so
    // a malformed harness call never silently drops the whole test.
    let selector = extract_check_layout_selector(html);
    let scoped: Option<HashSet<usize>> = selector
        .as_deref()
        .and_then(parse_selector_list)
        .map(|sels| matched_subtree_slabs(&sels, &infos));

    let mut engine = StyloEngine::with_document(doc, &css);
    // `element_layout_detail` gives us, per element slab, the absolute border-box
    // rect, the padding/border/margin edge widths Taffy resolved, AND whether the
    // element is positioned (for true offsetParent resolution).
    let details = engine.element_layout_detail(Size {
        w: WIDTH,
        h: HEIGHT,
    });
    let boxes: HashMap<usize, Rect> = details.iter().map(|d| (d.slab, d.rect)).collect();
    let edges: HashMap<usize, (Edges, Edges)> = details
        .iter()
        .map(|d| (d.slab, (d.padding, d.margin)))
        .collect();
    // Border edges per element — needed to measure `offsetLeft/offsetTop` from the
    // offset parent's PADDING box (its border-box origin + its top/left border).
    let borders: HashMap<usize, Edges> = details.iter().map(|d| (d.slab, d.border)).collect();
    let positioned: HashSet<usize> = details
        .iter()
        .filter(|d| d.is_positioned)
        .map(|d| d.slab)
        .collect();

    // The `<body>` element's slab id — the default offset parent (`document.body`).
    let body_slab: Option<usize> = infos.iter().find(|e| e.tag == "body").map(|e| e.slab);

    let mut errors: Vec<String> = Vec::new();
    let mut checked_any = false;

    for (slab, attrs) in &data_attrs {
        // Honor the selector scope: only elements inside a matched subtree count.
        if let Some(scoped) = &scoped {
            if !scoped.contains(slab) {
                continue;
            }
        }

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

        // The true CSS offset parent's border-box origin, for offset checks: the
        // nearest *positioned* ancestor, else the body. Falls back to (0,0) if none
        // (e.g. the body itself, or an unrooted fragment).
        let offset_origin = offset_parent_origin(
            *slab,
            &parent_of,
            &info_by_slab,
            &positioned,
            body_slab,
            &boxes,
            &borders,
        );

        // Resolved padding / margin edges for this element (default zero if, e.g.,
        // it didn't lay out — that path already pushed a "no box" error above).
        let (padding, margin) = edges.get(slab).copied().unwrap_or_default();

        for (name, value) in attrs {
            let check = check_attr(name, value, rect, offset_origin, &padding, &margin);
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

/// Resolve the origin against which an element's `offsetLeft`/`offsetTop` (the
/// `data-offset-x/y` assertions) are measured: the **padding-box origin of its CSS
/// offsetParent** — the nearest ancestor whose computed `position` is not `static`,
/// else the `<body>`. Returns that `(x, y)`, or `(0, 0)` if there is no offset
/// parent.
///
/// `offsetTop`/`offsetLeft` are measured from the offset parent's *padding* edge
/// (HTMLElement.offsetTop spec), so we add the offset parent's top/left **border**
/// width to its border-box origin. The previous implementation used the DIRECT
/// parent's border-box origin, which is only correct when the parent IS the offset
/// parent and has no border — failing, e.g., `position:relative` containers with a
/// border (a 2px border shifted every nested offset by 2px).
fn offset_parent_origin(
    slab: usize,
    parent_of: &HashMap<usize, Option<usize>>,
    info_by_slab: &HashMap<usize, ElementInfo>,
    positioned: &HashSet<usize>,
    body_slab: Option<usize>,
    boxes: &HashMap<usize, Rect>,
    borders: &HashMap<usize, Edges>,
) -> (f32, f32) {
    // The padding-box origin of an offset-parent candidate: its border-box origin
    // plus its top/left border widths.
    let padding_origin = |pid: usize| -> (f32, f32) {
        let r = boxes.get(&pid);
        let b = borders.get(&pid).copied().unwrap_or_default();
        match r {
            Some(r) => (r.origin.x + b.left, r.origin.y + b.top),
            None => (0.0, 0.0),
        }
    };

    // Climb ancestors looking for the nearest positioned one.
    let mut cur = parent_of.get(&slab).copied().flatten();
    while let Some(pid) = cur {
        if positioned.contains(&pid) {
            return padding_origin(pid);
        }
        // The body is the default offset parent even when not positioned: if we
        // reach it without finding a positioned ancestor, stop here.
        if Some(pid) == body_slab {
            break;
        }
        cur = parent_of.get(&pid).copied().flatten();
    }
    // No positioned ancestor: the offset parent is the body (if present).
    // `info_by_slab` is unused for the climb but kept in the signature to document
    // that identity data was available to it.
    let _ = info_by_slab;
    body_slab.map(padding_origin).unwrap_or((0.0, 0.0))
}

/// Pull the selector string out of the FIRST `checkLayout('...')` /
/// `checkLayout("...")` call in the source. Returns `None` if absent/unparseable.
fn extract_check_layout_selector(html: &str) -> Option<String> {
    let start = html.find("checkLayout(")? + "checkLayout(".len();
    let rest = &html[start..];
    // Skip whitespace to the opening quote.
    let rest = rest.trim_start();
    let mut chars = rest.char_indices();
    let (_, quote) = chars.next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    // Take everything up to the matching closing quote.
    let body = &rest[quote.len_utf8()..];
    let end = body.find(quote)?;
    Some(body[..end].to_string())
}

/// A single simple-selector compound: optional tag, optional id, zero+ classes.
/// `*` (or no tag) means "any tag".
#[derive(Debug, Default, Clone)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

impl Compound {
    /// Does this compound match `el`?
    fn matches(&self, el: &ElementInfo) -> bool {
        if let Some(tag) = &self.tag {
            if !el.tag.eq_ignore_ascii_case(tag) {
                return false;
            }
        }
        if let Some(id) = &self.id {
            if el.id.as_deref() != Some(id.as_str()) {
                return false;
            }
        }
        self.classes
            .iter()
            .all(|c| el.classes.iter().any(|ec| ec == c))
    }
}

/// One combinator between two compounds in a complex selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Combinator {
    /// Descendant (whitespace).
    Descendant,
    /// Child (`>`).
    Child,
}

/// A complex selector: a sequence of `(combinator, compound)` steps. The first
/// step's combinator is ignored (it anchors the leftmost compound).
#[derive(Debug, Clone)]
struct Complex {
    steps: Vec<(Combinator, Compound)>,
}

/// Parse a comma-separated selector LIST into complex selectors. Returns `None`
/// if the whole list is empty/unparseable; individual unparseable compounds make
/// the containing complex selector match nothing (an empty step list), which is
/// the safe conservative behavior.
fn parse_selector_list(input: &str) -> Option<Vec<Complex>> {
    let list: Vec<Complex> = input
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            parse_complex(part)
        })
        .collect();
    if list.is_empty() {
        None
    } else {
        Some(list)
    }
}

/// Parse one complex selector (compounds joined by descendant/child combinators).
fn parse_complex(input: &str) -> Option<Complex> {
    let mut steps: Vec<(Combinator, Compound)> = Vec::new();
    // Tokenize on whitespace, treating a bare `>` token (or `>`-glued forms) as a
    // child combinator. This handles `a > b`, `a>b`, and `a > .b`.
    let normalized = input.replace('>', " > ");
    let mut pending = Combinator::Descendant; // first step's combinator is ignored
    for tok in normalized.split_whitespace() {
        if tok == ">" {
            pending = Combinator::Child;
            continue;
        }
        let compound = parse_compound(tok)?;
        steps.push((pending, compound));
        pending = Combinator::Descendant;
    }
    if steps.is_empty() {
        None
    } else {
        Some(Complex { steps })
    }
}

/// Parse a single compound selector token (e.g. `div.foo#bar`, `.cls`, `#id`, `*`).
fn parse_compound(tok: &str) -> Option<Compound> {
    let mut c = Compound::default();
    let mut chars = tok.chars().peekable();
    // Leading type/universal selector (anything before the first . or #). Only a
    // valid identifier (or `*`) is accepted; anything else (`[`, `:`, `(`, …) means
    // an unsupported simple selector, so we bail.
    let mut lead = String::new();
    while let Some(&ch) = chars.peek() {
        if ch == '.' || ch == '#' {
            break;
        }
        lead.push(ch);
        chars.next();
    }
    if lead == "*" {
        // universal: no tag constraint
    } else if !lead.is_empty() {
        if !is_ident(&lead) {
            return None;
        }
        c.tag = Some(lead);
    }
    // Then a run of `.class` / `#id` segments.
    while let Some(&sigil) = chars.peek() {
        if sigil != '.' && sigil != '#' {
            // Unsupported simple selector (attribute, pseudo, etc.) — bail.
            return None;
        }
        chars.next();
        let mut name = String::new();
        while let Some(&ch) = chars.peek() {
            if ch == '.' || ch == '#' {
                break;
            }
            name.push(ch);
            chars.next();
        }
        if name.is_empty() || !is_ident(&name) {
            return None;
        }
        match sigil {
            '.' => c.classes.push(name),
            '#' => {
                if c.id.is_some() {
                    return None;
                }
                c.id = Some(name);
            }
            _ => unreachable!(),
        }
    }
    Some(c)
}

/// A CSS identifier we support: ASCII alphanumerics plus `-` and `_`. Anything
/// else (`[`, `:`, `(`, etc.) signals an unsupported simple selector.
fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Run the selector list over the arena and return the UNION of every matched
/// element's subtree (the element and all its descendants), as a slab-id set —
/// exactly the elements `checkLayout` walks.
fn matched_subtree_slabs(sels: &[Complex], infos: &[ElementInfo]) -> HashSet<usize> {
    // children index for subtree expansion.
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for e in infos {
        if let Some(p) = e.parent {
            children.entry(p).or_default().push(e.slab);
        }
    }
    let info_by_slab: HashMap<usize, &ElementInfo> = infos.iter().map(|e| (e.slab, e)).collect();

    let mut matched: Vec<usize> = Vec::new();
    for el in infos {
        if sels
            .iter()
            .any(|sel| complex_matches(sel, el, &info_by_slab))
        {
            matched.push(el.slab);
        }
    }

    // Expand each matched element to its whole subtree.
    let mut out: HashSet<usize> = HashSet::new();
    let mut stack = matched;
    while let Some(slab) = stack.pop() {
        if out.insert(slab) {
            if let Some(kids) = children.get(&slab) {
                stack.extend(kids.iter().copied());
            }
        }
    }
    out
}

/// Does the complex selector match `el` (with `el` as the rightmost compound)?
/// Walks each preceding compound up the ancestor chain, honoring child vs.
/// descendant combinators.
///
/// Each `steps[i]` stores the combinator that joins it to the step on its LEFT
/// (`steps[i-1]`). So when we match `steps[i-1]` against `current`'s ancestors, the
/// combinator we honor is the one recorded on `steps[i]` (the step to its right).
fn complex_matches(
    sel: &Complex,
    el: &ElementInfo,
    info_by_slab: &HashMap<usize, &ElementInfo>,
) -> bool {
    let n = sel.steps.len();
    if n == 0 {
        return false;
    }
    // The rightmost compound must match `el`.
    if !sel.steps[n - 1].1.matches(el) {
        return false;
    }
    // Walk remaining compounds right-to-left against ancestors. For `steps[i]`, the
    // combinator joining it to `steps[i+1]` is recorded on `steps[i+1]`.
    let mut current = el;
    for i in (0..n - 1).rev() {
        let compound = &sel.steps[i].1;
        let combinator = sel.steps[i + 1].0;
        let matched = match combinator {
            Combinator::Child => current
                .parent
                .and_then(|p| info_by_slab.get(&p).copied())
                .filter(|parent| compound.matches(parent)),
            Combinator::Descendant => {
                let mut anc = current.parent.and_then(|p| info_by_slab.get(&p).copied());
                let mut found = None;
                while let Some(a) = anc {
                    if compound.matches(a) {
                        found = Some(a);
                        break;
                    }
                    anc = a.parent.and_then(|p| info_by_slab.get(&p).copied());
                }
                found
            }
        };
        match matched {
            Some(m) => current = m,
            None => return false,
        }
    }
    true
}

/// Check one `data-*` attribute against the element's box. Returns:
///   * `None` — not a check attribute (ignore).
///   * `Some(Ok)` — supported and within tolerance.
///   * `Some(Err)` — supported but mismatched, OR unsupported (we can't verify).
fn check_attr(
    name: &str,
    value: &str,
    rect: &Rect,
    offset_origin: (f32, f32),
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
            let actual = rect.origin.x - offset_origin.0;
            if within_tolerance(exp, actual) {
                Ok(())
            } else {
                Err(format!("data-offset-x: expected {exp} got {actual}"))
            }
        })),
        "data-offset-y" => Some(parse(value).and_then(|exp| {
            let actual = rect.origin.y - offset_origin.1;
            if within_tolerance(exp, actual) {
                Ok(())
            } else {
                Err(format!("data-offset-y: expected {exp} got {actual}"))
            }
        })),

        // Padding / margin edges — CHECKED against the resolved box-model
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

    /// `data-expected-padding-*` is CHECKED: a `padding:10px` box whose
    /// expectations name 10px on every side PASSES.
    #[test]
    fn padding_edges_checked_and_pass() {
        let html = "<style>#t{width:100px;height:50px;padding:10px}</style>\
            <body onload=\"checkLayout('#t')\"><div id=\"t\" \
            data-expected-padding-top=\"10\" data-expected-padding-right=\"10\" \
            data-expected-padding-bottom=\"10\" data-expected-padding-left=\"10\"></div></body>";
        match run_attr_test(html) {
            AttrOutcome::Pass => {}
            AttrOutcome::Fail(msg) => panic!("expected pass, got fail: {msg}"),
            AttrOutcome::NoTarget => panic!("expected a checked target, got NoTarget"),
        }
    }

    /// A WRONG padding expectation FAILS with a real mismatch message.
    #[test]
    fn padding_edge_mismatch_fails() {
        let html = "<style>#t{width:100px;height:50px;padding:10px}</style>\
            <body onload=\"checkLayout('#t')\"><div id=\"t\" data-expected-padding-top=\"25\"></div></body>";
        match run_attr_test(html) {
            AttrOutcome::Fail(msg) => assert!(
                msg.contains("data-expected-padding-top"),
                "fail message should name the padding edge, got: {msg}"
            ),
            other => panic!("expected a Fail, got {:?}", outcome_label(&other)),
        }
    }

    /// `data-expected-margin-*` is CHECKED: a `margin:7px` box PASSES.
    #[test]
    fn margin_edges_checked_and_pass() {
        let html = "<style>#t{width:80px;height:30px;margin:7px}</style>\
            <body onload=\"checkLayout('#t')\"><div id=\"t\" \
            data-expected-margin-top=\"7\" data-expected-margin-right=\"7\" \
            data-expected-margin-bottom=\"7\" data-expected-margin-left=\"7\"></div></body>";
        match run_attr_test(html) {
            AttrOutcome::Pass => {}
            AttrOutcome::Fail(msg) => panic!("expected pass, got fail: {msg}"),
            AttrOutcome::NoTarget => panic!("expected a checked target, got NoTarget"),
        }
    }

    /// The selector SCOPES which elements get checked. A `#in` target carries a
    /// CORRECT expectation; a `#out` sibling (outside the selector) carries a WRONG
    /// one. Because `checkLayout('#in')` only walks the `#in` subtree, the wrong
    /// `#out` expectation is NEVER checked, so the test PASSES.
    #[test]
    fn selector_scopes_checked_elements() {
        let html = "<style>div{width:50px;height:20px}</style>\
            <body onload=\"checkLayout('#in')\">\
            <div id=\"in\" data-expected-width=\"50\"></div>\
            <div id=\"out\" data-expected-width=\"9999\"></div>\
            </body>";
        match run_attr_test(html) {
            AttrOutcome::Pass => {}
            AttrOutcome::Fail(msg) => panic!(
                "expected pass: #out is out of selector scope and must not be checked, got: {msg}"
            ),
            AttrOutcome::NoTarget => panic!("expected #in to be checked, got NoTarget"),
        }
    }

    /// The selector also INCLUDES descendants of the matched element. A matched
    /// `.box` ancestor with a wrong-expectation CHILD must FAIL (the child is in the
    /// matched subtree even though it doesn't match the selector itself).
    #[test]
    fn selector_includes_descendants() {
        let html = "<style>.box{width:50px;height:50px}.child{width:30px;height:30px}</style>\
            <body onload=\"checkLayout('.box')\">\
            <div class=\"box\"><div class=\"child\" data-expected-width=\"9999\"></div></div>\
            </body>";
        match run_attr_test(html) {
            AttrOutcome::Fail(msg) => assert!(
                msg.contains("data-expected-width"),
                "the descendant's wrong width should be checked & fail, got: {msg}"
            ),
            other => panic!(
                "expected a Fail (descendant checked), got {:?}",
                outcome_label(&other)
            ),
        }
    }

    /// The CHILD combinator is honored: `#p > .a` matches a direct child `.a` but
    /// NOT a grandchild `.a`. The direct child carries a correct expectation; the
    /// grandchild a wrong one that must NOT be checked -> PASS.
    #[test]
    fn child_combinator_excludes_grandchild() {
        let html = "<style>div{width:40px;height:20px}</style>\
            <body onload=\"checkLayout('#p > .a')\">\
            <div id=\"p\">\
              <div class=\"a\" data-expected-width=\"40\"></div>\
              <div class=\"mid\"><div class=\"a\" data-expected-width=\"9999\"></div></div>\
            </div></body>";
        match run_attr_test(html) {
            AttrOutcome::Pass => {}
            AttrOutcome::Fail(msg) => panic!(
                "grandchild .a is not a direct child of #p and must be out of scope, got: {msg}"
            ),
            AttrOutcome::NoTarget => panic!("expected the direct child to be checked"),
        }
    }

    /// A selector LIST (`a, b`) matches the union. Both targets carry correct
    /// expectations and both must be checked -> PASS, and removing one from the
    /// list would drop its check (covered implicitly by the scope test above).
    #[test]
    fn selector_list_matches_union() {
        let html = "<style>div{width:25px;height:25px}</style>\
            <body onload=\"checkLayout('.x, .y')\">\
            <div class=\"x\" data-expected-width=\"25\"></div>\
            <div class=\"y\" data-expected-height=\"25\"></div>\
            </body>";
        match run_attr_test(html) {
            AttrOutcome::Pass => {}
            AttrOutcome::Fail(msg) => panic!("both list targets should pass, got: {msg}"),
            AttrOutcome::NoTarget => panic!("expected both targets checked"),
        }
    }

    /// True offsetParent: a `position:relative` container holds the target through
    /// a STATIC wrapper, so the target's direct parent (the wrapper) is NOT its
    /// offsetParent — the positioned `#cont` two levels up is. The target sits 30px
    /// down / 20px right INSIDE `#cont` (via the container's padding, which doesn't
    /// margin-collapse), so its `offsetTop/offsetLeft` are (20, 30) measured from
    /// `#cont`. The old direct-parent logic would subtract the wrapper origin (which
    /// equals `#cont`'s here) — so to make the two genuinely differ we also push the
    /// wrapper down inside `#cont`, and the target down inside the wrapper, with
    /// PADDING (non-collapsing). Net: target is at cont-origin + (20, 30).
    #[test]
    fn offset_relative_to_positioned_ancestor() {
        let html = "<style>\
            #cont{position:relative;padding-left:20px;padding-top:30px;width:200px;height:200px}\
            #wrap{width:160px;height:160px}\
            #target{width:30px;height:30px}\
            </style>\
            <body onload=\"checkLayout('#target')\">\
            <div id=\"cont\"><div id=\"wrap\">\
            <div id=\"target\" data-offset-x=\"20\" data-offset-y=\"30\"></div>\
            </div></div></body>";
        match run_attr_test(html) {
            AttrOutcome::Pass => {}
            AttrOutcome::Fail(msg) => panic!(
                "offset must be measured from the positioned #cont (offsetParent), got: {msg}"
            ),
            AttrOutcome::NoTarget => panic!("expected #target checked"),
        }
    }

    /// Sanity check the offset-parent *climb itself* differs from the direct parent.
    /// The target's direct parent is the static `#wrap`; its offsetParent is `#cont`.
    /// We assert the offset is measured from `#cont` (so NOT 0,0, which is what a
    /// direct-parent subtraction would give since target and wrap share an origin
    /// only if no padding intervened — here padding makes them differ).
    #[test]
    fn offset_parent_is_not_direct_parent() {
        // #cont positioned with padding so #wrap is inset; #target flush in #wrap.
        // direct-parent (#wrap) subtraction => (0,0); offsetParent (#cont) => (15,25).
        let html = "<style>\
            #cont{position:relative;padding-left:15px;padding-top:25px;width:200px;height:200px}\
            #wrap{width:160px;height:160px}\
            #target{width:30px;height:30px}\
            </style>\
            <body onload=\"checkLayout('#target')\">\
            <div id=\"cont\"><div id=\"wrap\">\
            <div id=\"target\" data-offset-x=\"15\" data-offset-y=\"25\"></div>\
            </div></div></body>";
        match run_attr_test(html) {
            AttrOutcome::Pass => {}
            AttrOutcome::Fail(msg) => {
                panic!("offset should be from #cont (15,25), not the direct parent: {msg}")
            }
            AttrOutcome::NoTarget => panic!("expected #target checked"),
        }
    }

    fn outcome_label(o: &AttrOutcome) -> &'static str {
        match o {
            AttrOutcome::Pass => "Pass",
            AttrOutcome::Fail(_) => "Fail",
            AttrOutcome::NoTarget => "NoTarget",
        }
    }

    // ---- selector-parser unit tests (no layout) ----

    #[test]
    fn parse_compound_forms() {
        let c = parse_compound("div.foo#bar").unwrap();
        assert_eq!(c.tag.as_deref(), Some("div"));
        assert_eq!(c.id.as_deref(), Some("bar"));
        assert_eq!(c.classes, vec!["foo"]);

        let c = parse_compound(".only").unwrap();
        assert!(c.tag.is_none() && c.id.is_none());
        assert_eq!(c.classes, vec!["only"]);

        let c = parse_compound("*").unwrap();
        assert!(c.tag.is_none());

        // Unsupported simple selectors bail out.
        assert!(parse_compound("a[href]").is_none());
        assert!(parse_compound("p:hover").is_none());
    }

    #[test]
    fn parse_complex_combinators() {
        let cx = parse_complex("#a > .b c").unwrap();
        assert_eq!(cx.steps.len(), 3);
        // steps[1] (".b") joins to its LEFT step (#a) via Child.
        assert_eq!(cx.steps[1].0, Combinator::Child);
        // steps[2] ("c") joins to its left (".b") via Descendant.
        assert_eq!(cx.steps[2].0, Combinator::Descendant);
    }

    #[test]
    fn extract_selector_from_onload() {
        assert_eq!(
            extract_check_layout_selector("<body onload=\"checkLayout('.flex')\">"),
            Some(".flex".to_string())
        );
        assert_eq!(
            extract_check_layout_selector("checkLayout( \"#a > b\" )"),
            Some("#a > b".to_string())
        );
        assert_eq!(extract_check_layout_selector("no call here"), None);
    }
}
