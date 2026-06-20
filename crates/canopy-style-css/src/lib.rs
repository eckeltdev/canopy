//! Canopy CSS-lite: a tiny, dependency-free CSS subset that lets authors style
//! with **selector rules** instead of per-node inline calls.
//!
//! A stylesheet is a sequence of rules, each a selector list and a declaration block:
//!
//! ```text
//! button, .btn      { background: navy; border-width: 2 }
//! #hero .card.lead  { margin: 8; color: #f80 }
//! ```
//!
//! [`parse`] turns that string into a [`Stylesheet`]. Each declaration's property
//! *name* is mapped to the matching [`canopy_paint`] [`PropId`] const and its value
//! is normalized (a trailing `px` is stripped; colors fold to canonical `#rrggbb`),
//! so the resolved pairs feed the **existing inline-style path unchanged**:
//! [`Stylesheet::apply`] simply replays them through [`canopy_view::App::style`].
//!
//! # Selectors and specificity
//!
//! A selector is a single **compound** of simple selectors — an optional leading
//! type/tag name followed by any run of `.class` and `#id` parts — plus an optional
//! `:hover` pseudo-class. `*` is the universal (matches anything). Commas group
//! several selectors onto one declaration block.
//!
//! ```text
//! div                   /* type            */
//! #hero                 /* id              */
//! .card                 /* class           */
//! button.primary#go     /* compound: all parts must match */
//! .btn:hover            /* class + state   */
//! *                     /* universal       */
//! ```
//!
//! Matching is resolved against a [`MatchTarget`] (the element's type name, id, and
//! class list). [`Stylesheet::resolve_for`] is the **cascade resolver**: it gathers
//! every rule whose selector matches, orders them by CSS **specificity** (id = 100,
//! class/pseudo = 10, type = 1; ties broken by source order), and folds their
//! declarations **last-wins** per [`PropId`] — a higher-specificity (or later) rule
//! overrides an earlier one on the same property, while untouched properties are
//! preserved. `:hover` rules join the cascade only when `hovered` is set.
//! [`Stylesheet::resolve`] is the legacy class-only entry point (a [`MatchTarget`]
//! with no type/id); [`Stylesheet::apply_state`] replays a resolution onto an
//! [`App`], which the host re-calls whenever a node's hover state flips.
//!
//! # Supported properties and colors
//!
//! Box / flex / paint properties map to their [`canopy_paint`] ids: `background`,
//! `color`, `width`/`height`, `min-`/`max-` sizing, the `margin`/`padding`/`inset`
//! box edges (each as a shorthand *and* its per-side longhands), `gap`/`row-gap`/
//! `column-gap`, `display`/`visibility`/`position`/`overflow`/`box-sizing`,
//! `z-index`/`aspect-ratio`, the flex item/container props (`flex` shorthand,
//! `flex-grow`/`-shrink`/`-basis`/`-wrap`, `align-self`), the border frame
//! (`border` shorthand, per-side widths/colors, `border-style`, per-corner radii),
//! `font-size`/`font-weight`/`line-height`/`text-decoration`, the `outline`
//! shorthand + `outline-width`/`-color`/`-offset`, `box-shadow`,
//! `background-image`, `opacity`, `direction`, `align`, `justify`, `text-align`,
//! and the `translate-x`/`translate-y` offsets.
//!
//! **Shorthand + per-side expansion** happens at parse time: `margin: 8 16`
//! expands to the four `margin-*` longhands per the CSS 1/2/3/4-value rules
//! (`padding`/`inset` likewise), `gap: a b` -> `row-gap`/`column-gap`,
//! `border: 2 solid red` / `flex: 1 0 auto` / `outline: 1 solid red` split by
//! token shape. Lengths accept a bare number or a `px` suffix and preserve a
//! leading `-` (negatives) and a trailing `%`; `auto` passes through. Colors accept
//! a named keyword (`navy`, `red`, `transparent`, …), `#rgb`/`#rgba`/`#rrggbb`/
//! `#rrggbbaa`, `rgb(r, g, b)`, or `rgba(r, g, b, a)` — normalized to `#rrggbb` when
//! opaque and `#rrggbbaa` when alpha < 255. A trailing `!important` is stripped
//! (precedence not yet honored) and the CSS-wide keywords `inherit`/`initial`/
//! `unset` drop their declaration cleanly.
//!
//! # What this is *not*
//!
//! This is a deliberate subset, not a full CSS engine:
//!
//! - Selectors are a single compound only — no **descendant/child/sibling
//!   combinators**, no attribute selectors, and no pseudo-classes beyond `:hover`;
//!   no media queries. Box shorthands *are* expanded, but `!important` is only
//!   stripped (its precedence is not yet honored).
//! - The cascade matches each node against its own identity; there is no inheritance
//!   here (the host folds matched rules in as inline styles, and author inline styles
//!   win, mirroring CSS specificity where inline beats a selector).
//! - Unknown properties are silently ignored (skipped, never an error), as is any
//!   malformed fragment, so a partial stylesheet still yields the rules it could
//!   parse.
//!
//! `no_std` + `alloc`; zero external crates.
//!
//! [`apply`]: Stylesheet::apply
//! [`apply_state`]: Stylesheet::apply_state

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use canopy_paint::{
    ALIGN, ALIGN_SELF, ASPECT_RATIO, BACKGROUND_IMAGE, BG, BORDER_BOTTOM_COLOR,
    BORDER_BOTTOM_LEFT_RADIUS, BORDER_BOTTOM_RIGHT_RADIUS, BORDER_BOTTOM_WIDTH, BORDER_COLOR,
    BORDER_LEFT_COLOR, BORDER_LEFT_WIDTH, BORDER_RIGHT_COLOR, BORDER_RIGHT_WIDTH, BORDER_STYLE,
    BORDER_TOP_COLOR, BORDER_TOP_LEFT_RADIUS, BORDER_TOP_RIGHT_RADIUS, BORDER_TOP_WIDTH,
    BORDER_WIDTH, BOX_SHADOW, BOX_SIZING, COLUMN_GAP, DIRECTION, DISPLAY, FG, FLEX_BASIS,
    FLEX_GROW, FLEX_SHRINK, FLEX_WRAP, FONT_SIZE, FONT_WEIGHT, GAP, HEIGHT, INSET_BOTTOM,
    INSET_LEFT, INSET_RIGHT, INSET_TOP, JUSTIFY, LINE_HEIGHT, MARGIN, MARGIN_BOTTOM, MARGIN_LEFT,
    MARGIN_RIGHT, MARGIN_TOP, MAX_HEIGHT, MAX_WIDTH, MIN_HEIGHT, MIN_WIDTH, OPACITY, OUTLINE_COLOR,
    OUTLINE_OFFSET, OUTLINE_WIDTH, OVERFLOW, PADDING, PADDING_BOTTOM, PADDING_LEFT, PADDING_RIGHT,
    PADDING_TOP, POSITION, RADIUS, ROW_GAP, TEXT_ALIGN, TEXT_DECORATION, TRANSLATE_X, TRANSLATE_Y,
    VISIBILITY, WIDTH, Z_INDEX,
};
use canopy_protocol::{NodeId, PropId};
use canopy_view::App;

/// The resolved declarations for one class: the property id and its normalized
/// value, in source order.
type Decl = (PropId, String);

/// The interaction state a rule's `:hover`-style pseudo-class binds it to.
///
/// `Base` rules (a plain `.name` selector) always apply; stateful rules apply only
/// when the node is in that state. Today the only stateful variant is [`State::Hover`]
/// (`.name:hover`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// A plain selector with no pseudo-class; always applies.
    Base,
    /// A `:hover` rule; applies only when the node is hovered.
    Hover,
}

/// One part of a **compound** selector. A compound is an AND of these against a single
/// element: `button.primary#go` is `[Type("button"), Class("primary"), Id("go")]`. There are
/// no combinators (descendant/child) in this lite subset — each rule targets one element.
#[derive(Clone, PartialEq, Eq)]
enum Simple {
    /// A type/tag selector (`button`, `div`) — matches the element's type name.
    Type(String),
    /// An id selector (`#go`) — matches the element's id.
    Id(String),
    /// A class selector (`.primary`) — matches if the element carries that class.
    Class(String),
}

/// CSS **specificity** as the standard `(a, b, c)` tuple — `a` = id count, `b` = class +
/// pseudo-class count, `c` = type count — compared lexicographically (it derives `Ord`, so
/// the tuple order *is* the comparison order). Unlike a packed `a*100 + b*10 + c` integer, this
/// never overflows or mis-orders past 10 of any kind (e.g. 11 classes still beats 1 id correctly,
/// and 11 classes outrank 10). Ties break on source order at the call site.
type Spec = (u32, u32, u32);

/// A compound selector plus its pseudo-state and CSS **specificity** (see [`Spec`]).
struct Selector {
    simples: Vec<Simple>,
    state: State,
    specificity: Spec,
}

/// One parsed rule: a compound selector and the declarations it sets. Selector grouping
/// (`.a, .b { … }`) expands at parse time to one `Rule` per selector, sharing the decls.
struct Rule {
    selector: Selector,
    /// Declarations whose property name mapped to a known [`PropId`], in order.
    decls: Vec<Decl>,
}

/// The element a stylesheet is resolved against: its type/tag name, id, and class list. A
/// `Type`/`Id` simple selector only matches when the corresponding field is `Some` and equal,
/// so the legacy class-only [`Stylesheet::resolve`] (which leaves both `None`) keeps matching
/// exactly the pure-class rules it always did.
pub struct MatchTarget<'a> {
    /// The element's type/tag name (e.g. `"button"`), or `None`.
    pub type_name: Option<&'a str>,
    /// The element's id, or `None`.
    pub id: Option<&'a str>,
    /// The element's classes.
    pub classes: &'a [&'a str],
}

/// A parsed CSS-lite stylesheet: a set of class rules, each resolved to
/// `(PropId, value)` declarations ready for the inline-style path.
///
/// Build one with [`parse`]; query it with [`Stylesheet::declarations`] or replay a
/// node's classes onto an [`App`] with [`Stylesheet::apply`].
#[derive(Default)]
pub struct Stylesheet {
    rules: Vec<Rule>,
}

impl Stylesheet {
    /// An empty stylesheet with no rules.
    pub fn new() -> Self {
        Self::default()
    }

    /// The resolved **base** declarations for `class` (without a leading `.`), in
    /// source order. Returns an empty slice if no base rule names that class.
    ///
    /// This is the base-only path: `:hover` rules are not considered here — use
    /// [`resolve`] for the stateful cascade. When the same class appears in more than
    /// one base rule, the *first* base rule's declarations are returned (the original
    /// behavior); the full last-wins concatenation across rules lives in [`resolve`].
    ///
    /// [`resolve`]: Stylesheet::resolve
    pub fn declarations(&self, class: &str) -> &[Decl] {
        for rule in &self.rules {
            if rule.selector.state == State::Base
                && rule.selector.simples.len() == 1
                && matches!(&rule.selector.simples[0], Simple::Class(c) if c == class)
            {
                return &rule.decls;
            }
        }
        &[]
    }

    /// Resolve the final declarations for an element from its full [`MatchTarget`] (type, id,
    /// classes), applying CSS **specificity + source order**: every rule whose compound selector
    /// matches the element (and whose `:hover` state is satisfied by `hovered`) is collected, the
    /// matches are ordered by `(specificity, source position)`, and their declarations are folded
    /// last-wins — so a higher-specificity rule (or, at equal specificity, a later one) wins each
    /// property. Properties no matching rule touches are absent. The returned pairs are in first-
    /// appearance order (the order [`apply_state`] replays inline-style ops).
    pub fn resolve_for(&self, target: &MatchTarget, hovered: bool) -> Vec<Decl> {
        // Collect matching rules with their (specificity, source index) so we can order the
        // cascade correctly regardless of the order classes appear on the element. The
        // specificity is the `(a, b, c)` tuple, so the sort below is a true lexicographic
        // CSS comparison (id > class/pseudo > type) with the source index as the tie-break.
        let mut matched: Vec<(Spec, usize)> = Vec::new();
        for (idx, rule) in self.rules.iter().enumerate() {
            let state_ok = match rule.selector.state {
                State::Base => true,
                State::Hover => hovered,
            };
            if state_ok && selector_matches(&rule.selector.simples, target) {
                matched.push((rule.selector.specificity, idx));
            }
        }
        matched.sort_unstable(); // ascending (specificity, idx): lowest precedence applied first
        let mut resolved: Vec<Decl> = Vec::new();
        for (_, idx) in matched {
            for (prop, value) in &self.rules[idx].decls {
                cascade(&mut resolved, *prop, value);
            }
        }
        resolved
    }

    /// The legacy class-only resolve: a [`resolve_for`](Self::resolve_for) with no type/id, so it
    /// matches exactly the pure-class rules it always did. Kept for `canopy-ui` / `LiteEngine`.
    pub fn resolve(&self, classes: &[&str], hovered: bool) -> Vec<Decl> {
        self.resolve_for(
            &MatchTarget {
                type_name: None,
                id: None,
                classes,
            },
            hovered,
        )
    }

    /// Whether any of `classes` has a `:hover` rule, i.e. the node would restyle when
    /// the pointer enters or leaves it. A class-only predicate (type/id `:hover` rules are not
    /// considered) — the cheap "is this node worth tracking for hover" check `canopy-ui` uses.
    #[must_use]
    pub fn reacts_to_hover(&self, classes: &[&str]) -> bool {
        self.rules.iter().any(|rule| {
            rule.selector.state == State::Hover
                && rule
                    .selector
                    .simples
                    .iter()
                    .any(|s| matches!(s, Simple::Class(c) if classes.contains(&c.as_str())))
        })
    }

    /// Apply `classes` to `node` on `app`, in order, by replaying each resolved
    /// declaration through [`App::style`]. Later classes override earlier ones
    /// because the later inline-style op simply overwrites the property.
    ///
    /// This is the base-only replay (no `:hover`); use [`apply_state`] to fold hover
    /// state into the cascade. There is no cascade across the tree (see the crate
    /// docs).
    ///
    /// [`apply_state`]: Stylesheet::apply_state
    pub fn apply(&self, app: &App, node: NodeId, classes: &[&str]) {
        for class in classes {
            for (prop, value) in self.declarations(class) {
                app.style(node, *prop, value);
            }
        }
    }

    /// Apply the [`resolve`]d declarations for `classes` at the given `hovered` state
    /// onto `node`, replaying each through [`App::style`].
    ///
    /// The host calls this whenever a node's hover state changes: passing
    /// `hovered = true` layers the `:hover` rules over the base, and passing
    /// `hovered = false` re-applies the base-only resolution. Because each call
    /// re-emits the full resolved set, the latest call's values overwrite whatever a
    /// prior call wrote for the properties both touch.
    ///
    /// [`resolve`]: Stylesheet::resolve
    pub fn apply_state(&self, app: &App, node: NodeId, classes: &[&str], hovered: bool) {
        for (prop, value) in self.resolve(classes, hovered) {
            app.style(node, prop, &value);
        }
    }
}

/// Fold one declaration into the resolved set with last-wins semantics: overwrite the
/// value if `prop` is already present (preserving its original position), otherwise
/// append it.
fn cascade(resolved: &mut Vec<Decl>, prop: PropId, value: &str) {
    for entry in resolved.iter_mut() {
        if entry.0 == prop {
            entry.1.clear();
            entry.1.push_str(value);
            return;
        }
    }
    resolved.push((prop, value.to_string()));
}

/// Parse a CSS-lite stylesheet of class rules into a [`Stylesheet`].
///
/// Whitespace and newlines are flexible; `/* … */` comments are stripped. Each rule
/// is `.name { prop: value; … }`. Property names are mapped to [`PropId`]s and
/// values normalized; unknown properties and malformed fragments are skipped.
pub fn parse(css: &str) -> Stylesheet {
    let css = strip_comments(css);
    let mut rules = Vec::new();
    let bytes = css.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Read the selector-list (everything up to `{`); a stray `}`/`;` is skipped.
        if bytes[i] == b'}' || bytes[i] == b';' || bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let sel_start = i;
        while i < bytes.len() && bytes[i] != b'{' {
            i += 1;
        }
        if i >= bytes.len() {
            break; // a trailing selector with no block — drop it
        }
        let selector_list = css[sel_start..i].trim();
        i += 1; // consume `{`

        // Capture the block body up to `}`.
        let body_start = i;
        while i < bytes.len() && bytes[i] != b'}' {
            i += 1;
        }
        let body = &css[body_start..i];
        if i < bytes.len() {
            i += 1; // consume `}`
        }

        let decls = parse_block(body);
        if decls.is_empty() {
            continue; // a rule with no known declarations contributes nothing
        }
        // Selector grouping: `.a, button#b { … }` expands to one Rule per selector, sharing decls.
        for sel in selector_list.split(',') {
            if let Some(selector) = parse_selector(sel.trim()) {
                rules.push(Rule {
                    selector,
                    decls: decls.clone(),
                });
            }
        }
    }

    Stylesheet { rules }
}

/// Parse one selector — a compound (`button.primary#go`) plus an optional `:hover` — into a
/// [`Selector`] with its specificity. Returns `None` for an empty selector or one carrying an
/// unsupported pseudo-class (`:focus`, `::before`, …) so it is dropped (not mistaken for a base).
fn parse_selector(sel: &str) -> Option<Selector> {
    if sel.is_empty() {
        return None;
    }
    let (compound, state) = match sel.split_once(':') {
        // Pseudo-classes are ASCII case-insensitive (`:HOVER` == `:hover`).
        Some((compound, pseudo)) if pseudo.eq_ignore_ascii_case("hover") => {
            (compound, State::Hover)
        }
        Some(_) => return None, // unsupported pseudo-class -> drop this selector
        None => (sel, State::Base),
    };
    let simples = parse_compound(compound)?;
    let (mut ids, mut classes, mut types) = (0u32, 0u32, 0u32);
    for simple in &simples {
        match simple {
            Simple::Id(_) => ids += 1,
            Simple::Class(_) => classes += 1,
            Simple::Type(_) => types += 1,
        }
    }
    if state == State::Hover {
        classes += 1; // a pseudo-class counts at the class level of specificity
    }
    Some(Selector {
        simples,
        state,
        specificity: (ids, classes, types),
    })
}

/// Parse a compound selector into its simple parts: an optional leading **type** name, then a run
/// of `.class` / `#id`. A bare `*` (universal) yields an empty list (matches every element).
/// Returns `None` on a malformed identifier.
fn parse_compound(compound: &str) -> Option<Vec<Simple>> {
    let mut simples = Vec::new();
    let bytes = compound.as_bytes();
    let mut i = 0;
    // Optional leading type/tag name (anything before the first `.`/`#`).
    while i < bytes.len() && bytes[i] != b'.' && bytes[i] != b'#' {
        i += 1;
    }
    let head = &compound[..i];
    if !head.is_empty() && head != "*" {
        if !is_ident(head) {
            return None;
        }
        simples.push(Simple::Type(head.to_string()));
    }
    // Then a run of `.class` / `#id` parts.
    while i < bytes.len() {
        let kind = bytes[i];
        i += 1;
        let name_start = i;
        while i < bytes.len() && bytes[i] != b'.' && bytes[i] != b'#' {
            i += 1;
        }
        let name = &compound[name_start..i];
        if !is_ident(name) {
            return None;
        }
        match kind {
            b'.' => simples.push(Simple::Class(name.to_string())),
            b'#' => simples.push(Simple::Id(name.to_string())),
            _ => return None,
        }
    }
    Some(simples)
}

/// A valid (lite) CSS identifier: non-empty, only `[A-Za-z0-9_-]`.
fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether a compound selector's simple parts all match `target` (an AND).
///
/// Type names are matched ASCII case-insensitively (`BUTTON` matches `<button>`), per HTML's
/// case-insensitive tag names. Classes and ids stay case-**sensitive**, per CSS.
fn selector_matches(simples: &[Simple], target: &MatchTarget) -> bool {
    simples.iter().all(|simple| match simple {
        Simple::Type(t) => target
            .type_name
            .is_some_and(|name| name.eq_ignore_ascii_case(t)),
        Simple::Id(id) => target.id == Some(id.as_str()),
        Simple::Class(c) => target.classes.contains(&c.as_str()),
    })
}

/// Remove `/* … */` comments, replacing each with a single space so adjacent tokens
/// don't fuse. Unterminated comments swallow the rest of the input (CSS behavior).
fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let bytes = css.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 2; // consume `*/` (or run off the end if unterminated)
            out.push(' ');
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Parse a `prop: value; prop: value` block body into resolved declarations,
/// skipping unknown properties and malformed `prop: value` pairs.
///
/// Box shorthands (`margin`, `padding`, `inset`, `gap`, `border`, `flex`, `outline`) are
/// **expanded at parse time** into their per-side / per-axis longhands (see
/// [`expand_shorthand`]), each then normalized exactly as a directly written longhand would be.
/// A trailing `!important` is stripped (its precedence is not yet honored) so it never drops the
/// declaration, and the CSS-wide keywords `inherit`/`initial`/`unset` drop their single
/// declaration cleanly (real semantics land in a later wave) rather than failing a value parse.
fn parse_block(body: &str) -> Vec<Decl> {
    let mut decls = Vec::new();
    for stmt in body.split(';') {
        let stmt = stmt.trim();
        if stmt.is_empty() {
            continue;
        }
        let Some((name, value)) = stmt.split_once(':') else {
            continue;
        };
        let name = name.trim();
        // Strip a trailing `!important` (any casing) and re-trim, so `color: red !important`
        // resolves to red instead of dropping. Full precedence comes later.
        let value = strip_important(value.trim()).trim();
        if value.is_empty() {
            continue;
        }
        // CSS-wide keywords have no lite semantics yet: drop the declaration cleanly rather than
        // feed `inherit`/`initial`/`unset` into a color/number parse that would fail.
        if is_css_wide_keyword(value) {
            continue;
        }
        expand_shorthand(name, value, &mut decls);
    }
    decls
}

/// Strip a trailing `!important` (ASCII case-insensitive, optional whitespace before the `!`)
/// from a declaration value, returning the remainder. If there is no `!important`, the value is
/// returned unchanged. Precedence is not yet modeled — this just keeps the declaration alive.
fn strip_important(value: &str) -> &str {
    // Find the last `!` and check the suffix is `important` (case-insensitive) after trimming.
    if let Some(bang) = value.rfind('!') {
        let after = value[bang + 1..].trim();
        if after.eq_ignore_ascii_case("important") {
            return value[..bang].trim_end();
        }
    }
    value
}

/// Whether `value` is a CSS-wide keyword (`inherit` / `initial` / `unset`), matched ASCII
/// case-insensitively. These are dropped by [`parse_block`] until a later wave gives them meaning.
fn is_css_wide_keyword(value: &str) -> bool {
    value.eq_ignore_ascii_case("inherit")
        || value.eq_ignore_ascii_case("initial")
        || value.eq_ignore_ascii_case("unset")
}

/// Expand a (possibly shorthand) declaration into one or more normalized longhand [`Decl`]s,
/// pushing them onto `decls`.
///
/// Single-value `margin`/`padding`/`gap` keep their historical uniform mapping (`MARGIN`,
/// `PADDING`, `GAP`); a multi-value form expands to the per-side / per-axis longhands following
/// the CSS 1/2/3/4-value box rules. `inset` always expands to the four `INSET_*` sides. `border`,
/// `flex`, and `outline` split their space-separated parts by shape (length / keyword / color).
/// The two complex-value props `background-image` (a `linear-gradient`) and `box-shadow` are
/// reduced to a canonical form by [`normalize_background_image`] / [`normalize_box_shadow`], and a
/// value that doesn't parse drops its declaration cleanly. Any other longhand whose name maps to a
/// known [`PropId`] is normalized through [`normalize_value`]; unknown names are silently skipped,
/// mirroring the longhand path.
fn expand_shorthand(name: &str, value: &str, decls: &mut Vec<Decl>) {
    // Split the value into whitespace-separated tokens (shorthands are space-delimited).
    let parts: Vec<&str> = value.split_ascii_whitespace().collect();

    match name {
        // `margin`/`padding`: single value keeps the uniform PropId (unchanged behavior); a
        // multi-value form expands per side. `inset` has no uniform PropId, so even a single
        // value sets all four sides.
        "margin" if parts.len() == 1 => push_decl(decls, MARGIN, value),
        "margin" => expand_box(
            decls,
            &parts,
            [MARGIN_TOP, MARGIN_RIGHT, MARGIN_BOTTOM, MARGIN_LEFT],
        ),
        "padding" if parts.len() == 1 => push_decl(decls, PADDING, value),
        "padding" => expand_box(
            decls,
            &parts,
            [PADDING_TOP, PADDING_RIGHT, PADDING_BOTTOM, PADDING_LEFT],
        ),
        "inset" => expand_box(
            decls,
            &parts,
            [INSET_TOP, INSET_RIGHT, INSET_BOTTOM, INSET_LEFT],
        ),
        // `gap`: single -> uniform GAP (unchanged); `gap: row column` -> ROW_GAP, COLUMN_GAP.
        "gap" if parts.len() == 1 => push_decl(decls, GAP, value),
        "gap" => {
            if let [row, col, ..] = parts.as_slice() {
                push_decl(decls, ROW_GAP, row);
                push_decl(decls, COLUMN_GAP, col);
            }
        }
        // `border: <width> <style> <color>` in any order: width is the length, style is one of the
        // border-style keywords, color is whatever normalizes to a color; missing parts omitted.
        "border" => expand_border(decls, &parts),
        // `flex: grow [shrink [basis]]`.
        "flex" => match parts.as_slice() {
            [g] => push_decl(decls, FLEX_GROW, g),
            [g, s] => {
                push_decl(decls, FLEX_GROW, g);
                push_decl(decls, FLEX_SHRINK, s);
            }
            [g, s, basis, ..] => {
                push_decl(decls, FLEX_GROW, g);
                push_decl(decls, FLEX_SHRINK, s);
                push_decl(decls, FLEX_BASIS, basis);
            }
            [] => {}
        },
        // `outline: <width> <style> <color>`: width + color are kept, style ignored for now.
        "outline" => expand_outline(decls, &parts),
        // `background-image: linear-gradient(...)`: normalize to the canonical
        // `linear-gradient(<deg>, <#hex>[, <#hex>...])`. A value the parser can't make sense of
        // (unsupported function, bad colors) is DROPPED cleanly rather than emitted half-baked.
        "background-image" => {
            if let Some(canon) = normalize_background_image(value) {
                decls.push((BACKGROUND_IMAGE, canon));
            }
        }
        // `box-shadow: <dx> <dy> [<blur> [<spread>]] <color> [inset]` in either color-first or
        // color-last order: normalize to the canonical `<dx> <dy> <blur> <#hex>` (spread + inset
        // dropped). A value that doesn't parse is DROPPED cleanly.
        "box-shadow" => {
            if let Some(canon) = normalize_box_shadow(value) {
                decls.push((BOX_SHADOW, canon));
            }
        }
        // Not a shorthand: map the single property name directly.
        _ => {
            if let Some(prop) = map_property(name) {
                push_decl(decls, prop, value);
            }
        }
    }
}

/// Push one normalized longhand `Decl` for an already-resolved `PropId`.
fn push_decl(decls: &mut Vec<Decl>, prop: PropId, value: &str) {
    decls.push((prop, normalize_value(prop, value)));
}

/// Expand a per-side box shorthand (`margin`/`padding`/`inset`) over its `[top, right, bottom,
/// left]` PropIds, applying the CSS 1/2/3/4-value rules:
/// - 1 value  -> all four sides
/// - 2 values -> `a`=top/bottom, `b`=right/left
/// - 3 values -> `a`=top, `b`=right/left, `c`=bottom
/// - 4 values -> top, right, bottom, left
///
/// Each side's value is normalized as the corresponding longhand. A 0- or >4-token value is
/// ignored (malformed).
fn expand_box(decls: &mut Vec<Decl>, parts: &[&str], sides: [PropId; 4]) {
    let [top_id, right_id, bottom_id, left_id] = sides;
    let (top, right, bottom, left) = match parts {
        [a] => (*a, *a, *a, *a),
        [a, b] => (*a, *b, *a, *b),
        [a, b, c] => (*a, *b, *c, *b),
        [a, b, c, d] => (*a, *b, *c, *d),
        _ => return,
    };
    push_decl(decls, top_id, top);
    push_decl(decls, right_id, right);
    push_decl(decls, bottom_id, bottom);
    push_decl(decls, left_id, left);
}

/// Expand `border: <width> <style> <color>` (any order, parts optional) into `BORDER_WIDTH`,
/// `BORDER_STYLE`, and `BORDER_COLOR`. A token is classified as the width if it is a length, the
/// style if it is a border-style keyword, else the color.
fn expand_border(decls: &mut Vec<Decl>, parts: &[&str]) {
    for &tok in parts {
        if is_length(tok) {
            push_decl(decls, BORDER_WIDTH, tok);
        } else if is_border_style(tok) {
            push_decl(decls, BORDER_STYLE, tok);
        } else {
            push_decl(decls, BORDER_COLOR, tok);
        }
    }
}

/// Expand `outline: <width> <style> <color>` into `OUTLINE_WIDTH` + `OUTLINE_COLOR`; the style
/// token is recognized but ignored for now.
fn expand_outline(decls: &mut Vec<Decl>, parts: &[&str]) {
    for &tok in parts {
        if is_length(tok) {
            push_decl(decls, OUTLINE_WIDTH, tok);
        } else if is_border_style(tok) {
            // style ignored for now
        } else {
            push_decl(decls, OUTLINE_COLOR, tok);
        }
    }
}

/// Whether `tok` is one of the recognized border-style keywords (ASCII case-insensitive).
fn is_border_style(tok: &str) -> bool {
    matches!(
        tok.to_ascii_lowercase().as_str(),
        "none" | "solid" | "dashed" | "dotted" | "double"
    )
}

/// Whether `tok` looks like a CSS length: an optional leading `-`, digits with an optional decimal
/// point, and an optional `px` suffix (a bare number also qualifies). Used by the `border`/
/// `outline` shorthands to tell a width token apart from a color/style.
fn is_length(tok: &str) -> bool {
    let body = tok.strip_suffix("px").unwrap_or(tok);
    let body = body.strip_prefix('-').unwrap_or(body);
    !body.is_empty()
        && body.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && body.bytes().filter(|&b| b == b'.').count() <= 1
        && body.bytes().any(|b| b.is_ascii_digit())
}

/// Map a CSS property name to its [`canopy_paint`] [`PropId`], or `None` if the
/// property is outside this subset.
fn map_property(name: &str) -> Option<PropId> {
    match name {
        "background" => Some(BG),
        "color" => Some(FG),
        "width" => Some(WIDTH),
        "height" => Some(HEIGHT),
        "gap" => Some(GAP),
        "padding" => Some(PADDING),
        "border-radius" | "radius" => Some(RADIUS),
        "direction" | "flex-direction" => Some(DIRECTION),
        // Animation paint props. `opacity` is unitless; the two translates are
        // signed px lengths. Their values are normalized in `normalize_value`.
        "opacity" => Some(OPACITY),
        "translate-x" => Some(TRANSLATE_X),
        "translate-y" => Some(TRANSLATE_Y),
        // Flex alignment keywords (values pass through verbatim).
        "align-items" | "align" => Some(ALIGN),
        "justify-content" | "justify" => Some(JUSTIFY),
        // Text alignment keyword (left/center/right; passes through verbatim).
        "text-align" => Some(TEXT_ALIGN),
        // Box model: outer margin + min/max sizing (all px lengths). The `margin`/`padding`
        // shorthands are handled in `expand_shorthand`; the per-side longhands map here.
        "margin" => Some(MARGIN),
        "margin-top" => Some(MARGIN_TOP),
        "margin-right" => Some(MARGIN_RIGHT),
        "margin-bottom" => Some(MARGIN_BOTTOM),
        "margin-left" => Some(MARGIN_LEFT),
        "padding-top" => Some(PADDING_TOP),
        "padding-right" => Some(PADDING_RIGHT),
        "padding-bottom" => Some(PADDING_BOTTOM),
        "padding-left" => Some(PADDING_LEFT),
        "min-width" => Some(MIN_WIDTH),
        "min-height" => Some(MIN_HEIGHT),
        "max-width" => Some(MAX_WIDTH),
        "max-height" => Some(MAX_HEIGHT),
        // Box display / formatting.
        "display" => Some(DISPLAY),
        "visibility" => Some(VISIBILITY),
        "position" => Some(POSITION),
        // Box-edge offsets: `top`/`right`/`bottom`/`left` (the `inset` shorthand expands to these).
        "top" => Some(INSET_TOP),
        "right" => Some(INSET_RIGHT),
        "bottom" => Some(INSET_BOTTOM),
        "left" => Some(INSET_LEFT),
        "z-index" => Some(Z_INDEX),
        "box-sizing" => Some(BOX_SIZING),
        "aspect-ratio" => Some(ASPECT_RATIO),
        "overflow" => Some(OVERFLOW),
        // Per-axis gaps (the `gap` shorthand expands to these for a two-value form).
        "row-gap" => Some(ROW_GAP),
        "column-gap" => Some(COLUMN_GAP),
        // Flex item / container longhands.
        "flex-grow" => Some(FLEX_GROW),
        "flex-shrink" => Some(FLEX_SHRINK),
        "flex-basis" => Some(FLEX_BASIS),
        "flex-wrap" => Some(FLEX_WRAP),
        "align-self" => Some(ALIGN_SELF),
        // Border frame: shorthand width/style/color + per-side widths/colors + per-corner radii.
        "border-width" => Some(BORDER_WIDTH),
        "border-color" => Some(BORDER_COLOR),
        "border-style" => Some(BORDER_STYLE),
        "border-top-width" => Some(BORDER_TOP_WIDTH),
        "border-right-width" => Some(BORDER_RIGHT_WIDTH),
        "border-bottom-width" => Some(BORDER_BOTTOM_WIDTH),
        "border-left-width" => Some(BORDER_LEFT_WIDTH),
        "border-top-color" => Some(BORDER_TOP_COLOR),
        "border-right-color" => Some(BORDER_RIGHT_COLOR),
        "border-bottom-color" => Some(BORDER_BOTTOM_COLOR),
        "border-left-color" => Some(BORDER_LEFT_COLOR),
        "border-top-left-radius" => Some(BORDER_TOP_LEFT_RADIUS),
        "border-top-right-radius" => Some(BORDER_TOP_RIGHT_RADIUS),
        "border-bottom-right-radius" => Some(BORDER_BOTTOM_RIGHT_RADIUS),
        "border-bottom-left-radius" => Some(BORDER_BOTTOM_LEFT_RADIUS),
        // Text / font properties.
        "font-size" => Some(FONT_SIZE),
        "font-weight" => Some(FONT_WEIGHT),
        "line-height" => Some(LINE_HEIGHT),
        "text-decoration" | "text-decoration-line" => Some(TEXT_DECORATION),
        // Outline (the `outline` shorthand expands to width + color).
        "outline-width" => Some(OUTLINE_WIDTH),
        "outline-color" => Some(OUTLINE_COLOR),
        "outline-offset" => Some(OUTLINE_OFFSET),
        // Effects with complex values: normalized to a canonical form in `expand_shorthand`
        // (`box-shadow` -> `<dx> <dy> <blur> <#hex>`; `background-image` -> the canonical
        // `linear-gradient(<deg>, <#hex>...)`), not through the `normalize_value` path below.
        "box-shadow" => Some(BOX_SHADOW),
        "background-image" => Some(BACKGROUND_IMAGE),
        _ => None,
    }
}

/// Normalize a value for `prop`: strip a trailing `px` from length values (keeping
/// the number), and pass colors, directions, and unitless values through verbatim.
///
/// The two translate props are lengths too, so they go through the same `px` strip —
/// but unlike the box-model sizes their numbers may be **negative** and
/// **fractional** (`-24px`, `12.5px`); we only remove the unit and never touch the
/// sign or decimal point, so the scene builder's signed-float reader sees the raw
/// number. [`OPACITY`] is deliberately excluded: it is a unitless float in `[0, 1]`,
/// so a stray `px` is *not* stripped (an authoring slip like `opacity: 0.5px` is left
/// intact to fail the float parse rather than silently becoming `0.5`).
fn normalize_value(prop: PropId, value: &str) -> String {
    // Colors (background / color / all border colors / outline color): expand to `#rrggbb` (opaque)
    // or `#rrggbbaa` (alpha < 255) so the renderers' hex `parse_color` accepts named colors, `#rgb`,
    // `rgba()`, and `transparent` too. An unrecognized value is left verbatim (the renderer then
    // ignores it).
    if is_color_prop(prop) {
        return normalize_color(value);
    }
    // `font-weight`: fold the `normal`/`bold` keywords to their numeric weights; pass a numeric
    // value (or anything else) through.
    if prop == FONT_WEIGHT {
        if value.eq_ignore_ascii_case("normal") {
            return "400".to_string();
        }
        if value.eq_ignore_ascii_case("bold") {
            return "700".to_string();
        }
        return value.to_string();
    }
    // `display`: `block` is the lite alias for `flex` (the only block-ish layout we model); other
    // values (`flex`, `none`, …) pass through verbatim.
    if prop == DISPLAY && value.eq_ignore_ascii_case("block") {
        return "flex".to_string();
    }
    // Lengths: strip a trailing `px`, keep a bare number, PRESERVE a leading `-` and a trailing `%`.
    // The `auto` keyword passes through verbatim (margins / inset / flex-basis). Anything that is
    // not a recognized length form (a stray keyword) also passes through untouched.
    if is_length_prop(prop) {
        if value.eq_ignore_ascii_case("auto") {
            return value.to_string();
        }
        if let Some(num) = value.strip_suffix("px") {
            return num.trim().to_string();
        }
    }
    // Keywords and the verbatim-passthrough props (z-index, flex-shrink, aspect-ratio,
    // display:flex|none, position, overflow, …): unchanged. (`box-shadow` / `background-image`
    // never reach here — their complex values are normalized in `expand_shorthand`.)
    value.to_string()
}

/// Whether `prop` carries a color value (and so must round-trip through [`normalize_color`]).
fn is_color_prop(prop: PropId) -> bool {
    prop == BG
        || prop == FG
        || prop == BORDER_COLOR
        || prop == BORDER_TOP_COLOR
        || prop == BORDER_RIGHT_COLOR
        || prop == BORDER_BOTTOM_COLOR
        || prop == BORDER_LEFT_COLOR
        || prop == OUTLINE_COLOR
}

/// Whether `prop` carries a CSS **length** (a `px`-strippable number that may keep a leading `-`,
/// a trailing `%`, or be the `auto` keyword). Excludes the unitless props (opacity, flex-grow,
/// flex-shrink, z-index, aspect-ratio) and the keyword props.
fn is_length_prop(prop: PropId) -> bool {
    prop == WIDTH
        || prop == HEIGHT
        || prop == GAP
        || prop == ROW_GAP
        || prop == COLUMN_GAP
        || prop == PADDING
        || prop == PADDING_TOP
        || prop == PADDING_RIGHT
        || prop == PADDING_BOTTOM
        || prop == PADDING_LEFT
        || prop == RADIUS
        || prop == TRANSLATE_X
        || prop == TRANSLATE_Y
        || prop == MARGIN
        || prop == MARGIN_TOP
        || prop == MARGIN_RIGHT
        || prop == MARGIN_BOTTOM
        || prop == MARGIN_LEFT
        || prop == INSET_TOP
        || prop == INSET_RIGHT
        || prop == INSET_BOTTOM
        || prop == INSET_LEFT
        || prop == MIN_WIDTH
        || prop == MIN_HEIGHT
        || prop == MAX_WIDTH
        || prop == MAX_HEIGHT
        || prop == FLEX_BASIS
        || prop == FONT_SIZE
        || prop == LINE_HEIGHT
        || prop == BORDER_WIDTH
        || prop == BORDER_TOP_WIDTH
        || prop == BORDER_RIGHT_WIDTH
        || prop == BORDER_BOTTOM_WIDTH
        || prop == BORDER_LEFT_WIDTH
        || prop == BORDER_TOP_LEFT_RADIUS
        || prop == BORDER_TOP_RIGHT_RADIUS
        || prop == BORDER_BOTTOM_RIGHT_RADIUS
        || prop == BORDER_BOTTOM_LEFT_RADIUS
        || prop == OUTLINE_WIDTH
        || prop == OUTLINE_OFFSET
}

/// Normalize a CSS color: a 6-digit hex passes through, `#rgb`/`#rgba` expand, `#rrggbbaa` passes
/// through, `rgb(r,g,b)` / `rgb(r g b)` and `rgba(r,g,b,a)` convert, and a CSS named color maps via
/// a small table. The output is `#rrggbb` when the color is opaque (alpha 255) and `#rrggbbaa` when
/// alpha < 255 — so existing opaque colors stay byte-stable. An unrecognized value is returned
/// verbatim so the renderer's `parse_color` simply rejects it (no paint).
fn normalize_color(value: &str) -> String {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        // `#rrggbb`: passes through unchanged (opaque).
        if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return value.to_string();
        }
        // `#rrggbbaa`: an opaque alpha (`ff`) collapses to the 6-digit form for byte-stability;
        // otherwise it passes through verbatim.
        if hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            if hex[6..8].eq_ignore_ascii_case("ff") {
                let mut out = String::with_capacity(7);
                out.push('#');
                out.push_str(&hex[..6]);
                return out;
            }
            return value.to_string();
        }
        // `#rgb` -> `#rrggbb` (each nibble doubled).
        if hex.len() == 3 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let mut out = String::with_capacity(7);
            out.push('#');
            for ch in hex.chars() {
                out.push(ch);
                out.push(ch);
            }
            return out;
        }
        // `#rgba` -> `#rrggbbaa` (each nibble doubled); an opaque `f` alpha collapses to `#rrggbb`.
        if hex.len() == 4 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let alpha_nibble = hex.as_bytes()[3];
            let opaque = alpha_nibble == b'f' || alpha_nibble == b'F';
            let rgb = if opaque { &hex[..3] } else { hex };
            let mut out = String::with_capacity(if opaque { 7 } else { 9 });
            out.push('#');
            for ch in rgb.chars() {
                out.push(ch);
                out.push(ch);
            }
            return out;
        }
    }
    if let Some(inner) = value.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        if let Some((r, g, b)) = parse_rgb_channels(inner) {
            return rgba_hex(r, g, b, 255);
        }
    }
    if let Some(inner) = value
        .strip_prefix("rgba(")
        .and_then(|s| s.strip_suffix(')'))
    {
        if let Some((r, g, b, a)) = parse_rgba_channels(inner) {
            return rgba_hex(r, g, b, a);
        }
    }
    if let Some(hex) = named_color(value) {
        return hex.to_string();
    }
    value.to_string()
}

/// Parse the three `r,g,b` channels of an `rgb(...)` body (comma- or space-separated), each a
/// `u8`; `None` if there are not exactly three valid channels.
fn parse_rgb_channels(inner: &str) -> Option<(u8, u8, u8)> {
    let mut chans = inner
        .split([',', ' ', '/'])
        .filter(|p| !p.trim().is_empty());
    match (chans.next(), chans.next(), chans.next(), chans.next()) {
        (Some(r), Some(g), Some(b), None) => Some((
            r.trim().parse::<u8>().ok()?,
            g.trim().parse::<u8>().ok()?,
            b.trim().parse::<u8>().ok()?,
        )),
        _ => None,
    }
}

/// Parse the four channels of an `rgba(...)` body: `r,g,b` as `u8`, and the alpha as a `0..=1`
/// float (per CSS), folded to a `0..=255` byte. `None` if malformed.
fn parse_rgba_channels(inner: &str) -> Option<(u8, u8, u8, u8)> {
    let mut chans = inner
        .split([',', ' ', '/'])
        .filter(|p| !p.trim().is_empty());
    let (r, g, b, a) = (chans.next()?, chans.next()?, chans.next()?, chans.next()?);
    if chans.next().is_some() {
        return None; // too many channels
    }
    let r = r.trim().parse::<u8>().ok()?;
    let g = g.trim().parse::<u8>().ok()?;
    let b = b.trim().parse::<u8>().ok()?;
    let alpha = a.trim().parse::<f32>().ok()?;
    // Clamp to [0, 1] and round to the nearest byte (0.0 -> 0, 1.0 -> 255).
    let alpha = alpha.clamp(0.0, 1.0);
    let a = (alpha * 255.0 + 0.5) as u8;
    Some((r, g, b, a))
}

/// Format an RGBA color as `#rrggbb` when opaque (`a == 255`) or `#rrggbbaa` otherwise, building
/// the hex by hand (no `format!`, to stay `no_std`-clean).
fn rgba_hex(r: u8, g: u8, b: u8, a: u8) -> String {
    let mut out = String::with_capacity(if a == 255 { 7 } else { 9 });
    out.push('#');
    push_hex_byte(&mut out, r);
    push_hex_byte(&mut out, g);
    push_hex_byte(&mut out, b);
    if a != 255 {
        push_hex_byte(&mut out, a);
    }
    out
}

/// Append `byte` as two lowercase hex digits (no `format!`, to stay `no_std`-clean).
fn push_hex_byte(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(HEX[(byte >> 4) as usize] as char);
    out.push(HEX[(byte & 0x0f) as usize] as char);
}

/// Map a CSS named color (case-insensitive) to its hex. The 16 HTML basic colors plus a handful of
/// common extras, and `transparent` -> `#00000000` (fully transparent black) now that the lite
/// color carries an alpha channel via the `#rrggbbaa` form.
fn named_color(name: &str) -> Option<&'static str> {
    // ASCII-lowercase compare without allocating.
    let eq = |kw: &str| {
        name.len() == kw.len()
            && name
                .bytes()
                .zip(kw.bytes())
                .all(|(a, b)| a.to_ascii_lowercase() == b)
    };
    let table: &[(&str, &str)] = &[
        ("black", "#000000"),
        ("white", "#ffffff"),
        ("red", "#ff0000"),
        ("green", "#008000"),
        ("lime", "#00ff00"),
        ("blue", "#0000ff"),
        ("yellow", "#ffff00"),
        ("cyan", "#00ffff"),
        ("aqua", "#00ffff"),
        ("magenta", "#ff00ff"),
        ("fuchsia", "#ff00ff"),
        ("gray", "#808080"),
        ("grey", "#808080"),
        ("silver", "#c0c0c0"),
        ("maroon", "#800000"),
        ("olive", "#808000"),
        ("teal", "#008080"),
        ("navy", "#000080"),
        ("purple", "#800080"),
        ("orange", "#ffa500"),
        ("pink", "#ffc0cb"),
        ("brown", "#a52a2a"),
        ("gold", "#ffd700"),
        ("transparent", "#00000000"),
    ];
    table.iter().find(|(kw, _)| eq(kw)).map(|(_, hex)| *hex)
}

// ---------------------------------------------------------------------------------
// Complex value normalizers: `background-image` linear-gradient + `box-shadow`.
//
// These two values are reduced to a tiny canonical grammar the (no_std) layout
// consumer can split on whitespace/commas without a real CSS value parser:
//   background-image: linear-gradient(<deg>, <#hex>[, <#hex>...])   (1..=8 stops)
//   box-shadow:       <dx> <dy> <blur> <#hex>                       (four tokens)
// Either returns `None` on anything it can't faithfully reduce, so the caller drops
// the whole declaration rather than emitting a half-formed value.
// ---------------------------------------------------------------------------------

/// The maximum number of gradient color stops the canonical form carries; matches the
/// consumer's inline `GradientStops` capacity (`canopy_traits::MAX_GRADIENT_STOPS`). Extra
/// stops past this are dropped (truncated to the first eight).
const MAX_GRADIENT_STOPS: usize = 8;

/// Normalize a CSS `background-image: linear-gradient(...)` into the canonical
/// `linear-gradient(<deg>, <#hex>[, <#hex>...])`:
/// - the direction folds to an integer degree (`to top`->0, `to right`->90, `to bottom`->180,
///   `to left`->270; a bare `<n>deg` or `<n>` keeps `n` mod 360); when no direction is given the
///   default is `180` (`to bottom`, CSS's default).
/// - each color stop is normalized through [`normalize_color`] to `#rrggbb`/`#rrggbbaa`; a per-stop
///   position percentage (`#fff 20%`) is dropped (only the color is kept).
/// - at most [`MAX_GRADIENT_STOPS`] stops are kept (the consumer's inline cap); extra stops are
///   truncated away.
///
/// Returns `None` (the caller then drops the declaration) when the value is not a
/// `linear-gradient(...)`, has no color stops, or a stop doesn't normalize to a `#hex` color.
fn normalize_background_image(value: &str) -> Option<String> {
    let inner = value
        .trim()
        .strip_prefix("linear-gradient(")
        .and_then(|s| s.strip_suffix(')'))?;
    // Split the function arguments on top-level commas. A `rgb(...)`/`rgba(...)` stop carries its
    // own commas, so track parenthesis depth and only split at depth 0.
    let args = split_top_level_commas(inner);
    let mut args = args.iter().map(|s| s.trim()).filter(|s| !s.is_empty());
    let first = args.next()?;

    // Decide whether the first argument is a direction or already the first color stop.
    let (deg, first_stop) = match parse_gradient_direction(first) {
        Some(deg) => (deg, None),
        None => (180, Some(first)), // no direction: default `to bottom` (180deg)
    };

    let mut out = String::new();
    out.push_str("linear-gradient(");
    push_int(&mut out, deg);

    let mut stops = 0usize;
    let push_stop = |out: &mut String, raw: &str, stops: &mut usize| -> Option<()> {
        if *stops >= MAX_GRADIENT_STOPS {
            return Some(()); // cap reached: silently drop further stops
        }
        // Drop a trailing per-stop position (`#fff 20%` / `red 0%`): keep only the color token.
        let color_tok = raw.split_ascii_whitespace().next()?;
        let hex = normalize_color(color_tok);
        if !is_hex_color(&hex) {
            return None; // a stop that doesn't resolve to a color invalidates the gradient
        }
        out.push_str(", ");
        out.push_str(&hex);
        *stops += 1;
        Some(())
    };

    if let Some(raw) = first_stop {
        push_stop(&mut out, raw, &mut stops)?;
    }
    for raw in args {
        push_stop(&mut out, raw, &mut stops)?;
    }
    if stops == 0 {
        return None; // a gradient with a direction but no color stops is malformed
    }
    out.push(')');
    Some(out)
}

/// Parse a `linear-gradient` direction argument into an integer degree, or `None` if the argument
/// is not a direction (so the caller treats it as the first color stop). Recognizes the four
/// orthogonal `to <side>` keywords (`to top`->0, `to right`->90, `to bottom`->180, `to left`->270),
/// a `<n>deg` angle, and a bare `<n>` integer; the angle is taken mod 360 (normalized to `0..360`).
fn parse_gradient_direction(arg: &str) -> Option<i32> {
    let arg = arg.trim();
    // `to <side>` (case-insensitive prefix), e.g. `to right`.
    if arg.len() >= 3 && arg[..3].eq_ignore_ascii_case("to ") {
        return match arg[3..].trim().to_ascii_lowercase().as_str() {
            "top" => Some(0),
            "right" => Some(90),
            "bottom" => Some(180),
            "left" => Some(270),
            _ => None, // diagonal "to top right" etc. unsupported -> not a recognized direction
        };
    }
    let num = arg.strip_suffix("deg").unwrap_or(arg).trim();
    // A bare number or `<n>deg`: parse as a (possibly signed) integer and normalize to `0..360`.
    // A non-integer (e.g. a color keyword `red`) returns `None` so it is treated as a color stop.
    let n: i32 = num.parse().ok()?;
    Some(n.rem_euclid(360))
}

/// Normalize a CSS `box-shadow` into the canonical four-token `<dx> <dy> <blur> <#hex>`.
///
/// Parses `[<dx> <dy> [<blur> [<spread>]]] <color> [inset]` with the color appearing **before or
/// after** the lengths: each token is classified as a length (px integer) or a color. `dx`/`dy`/
/// `blur` are px integers (a `px` suffix is stripped); a missing `blur` defaults to `0`. The
/// `spread` length (a 4th length) and the `inset` keyword are **dropped**. Returns `None` (caller
/// drops the declaration) when there is no color, fewer than two lengths, or a token is neither a
/// length nor a color.
fn normalize_box_shadow(value: &str) -> Option<String> {
    let mut color: Option<String> = None;
    let mut lengths: Vec<i32> = Vec::new();
    for tok in value.split_ascii_whitespace() {
        if tok.eq_ignore_ascii_case("inset") {
            continue; // inset dropped for now
        }
        if let Some(n) = parse_px_int(tok) {
            lengths.push(n);
            continue;
        }
        // Not a length: must be the (single) color. A second color is malformed.
        let hex = normalize_color(tok);
        if is_hex_color(&hex) && color.is_none() {
            color = Some(hex);
        } else {
            return None;
        }
    }
    let color = color?;
    // Need at least dx + dy; blur defaults to 0, spread (a 4th length) is dropped.
    let (dx, dy) = match lengths.as_slice() {
        [dx, dy, ..] => (*dx, *dy),
        _ => return None,
    };
    let blur = lengths.get(2).copied().unwrap_or(0);

    let mut out = String::new();
    push_int(&mut out, dx);
    out.push(' ');
    push_int(&mut out, dy);
    out.push(' ');
    push_int(&mut out, blur);
    out.push(' ');
    out.push_str(&color);
    Some(out)
}

/// Split `s` on commas that are **not** nested inside parentheses, returning each segment (untrimmed).
/// Used to separate `linear-gradient` arguments without breaking a `rgb(r, g, b)` color stop apart.
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Whether `s` is a canonical `#rrggbb` or `#rrggbbaa` hex color (the form [`normalize_color`]
/// produces for a value it recognized). A value `normalize_color` couldn't resolve comes back
/// verbatim and so fails this check.
fn is_hex_color(s: &str) -> bool {
    let Some(hex) = s.strip_prefix('#') else {
        return false;
    };
    (hex.len() == 6 || hex.len() == 8) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse a `box-shadow` length token into an integer px, stripping a `px` suffix. Accepts an
/// optional leading `-`; returns `None` for a non-integer (so it can be classified as a color).
/// A fractional value (`4.5px`) is rejected — the canonical shadow form carries integer px.
fn parse_px_int(tok: &str) -> Option<i32> {
    let body = tok.strip_suffix("px").unwrap_or(tok);
    if body.is_empty() {
        return None;
    }
    body.parse::<i32>().ok()
}

/// Append a signed integer to `out` in decimal, by hand (no `format!`, to stay `no_std`-clean).
fn push_int(out: &mut String, n: i32) {
    if n < 0 {
        out.push('-');
    }
    // Work in the unsigned domain so `i32::MIN` doesn't overflow when negated.
    let mut mag = (n as i64).unsigned_abs();
    if mag == 0 {
        out.push('0');
        return;
    }
    let mut digits = [0u8; 20];
    let mut len = 0;
    while mag > 0 {
        digits[len] = b'0' + (mag % 10) as u8;
        mag /= 10;
        len += 1;
    }
    for &d in digits[..len].iter().rev() {
        out.push(d as char);
    }
}

// ---------------------------------------------------------------------------------
// The constrained-tier StyleEngine.
// ---------------------------------------------------------------------------------

use alloc::collections::BTreeMap;

use canopy_dom::Dom;
use canopy_traits::{Color, ComputedStyle, HostError, StyleEngine};

/// CSS initial `color` — opaque black (`canvastext`).
const INITIAL_COLOR: Color = Color {
    r: 0,
    g: 0,
    b: 0,
    a: 255,
};
/// CSS initial `font-size` — `medium`, i.e. 16 logical px.
const INITIAL_FONT_SIZE: f32 = 16.0;

/// The **constrained-tier** [`StyleEngine`]: resolves a node's flat [`ComputedStyle`]
/// from its classes through the CSS-lite [`Stylesheet`], honoring the parent style for
/// inherited properties.
///
/// This is the lite twin of the capable tier's `StyloEngine::from_dom`: both cascade
/// the *real* [`canopy_dom`] tree behind the same [`StyleEngine`] trait, so one host
/// loop can drive either tier. The difference is only in the language — this engine has
/// the class-rule subset (no combinators, no specificity, no selector-driven
/// inheritance), so it resolves each node's own classes and pulls inherited `color` /
/// `font-size` from the `parent` argument per the [`StyleEngine::resolve`] contract.
pub struct LiteEngine {
    /// The parsed class rules.
    sheet: Stylesheet,
    /// Each node's class list, captured from the [`Dom`] at construction so `resolve`
    /// needs no further tree access.
    classes: BTreeMap<NodeId, Vec<String>>,
}

impl LiteEngine {
    /// Build a lite engine over `dom` with the CSS-lite stylesheet `css`. Walks the
    /// tree once to record each node's classes; `resolve` then needs only this engine.
    #[must_use]
    pub fn from_dom(dom: &Dom, css: &str) -> Self {
        let mut classes = BTreeMap::new();
        collect_classes(dom, canopy_dom::ROOT, &mut classes);
        Self {
            sheet: parse(css),
            classes,
        }
    }
}

/// Record `dom`'s class list for every descendant of `parent` (depth-first).
fn collect_classes(dom: &Dom, parent: NodeId, out: &mut BTreeMap<NodeId, Vec<String>>) {
    for &child in dom.children(parent) {
        out.insert(child, dom.classes(child).to_vec());
        collect_classes(dom, child, out);
    }
}

impl StyleEngine for LiteEngine {
    fn resolve(
        &mut self,
        node: NodeId,
        parent: Option<&ComputedStyle>,
    ) -> Result<ComputedStyle, HostError> {
        // Start from the CSS initial values, then — per the parent-inheritance
        // contract — seed inherited properties from the parent, since this reduced
        // resolver has no internal tree inheritance of its own.
        let mut style = ComputedStyle {
            color: INITIAL_COLOR,
            font_size: INITIAL_FONT_SIZE,
            opacity: 1.0,
            ..ComputedStyle::default()
        };
        if let Some(p) = parent {
            style.color = p.color;
            style.font_size = p.font_size;
        }

        let classes = self.classes.get(&node).ok_or(HostError::BadHandle)?;
        let refs: Vec<&str> = classes.iter().map(String::as_str).collect();
        for (prop, value) in self.sheet.resolve(&refs, false) {
            reduce(&mut style, prop, &value);
        }
        Ok(style)
    }
}

/// Fold one resolved `(PropId, value)` declaration into a [`ComputedStyle`]. Only the
/// properties the flat paint seam represents are mapped; layout/flex properties
/// (width, gap, direction, align, …) have no `ComputedStyle` field and are ignored.
fn reduce(style: &mut ComputedStyle, prop: PropId, value: &str) {
    if prop == BG {
        if let Some(c) = parse_color(value) {
            style.background = c;
        }
    } else if prop == FG {
        if let Some(c) = parse_color(value) {
            style.color = c;
        }
    } else if prop == PADDING {
        if let Ok(v) = value.parse::<f32>() {
            style.padding = v;
        }
    } else if prop == RADIUS {
        if let Ok(v) = value.parse::<f32>() {
            style.border_radius = v;
        }
    } else if prop == OPACITY {
        if let Ok(v) = value.parse::<f32>() {
            style.opacity = v;
        }
    }
}

/// Parse a `#rrggbb` color (the format the CSS-lite path carries through verbatim);
/// `None` on anything else.
fn parse_color(s: &str) -> Option<Color> {
    let hex = s.trim().strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    Some(Color {
        r: u8::from_str_radix(&hex[0..2], 16).ok()?,
        g: u8::from_str_radix(&hex[2..4], 16).ok()?,
        b: u8::from_str_radix(&hex[4..6], 16).ok()?,
        a: 255,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_dom::Dom;
    use canopy_traits::OpSink;
    use canopy_view::{App, COLUMN};

    const CSS: &str = ".btn { background: #313244; padding: 5px; } .danger { color: #f38ba8 }";

    #[test]
    fn parses_declarations_in_order_with_px_stripped() {
        let sheet = parse(CSS);
        let btn = sheet.declarations("btn");
        assert_eq!(btn.len(), 2);
        assert_eq!(btn[0], (BG, "#313244".to_string()));
        assert_eq!(btn[1], (PADDING, "5".to_string()));
    }

    #[test]
    fn color_passes_through() {
        let sheet = parse(CSS);
        assert_eq!(sheet.declarations("danger"), &[(FG, "#f38ba8".to_string())]);
    }

    #[test]
    fn unknown_property_is_ignored() {
        let sheet = parse(".x { background: #fff; -webkit-foo: 1px; zoom: 2 }");
        // `background` maps and `#fff` normalizes to the 6-digit `#ffffff`; the two unsupported
        // property names (`-webkit-foo`, `zoom`) are silently skipped.
        assert_eq!(sheet.declarations("x"), &[(BG, "#ffffff".to_string())]);
    }

    #[test]
    fn missing_class_is_empty() {
        let sheet = parse(CSS);
        assert!(sheet.declarations("nope").is_empty());
    }

    #[test]
    fn size_without_px_is_kept_verbatim() {
        let sheet = parse(".s { width: 12px; height: 12 }");
        assert_eq!(
            sheet.declarations("s"),
            &[(WIDTH, "12".to_string()), (HEIGHT, "12".to_string())]
        );
    }

    #[test]
    fn flex_direction_and_direction_both_map() {
        let a = parse(".a { direction: row }");
        let b = parse(".b { flex-direction: column }");
        assert_eq!(a.declarations("a"), &[(DIRECTION, "row".to_string())]);
        assert_eq!(b.declarations("b"), &[(DIRECTION, "column".to_string())]);
    }

    #[test]
    fn text_align_maps_and_passes_keyword_through_verbatim() {
        // `text-align` maps to TEXT_ALIGN, and its value is a keyword (not a length),
        // so `normalize_value` must leave it untouched (no `px` strip).
        let sheet = parse(".c { text-align: center } .r { text-align: right }");
        assert_eq!(
            sheet.declarations("c"),
            &[(TEXT_ALIGN, "center".to_string())]
        );
        assert_eq!(
            sheet.declarations("r"),
            &[(TEXT_ALIGN, "right".to_string())]
        );
    }

    #[test]
    fn percent_sizes_pass_through_normalize_unchanged() {
        // Percentages are not lengths-with-`px`: `normalize_value` only strips a
        // trailing `px`, so a `%` value round-trips verbatim for the layout engine to
        // resolve into a Taffy `percent`.
        let sheet = parse(".fill { width: 100%; height: 50% }");
        assert_eq!(
            sheet.declarations("fill"),
            &[(WIDTH, "100%".to_string()), (HEIGHT, "50%".to_string())]
        );
    }

    #[test]
    fn border_radius_maps_to_radius_with_px_stripped() {
        // Both spellings map to RADIUS, and the `px` unit is stripped like the other
        // pixel dimensions so the value feeds the integer inline-style path.
        let a = parse(".card { border-radius: 8px }");
        let b = parse(".pill { radius: 16 }");
        assert_eq!(a.declarations("card"), &[(RADIUS, "8".to_string())]);
        assert_eq!(b.declarations("pill"), &[(RADIUS, "16".to_string())]);
    }

    #[test]
    fn opacity_parses_as_a_unitless_float() {
        // `opacity` maps to OPACITY and its value is passed through verbatim — no
        // `px` strip, since it is a unitless ratio in [0, 1].
        let sheet = parse(".fade { opacity: 0.5 }");
        assert_eq!(sheet.declarations("fade"), &[(OPACITY, "0.5".to_string())]);
    }

    #[test]
    fn opacity_does_not_strip_a_stray_px() {
        // Unlike the length props, `opacity` is unitless: a bogus `px` is left on so
        // it fails the downstream float parse instead of silently becoming `0.5`.
        let sheet = parse(".x { opacity: 0.5px }");
        assert_eq!(sheet.declarations("x"), &[(OPACITY, "0.5px".to_string())]);
    }

    #[test]
    fn translate_x_y_parse_with_px_stripped_keeping_sign_and_decimal() {
        // Both translates are px lengths: the unit is stripped but the sign and the
        // decimal point survive, so a negative/fractional shift round-trips.
        let sheet = parse(".slide { translate-x: -24px; translate-y: 12.5px }");
        assert_eq!(
            sheet.declarations("slide"),
            &[
                (TRANSLATE_X, "-24".to_string()),
                (TRANSLATE_Y, "12.5".to_string()),
            ]
        );
    }

    #[test]
    fn translate_without_px_is_kept_verbatim() {
        // A bare number (no unit) is already in the form the scene builder reads.
        let sheet = parse(".t { translate-x: -8; translate-y: 4 }");
        assert_eq!(
            sheet.declarations("t"),
            &[
                (TRANSLATE_X, "-8".to_string()),
                (TRANSLATE_Y, "4".to_string()),
            ]
        );
    }

    #[test]
    fn comments_are_stripped() {
        let sheet = parse(".c /* sel */ { background /* k */ : #010203 /* v */ ; }");
        assert_eq!(sheet.declarations("c"), &[(BG, "#010203".to_string())]);
    }

    #[test]
    fn whitespace_and_newlines_are_flexible() {
        let css = "\n  .pad {\n    padding : 8px ;\n    gap:2px;\n  }\n";
        let sheet = parse(css);
        assert_eq!(
            sheet.declarations("pad"),
            &[(PADDING, "8".to_string()), (GAP, "2".to_string())]
        );
    }

    #[test]
    fn apply_writes_inline_styles_onto_the_node() {
        let sheet = parse(CSS);
        let app = App::new();
        let node = app.el(COLUMN);
        app.append(NodeId::new(0), node);
        sheet.apply(&app, node, &["btn"]);

        // Replay the emitted ops into a Dom and read the styles back.
        let mut dom = Dom::new();
        dom.apply(&app.take_batch(0)).unwrap();
        assert_eq!(dom.style(node, BG), Some("#313244"));
        assert_eq!(dom.style(node, PADDING), Some("5"));
    }

    #[test]
    fn later_class_overrides_earlier_in_order() {
        let sheet = parse(".base { background: #111111 } .skin { background: #222222 }");
        let app = App::new();
        let node = app.el(COLUMN);
        app.append(NodeId::new(0), node);
        // `skin` comes after `base`, so its background wins.
        sheet.apply(&app, node, &["base", "skin"]);

        let mut dom = Dom::new();
        dom.apply(&app.take_batch(0)).unwrap();
        assert_eq!(dom.style(node, BG), Some("#222222"));
    }

    #[test]
    fn apply_with_unknown_class_is_a_no_op() {
        let sheet = parse(CSS);
        let app = App::new();
        let node = app.el(COLUMN);
        app.append(NodeId::new(0), node);
        sheet.apply(&app, node, &["does-not-exist"]);

        let mut dom = Dom::new();
        dom.apply(&app.take_batch(0)).unwrap();
        assert_eq!(dom.style(node, BG), None);
        assert_eq!(dom.style(node, PADDING), None);
    }

    // --- :hover + cascade --------------------------------------------------

    const HOVER_CSS: &str =
        ".btn { background:#313244; padding:5px } .btn:hover { background:#585b70 }";

    #[test]
    fn hover_rule_does_not_leak_into_base_declarations() {
        // `declarations` is the base-only path: it must ignore the `:hover` rule.
        let sheet = parse(HOVER_CSS);
        assert_eq!(
            sheet.declarations("btn"),
            &[(BG, "#313244".to_string()), (PADDING, "5".to_string())]
        );
    }

    #[test]
    fn resolve_base_when_not_hovered() {
        let sheet = parse(HOVER_CSS);
        let resolved = sheet.resolve(&["btn"], false);
        assert_eq!(
            resolved,
            vec![(BG, "#313244".to_string()), (PADDING, "5".to_string())]
        );
    }

    #[test]
    fn resolve_hover_overrides_base_and_keeps_untouched_props() {
        let sheet = parse(HOVER_CSS);
        let resolved = sheet.resolve(&["btn"], true);
        // background overridden by `:hover`, padding preserved from the base rule.
        assert_eq!(
            resolved,
            vec![(BG, "#585b70".to_string()), (PADDING, "5".to_string())]
        );
    }

    #[test]
    fn resolve_unknown_class_is_empty() {
        let sheet = parse(HOVER_CSS);
        assert!(sheet.resolve(&["nope"], false).is_empty());
        assert!(sheet.resolve(&["nope"], true).is_empty());
        assert!(sheet.resolve(&[], true).is_empty());
    }

    #[test]
    fn resolve_multiple_classes_cascade_in_order() {
        let sheet =
            parse(".base { background:#111111; color:#eeeeee } .skin { background:#222222 }");
        let resolved = sheet.resolve(&["base", "skin"], false);
        // `skin`'s background wins (later class), `base`'s color is preserved.
        assert_eq!(
            resolved,
            vec![(BG, "#222222".to_string()), (FG, "#eeeeee".to_string())]
        );
    }

    #[test]
    fn resolve_hover_cascades_across_multiple_classes() {
        let sheet =
            parse(".a { background:#111111 } .b:hover { background:#222222 } .a:hover { background:#333333 }");
        // base-only: only `.a`'s base applies.
        assert_eq!(
            sheet.resolve(&["a", "b"], false),
            vec![(BG, "#111111".to_string())]
        );
        // hovered: `.a`(spec 10), `.b:hover`(spec 20), `.a:hover`(spec 20) all match. Sorted by
        // (specificity, source order): the two spec-20 rules tie, so the LATER one in the sheet
        // (`.a:hover` at index 2) wins -> #333333. (CSS source-order tie-break, not class order.)
        assert_eq!(
            sheet.resolve(&["a", "b"], true),
            vec![(BG, "#333333".to_string())]
        );
    }

    #[test]
    fn type_and_id_selectors_match() {
        let sheet =
            parse("button { background:#111111 } #go { color:#222222 } .btn { padding:4px }");
        let hit = MatchTarget {
            type_name: Some("button"),
            id: Some("go"),
            classes: &["btn"],
        };
        let r = sheet.resolve_for(&hit, false);
        assert!(
            r.contains(&(BG, "#111111".to_string())),
            "type selector matched"
        );
        assert!(
            r.contains(&(FG, "#222222".to_string())),
            "id selector matched"
        );
        assert!(
            r.contains(&(PADDING, "4".to_string())),
            "class selector matched"
        );
        let miss = MatchTarget {
            type_name: Some("div"),
            id: None,
            classes: &[],
        };
        assert!(
            sheet.resolve_for(&miss, false).is_empty(),
            "no selector matches a bare div"
        );
    }

    #[test]
    fn compound_selector_requires_all_parts() {
        let sheet = parse("button.primary { background:#abcdef }");
        let both = MatchTarget {
            type_name: Some("button"),
            id: None,
            classes: &["primary"],
        };
        assert_eq!(
            sheet.resolve_for(&both, false),
            vec![(BG, "#abcdef".to_string())]
        );
        let only_type = MatchTarget {
            type_name: Some("button"),
            id: None,
            classes: &[],
        };
        assert!(
            sheet.resolve_for(&only_type, false).is_empty(),
            "missing the .primary class"
        );
        let only_class = MatchTarget {
            type_name: Some("div"),
            id: None,
            classes: &["primary"],
        };
        assert!(
            sheet.resolve_for(&only_class, false).is_empty(),
            "wrong type"
        );
    }

    #[test]
    fn id_beats_class_beats_type_by_specificity() {
        // All three set `background`; specificity id(100) > class(10) > type(1) decides, regardless
        // of the source order (here type is last in the sheet but lowest specificity).
        let sheet = parse(
            "#x { background:#111111 } .c { background:#222222 } button { background:#333333 }",
        );
        let hit = MatchTarget {
            type_name: Some("button"),
            id: Some("x"),
            classes: &["c"],
        };
        assert_eq!(
            sheet.resolve_for(&hit, false),
            vec![(BG, "#111111".to_string())]
        );
    }

    #[test]
    fn selector_grouping_shares_declarations() {
        let sheet = parse(".a, .b, button { color:#445566 }");
        let targets = [
            MatchTarget {
                type_name: None,
                id: None,
                classes: &["a"],
            },
            MatchTarget {
                type_name: None,
                id: None,
                classes: &["b"],
            },
            MatchTarget {
                type_name: Some("button"),
                id: None,
                classes: &[],
            },
        ];
        for target in &targets {
            assert_eq!(
                sheet.resolve_for(target, false),
                vec![(FG, "#445566".to_string())]
            );
        }
    }

    #[test]
    fn colors_named_short_hex_and_rgb_normalize() {
        let sheet = parse(".a { background: red; color: #0f0; border-color: rgb(0, 0, 255) }");
        let decls = sheet.declarations("a");
        assert!(decls.contains(&(BG, "#ff0000".to_string())), "named color");
        assert!(
            decls.contains(&(FG, "#00ff00".to_string())),
            "#rgb expanded"
        );
        assert!(
            decls.contains(&(BORDER_COLOR, "#0000ff".to_string())),
            "rgb() converted"
        );
    }

    #[test]
    fn new_box_props_map_and_strip_px() {
        let sheet = parse(
            ".a { margin:8px; min-width:40px; max-width:200px; flex-grow:1; border-width:2px }",
        );
        let decls = sheet.declarations("a");
        assert!(decls.contains(&(MARGIN, "8".to_string())));
        assert!(decls.contains(&(MIN_WIDTH, "40".to_string())));
        assert!(decls.contains(&(MAX_WIDTH, "200".to_string())));
        assert!(decls.contains(&(FLEX_GROW, "1".to_string())));
        assert!(decls.contains(&(BORDER_WIDTH, "2".to_string())));
    }

    // --- Shorthand + per-side expansion ------------------------------------

    /// Look up the value a declaration set carries for `prop` (`None` if absent).
    fn val(decls: &[Decl], prop: PropId) -> Option<&str> {
        decls
            .iter()
            .find(|(p, _)| *p == prop)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn margin_one_value_keeps_uniform_margin() {
        // The single-value form keeps the historical uniform MARGIN PropId unchanged.
        let sheet = parse(".a { margin: 8px }");
        assert_eq!(sheet.declarations("a"), &[(MARGIN, "8".to_string())]);
    }

    #[test]
    fn margin_two_values_expand_top_bottom_and_right_left() {
        // `margin: a b` -> top=bottom=a, right=left=b.
        let sheet = parse(".a { margin: 8 16 }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, MARGIN_TOP), Some("8"));
        assert_eq!(val(d, MARGIN_BOTTOM), Some("8"));
        assert_eq!(val(d, MARGIN_RIGHT), Some("16"));
        assert_eq!(val(d, MARGIN_LEFT), Some("16"));
        assert_eq!(
            val(d, MARGIN),
            None,
            "uniform MARGIN not emitted for multi-value"
        );
    }

    #[test]
    fn margin_three_values_expand_top_sides_bottom() {
        // `margin: a b c` -> top=a, right=left=b, bottom=c.
        let sheet = parse(".a { margin: 1px 2px 3px }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, MARGIN_TOP), Some("1"));
        assert_eq!(val(d, MARGIN_RIGHT), Some("2"));
        assert_eq!(val(d, MARGIN_LEFT), Some("2"));
        assert_eq!(val(d, MARGIN_BOTTOM), Some("3"));
    }

    #[test]
    fn margin_four_values_expand_each_side_clockwise() {
        // `margin: a b c d` -> top, right, bottom, left (clockwise).
        let sheet = parse(".a { margin: 1 2 3 4 }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, MARGIN_TOP), Some("1"));
        assert_eq!(val(d, MARGIN_RIGHT), Some("2"));
        assert_eq!(val(d, MARGIN_BOTTOM), Some("3"));
        assert_eq!(val(d, MARGIN_LEFT), Some("4"));
    }

    #[test]
    fn padding_shorthand_expands_per_side() {
        // Single value keeps uniform PADDING; multi-value expands to the per-side longhands.
        let one = parse(".a { padding: 5px }");
        assert_eq!(one.declarations("a"), &[(PADDING, "5".to_string())]);
        let two = parse(".b { padding: 4 8 }");
        let d = two.declarations("b");
        assert_eq!(val(d, PADDING_TOP), Some("4"));
        assert_eq!(val(d, PADDING_BOTTOM), Some("4"));
        assert_eq!(val(d, PADDING_RIGHT), Some("8"));
        assert_eq!(val(d, PADDING_LEFT), Some("8"));
    }

    #[test]
    fn inset_shorthand_sets_all_four_sides_even_for_one_value() {
        // `inset` has no uniform PropId, so even a single value sets all four INSET_* sides.
        let sheet = parse(".a { inset: 0 }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, INSET_TOP), Some("0"));
        assert_eq!(val(d, INSET_RIGHT), Some("0"));
        assert_eq!(val(d, INSET_BOTTOM), Some("0"));
        assert_eq!(val(d, INSET_LEFT), Some("0"));
    }

    #[test]
    fn top_right_bottom_left_map_to_inset_sides() {
        let sheet = parse(".a { top: 1px; right: 2px; bottom: 3px; left: 4px }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, INSET_TOP), Some("1"));
        assert_eq!(val(d, INSET_RIGHT), Some("2"));
        assert_eq!(val(d, INSET_BOTTOM), Some("3"));
        assert_eq!(val(d, INSET_LEFT), Some("4"));
    }

    #[test]
    fn gap_one_value_uniform_two_values_split_axes() {
        let one = parse(".a { gap: 8px }");
        assert_eq!(one.declarations("a"), &[(GAP, "8".to_string())]);
        let two = parse(".b { gap: 4 12 }");
        let d = two.declarations("b");
        assert_eq!(val(d, ROW_GAP), Some("4"));
        assert_eq!(val(d, COLUMN_GAP), Some("12"));
        assert_eq!(val(d, GAP), None);
    }

    #[test]
    fn border_shorthand_splits_width_style_color_any_order() {
        let sheet = parse(".a { border: 2px solid red } .b { border: red dashed 3 }");
        let a = sheet.declarations("a");
        assert_eq!(val(a, BORDER_WIDTH), Some("2"));
        assert_eq!(val(a, BORDER_STYLE), Some("solid"));
        assert_eq!(val(a, BORDER_COLOR), Some("#ff0000"));
        // Order-tolerant: color first, then style, then width.
        let b = sheet.declarations("b");
        assert_eq!(val(b, BORDER_WIDTH), Some("3"));
        assert_eq!(val(b, BORDER_STYLE), Some("dashed"));
        assert_eq!(val(b, BORDER_COLOR), Some("#ff0000"));
    }

    #[test]
    fn flex_shorthand_grow_shrink_basis() {
        let g = parse(".a { flex: 1 }");
        assert_eq!(g.declarations("a"), &[(FLEX_GROW, "1".to_string())]);
        let gs = parse(".b { flex: 1 0 }");
        let d = gs.declarations("b");
        assert_eq!(val(d, FLEX_GROW), Some("1"));
        assert_eq!(val(d, FLEX_SHRINK), Some("0"));
        let gsb = parse(".c { flex: 1 0 auto }");
        let d = gsb.declarations("c");
        assert_eq!(val(d, FLEX_GROW), Some("1"));
        assert_eq!(val(d, FLEX_SHRINK), Some("0"));
        assert_eq!(
            val(d, FLEX_BASIS),
            Some("auto"),
            "auto basis passes through"
        );
    }

    #[test]
    fn outline_shorthand_keeps_width_and_color_ignores_style() {
        let sheet = parse(".a { outline: 1px solid blue }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, OUTLINE_WIDTH), Some("1"));
        assert_eq!(val(d, OUTLINE_COLOR), Some("#0000ff"));
        // The `solid` style token is recognized but not mapped to a PropId for now.
        assert_eq!(d.len(), 2, "only width + color, no style decl");
    }

    // --- Color: alpha + transparent ----------------------------------------

    #[test]
    fn rgba_normalizes_to_rrggbbaa() {
        // alpha 0.5 -> 128 (0x80); opaque alpha 1 collapses to #rrggbb.
        let sheet = parse(".a { color: rgba(255, 0, 0, 0.5) } .b { color: rgba(0,0,255,1) }");
        assert_eq!(sheet.declarations("a"), &[(FG, "#ff000080".to_string())]);
        assert_eq!(sheet.declarations("b"), &[(FG, "#0000ff".to_string())]);
    }

    #[test]
    fn short_hex_with_alpha_expands() {
        // `#rgba` -> `#rrggbbaa`; an opaque `f` alpha collapses to `#rrggbb`.
        let sheet = parse(".a { background: #f008 } .b { background: #0f0f }");
        assert_eq!(sheet.declarations("a"), &[(BG, "#ff000088".to_string())]);
        assert_eq!(sheet.declarations("b"), &[(BG, "#00ff00".to_string())]);
    }

    #[test]
    fn eight_digit_hex_passes_through_and_opaque_collapses() {
        let sheet = parse(".a { background: #11223344 } .b { background: #112233ff }");
        assert_eq!(sheet.declarations("a"), &[(BG, "#11223344".to_string())]);
        assert_eq!(sheet.declarations("b"), &[(BG, "#112233".to_string())]);
    }

    #[test]
    fn transparent_normalizes_to_fully_transparent_black() {
        let sheet = parse(".a { background: transparent }");
        assert_eq!(sheet.declarations("a"), &[(BG, "#00000000".to_string())]);
    }

    #[test]
    fn opaque_colors_stay_byte_stable() {
        // The opaque normalization path must not change: 6-digit, #rgb, rgb(), and named all fold
        // to the same `#rrggbb` form they always did.
        let sheet = parse(
            ".a { background: #313244 } .b { color: #0f0 } .c { border-color: rgb(0,0,255) } .d { background: navy }",
        );
        assert_eq!(sheet.declarations("a"), &[(BG, "#313244".to_string())]);
        assert_eq!(sheet.declarations("b"), &[(FG, "#00ff00".to_string())]);
        assert_eq!(
            sheet.declarations("c"),
            &[(BORDER_COLOR, "#0000ff".to_string())]
        );
        assert_eq!(sheet.declarations("d"), &[(BG, "#000080".to_string())]);
    }

    // --- Robustness: !important / inherit ----------------------------------

    #[test]
    fn important_is_stripped_not_dropped() {
        // `!important` must not drop the declaration; the value resolves normally.
        let sheet = parse(".a { color: red !important; padding: 8px !important }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, FG), Some("#ff0000"));
        assert_eq!(val(d, PADDING), Some("8"));
    }

    #[test]
    fn css_wide_keywords_drop_their_declaration_cleanly() {
        // inherit/initial/unset have no lite semantics yet: each drops its own declaration, leaving
        // the sibling declarations intact (and never feeding a bogus value into a color/number parse).
        let sheet =
            parse(".a { color: inherit; background: #111111; padding: initial; gap: unset }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, FG), None, "inherit dropped");
        assert_eq!(val(d, PADDING), None, "initial dropped");
        assert_eq!(val(d, GAP), None, "unset dropped");
        assert_eq!(val(d, BG), Some("#111111"), "sibling declaration survives");
    }

    // --- Specificity overflow + case-insensitivity -------------------------

    #[test]
    fn specificity_does_not_overflow_at_eleven_classes() {
        // 11 classes (b=11) must still lose to a single id (a=1). The old packed `a*100 + b*10`
        // would score the 11-class rule at 110, tying/beating the id at 100 and mis-ordering the
        // cascade; the (a, b, c) tuple keeps `id > any number of classes`.
        let many = ".c1.c2.c3.c4.c5.c6.c7.c8.c9.c10.c11 { background:#222222 }";
        let one_id = "#x { background:#111111 }";
        let mut css = String::new();
        css.push_str(many);
        css.push(' ');
        css.push_str(one_id);
        let sheet = parse(&css);
        let target = MatchTarget {
            type_name: None,
            id: Some("x"),
            classes: &[
                "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11",
            ],
        };
        // The id rule wins despite appearing later and having far fewer simple selectors.
        assert_eq!(
            sheet.resolve_for(&target, false),
            vec![(BG, "#111111".to_string())]
        );
    }

    #[test]
    fn type_name_match_is_ascii_case_insensitive() {
        // A `BUTTON` selector matches a `<button>` element (HTML tag names are case-insensitive).
        let sheet = parse("BUTTON { background:#abcdef }");
        let target = MatchTarget {
            type_name: Some("button"),
            id: None,
            classes: &[],
        };
        assert_eq!(
            sheet.resolve_for(&target, false),
            vec![(BG, "#abcdef".to_string())]
        );
    }

    #[test]
    fn class_match_stays_case_sensitive() {
        // Classes remain case-sensitive per CSS: `.Btn` does not match the `btn` class.
        let sheet = parse(".Btn { background:#abcdef }");
        let target = MatchTarget {
            type_name: None,
            id: None,
            classes: &["btn"],
        };
        assert!(sheet.resolve_for(&target, false).is_empty());
    }

    #[test]
    fn hover_pseudo_is_case_insensitive() {
        let sheet = parse(".btn:HOVER { background:#585b70 }");
        assert_eq!(
            sheet.resolve(&["btn"], true),
            vec![(BG, "#585b70".to_string())]
        );
        assert!(sheet.resolve(&["btn"], false).is_empty(), "base unaffected");
    }

    // --- New keyword + numeric props ---------------------------------------

    #[test]
    fn display_none_and_block_alias_to_flex() {
        let none = parse(".a { display: none }");
        assert_eq!(none.declarations("a"), &[(DISPLAY, "none".to_string())]);
        // `block` is the lite alias for `flex`.
        let block = parse(".b { display: block }");
        assert_eq!(block.declarations("b"), &[(DISPLAY, "flex".to_string())]);
        let flex = parse(".c { display: flex }");
        assert_eq!(flex.declarations("c"), &[(DISPLAY, "flex".to_string())]);
    }

    #[test]
    fn position_and_visibility_and_overflow_pass_through() {
        let sheet = parse(".a { position: absolute; visibility: hidden; overflow: scroll }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, POSITION), Some("absolute"));
        assert_eq!(val(d, VISIBILITY), Some("hidden"));
        assert_eq!(val(d, OVERFLOW), Some("scroll"));
    }

    #[test]
    fn flex_wrap_and_align_self_and_box_sizing_pass_through() {
        let sheet =
            parse(".a { flex-wrap: wrap-reverse; align-self: center; box-sizing: border-box }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, FLEX_WRAP), Some("wrap-reverse"));
        assert_eq!(val(d, ALIGN_SELF), Some("center"));
        assert_eq!(val(d, BOX_SIZING), Some("border-box"));
    }

    #[test]
    fn font_weight_keywords_fold_to_numbers() {
        let sheet =
            parse(".a { font-weight: bold } .b { font-weight: normal } .c { font-weight: 600 }");
        assert_eq!(sheet.declarations("a"), &[(FONT_WEIGHT, "700".to_string())]);
        assert_eq!(sheet.declarations("b"), &[(FONT_WEIGHT, "400".to_string())]);
        assert_eq!(sheet.declarations("c"), &[(FONT_WEIGHT, "600".to_string())]);
    }

    #[test]
    fn font_and_text_length_props_strip_px() {
        let sheet = parse(".a { font-size: 14px; line-height: 20px; flex-basis: 120px }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, FONT_SIZE), Some("14"));
        assert_eq!(val(d, LINE_HEIGHT), Some("20"));
        assert_eq!(val(d, FLEX_BASIS), Some("120"));
    }

    #[test]
    fn flex_basis_percent_and_auto_round_trip() {
        let pct = parse(".a { flex-basis: 50% }");
        assert_eq!(pct.declarations("a"), &[(FLEX_BASIS, "50%".to_string())]);
        let auto = parse(".b { flex-basis: auto }");
        assert_eq!(auto.declarations("b"), &[(FLEX_BASIS, "auto".to_string())]);
    }

    #[test]
    fn negative_inset_and_outline_offset_preserve_sign() {
        let sheet = parse(".a { top: -4px; outline-offset: -2px; margin-left: -8 }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, INSET_TOP), Some("-4"));
        assert_eq!(val(d, OUTLINE_OFFSET), Some("-2"));
        assert_eq!(val(d, MARGIN_LEFT), Some("-8"));
    }

    #[test]
    fn unitless_props_pass_through() {
        let sheet = parse(".a { z-index: 10; flex-shrink: 0; aspect-ratio: 16/9 }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, Z_INDEX), Some("10"));
        assert_eq!(val(d, FLEX_SHRINK), Some("0"));
        assert_eq!(val(d, ASPECT_RATIO), Some("16/9"));
    }

    #[test]
    fn box_shadow_and_background_image_normalize_to_canonical_form() {
        // `box-shadow` -> `<dx> <dy> <blur> <#hex>` (px stripped, rgba alpha folded to #rrggbbaa).
        // `background-image` -> `linear-gradient(<deg>, <#hex>...)` (named colors normalized,
        // default direction 180 when none is given).
        let sheet = parse(
            ".a { box-shadow: 0 2px 4px rgba(0,0,0,0.3) } .b { background-image: linear-gradient(red, blue) }",
        );
        assert_eq!(
            sheet.declarations("a"),
            &[(BOX_SHADOW, "0 2 4 #0000004d".to_string())]
        );
        assert_eq!(
            sheet.declarations("b"),
            &[(
                BACKGROUND_IMAGE,
                "linear-gradient(180, #ff0000, #0000ff)".to_string()
            )]
        );
    }

    // --- background-image: linear-gradient normalization -------------------

    #[test]
    fn gradient_to_right_folds_direction_to_90_degrees() {
        let sheet = parse(".a { background-image: linear-gradient(to right, #89b4fa, #b4caff) }");
        assert_eq!(
            sheet.declarations("a"),
            &[(
                BACKGROUND_IMAGE,
                "linear-gradient(90, #89b4fa, #b4caff)".to_string()
            )]
        );
    }

    #[test]
    fn gradient_default_direction_is_180() {
        // No direction argument -> default `to bottom` (180deg).
        let sheet = parse(".a { background-image: linear-gradient(#89b4fa, #b4caff) }");
        assert_eq!(
            sheet.declarations("a"),
            &[(
                BACKGROUND_IMAGE,
                "linear-gradient(180, #89b4fa, #b4caff)".to_string()
            )]
        );
    }

    #[test]
    fn gradient_side_keywords_map_to_their_degrees() {
        // to top -> 0, to bottom -> 180, to left -> 270, to right -> 90.
        let sheet = parse(
            ".t { background-image: linear-gradient(to top, #000000, #ffffff) } \
             .b { background-image: linear-gradient(to bottom, #000000, #ffffff) } \
             .l { background-image: linear-gradient(to left, #000000, #ffffff) }",
        );
        assert_eq!(
            val(sheet.declarations("t"), BACKGROUND_IMAGE),
            Some("linear-gradient(0, #000000, #ffffff)")
        );
        assert_eq!(
            val(sheet.declarations("b"), BACKGROUND_IMAGE),
            Some("linear-gradient(180, #000000, #ffffff)")
        );
        assert_eq!(
            val(sheet.declarations("l"), BACKGROUND_IMAGE),
            Some("linear-gradient(270, #000000, #ffffff)")
        );
    }

    #[test]
    fn gradient_bare_deg_angle_keeps_value_mod_360() {
        let plain = parse(".a { background-image: linear-gradient(45deg, red, blue) }");
        assert_eq!(
            val(plain.declarations("a"), BACKGROUND_IMAGE),
            Some("linear-gradient(45, #ff0000, #0000ff)")
        );
        // A bare number (no `deg`) is also accepted, and an angle past 360 wraps.
        let wrap = parse(".b { background-image: linear-gradient(450, red, blue) }");
        assert_eq!(
            val(wrap.declarations("b"), BACKGROUND_IMAGE),
            Some("linear-gradient(90, #ff0000, #0000ff)")
        );
    }

    #[test]
    fn gradient_three_stops_all_normalize() {
        let sheet =
            parse(".a { background-image: linear-gradient(to right, #89b4fa, #b4caff, #cba6f7) }");
        assert_eq!(
            sheet.declarations("a"),
            &[(
                BACKGROUND_IMAGE,
                "linear-gradient(90, #89b4fa, #b4caff, #cba6f7)".to_string()
            )]
        );
    }

    #[test]
    fn gradient_rgba_stop_carries_alpha_as_rrggbbaa() {
        // An rgba() stop with alpha < 1 folds to the 8-digit `#rrggbbaa` form.
        let sheet = parse(
            ".a { background-image: linear-gradient(to right, rgba(137,180,250,0.5), #b4caff) }",
        );
        assert_eq!(
            sheet.declarations("a"),
            &[(
                BACKGROUND_IMAGE,
                "linear-gradient(90, #89b4fa80, #b4caff)".to_string()
            )]
        );
    }

    #[test]
    fn gradient_drops_per_stop_position_percentages() {
        // A per-stop position (`#fff 20%`) is dropped — only the color is kept for now.
        let sheet =
            parse(".a { background-image: linear-gradient(to right, #89b4fa 0%, #b4caff 100%) }");
        assert_eq!(
            sheet.declarations("a"),
            &[(
                BACKGROUND_IMAGE,
                "linear-gradient(90, #89b4fa, #b4caff)".to_string()
            )]
        );
    }

    #[test]
    fn gradient_caps_at_eight_stops() {
        // Nine stops -> the consumer's inline cap keeps the first eight.
        let sheet = parse(
            ".a { background-image: linear-gradient(to right, #010101, #020202, #030303, #040404, #050505, #060606, #070707, #080808, #090909) }",
        );
        let v = val(sheet.declarations("a"), BACKGROUND_IMAGE).unwrap();
        assert_eq!(
            v,
            "linear-gradient(90, #010101, #020202, #030303, #040404, #050505, #060606, #070707, #080808)"
        );
    }

    #[test]
    fn gradient_with_a_bad_stop_drops_the_declaration() {
        // `notacolor` doesn't resolve to a hex color -> the whole gradient is dropped cleanly,
        // and a sibling declaration survives.
        let sheet = parse(
            ".a { background-image: linear-gradient(to right, #89b4fa, notacolor); background: #111111 }",
        );
        let d = sheet.declarations("a");
        assert_eq!(val(d, BACKGROUND_IMAGE), None, "bad gradient dropped");
        assert_eq!(val(d, BG), Some("#111111"), "sibling survives");
    }

    #[test]
    fn non_gradient_background_image_is_dropped() {
        // A non-`linear-gradient` value (e.g. `url(...)`) isn't supported -> dropped cleanly.
        let sheet = parse(".a { background-image: url(pic.png) }");
        assert!(sheet.declarations("a").is_empty());
    }

    // --- box-shadow normalization ------------------------------------------

    #[test]
    fn box_shadow_color_last_form_normalizes() {
        // `0 4px 12px rgba(0,0,0,0.25)` -> `0 4 12 #00000040` (px stripped, alpha 0.25 -> 0x40).
        let sheet = parse(".a { box-shadow: 0 4px 12px rgba(0,0,0,0.25) }");
        assert_eq!(
            sheet.declarations("a"),
            &[(BOX_SHADOW, "0 4 12 #00000040".to_string())]
        );
    }

    #[test]
    fn box_shadow_color_first_form_normalizes() {
        // The color may appear before the lengths.
        let sheet = parse(".a { box-shadow: #000000 2px 4px 6px }");
        assert_eq!(
            sheet.declarations("a"),
            &[(BOX_SHADOW, "2 4 6 #000000".to_string())]
        );
    }

    #[test]
    fn box_shadow_blur_omitted_defaults_to_zero() {
        // Only dx + dy given -> blur defaults to 0.
        let sheet = parse(".a { box-shadow: 3px 5px red }");
        assert_eq!(
            sheet.declarations("a"),
            &[(BOX_SHADOW, "3 5 0 #ff0000".to_string())]
        );
    }

    #[test]
    fn box_shadow_drops_spread_and_inset() {
        // A 4th length (spread) and the `inset` keyword are dropped from the canonical form.
        let spread = parse(".a { box-shadow: 1px 2px 3px 4px #000000 }");
        assert_eq!(
            spread.declarations("a"),
            &[(BOX_SHADOW, "1 2 3 #000000".to_string())]
        );
        let inset = parse(".b { box-shadow: inset 1px 2px 3px #000000 }");
        assert_eq!(
            inset.declarations("b"),
            &[(BOX_SHADOW, "1 2 3 #000000".to_string())]
        );
    }

    #[test]
    fn box_shadow_negative_offsets_preserve_sign() {
        let sheet = parse(".a { box-shadow: -2px -4px 6px #000000 }");
        assert_eq!(
            sheet.declarations("a"),
            &[(BOX_SHADOW, "-2 -4 6 #000000".to_string())]
        );
    }

    #[test]
    fn box_shadow_without_a_color_is_dropped() {
        // No color token -> the seam can't draw a shadow, so the declaration is dropped cleanly.
        let sheet = parse(".a { box-shadow: 2px 4px 6px; background: #111111 }");
        let d = sheet.declarations("a");
        assert_eq!(val(d, BOX_SHADOW), None, "colorless shadow dropped");
        assert_eq!(val(d, BG), Some("#111111"), "sibling survives");
    }

    #[test]
    fn box_shadow_with_too_few_lengths_is_dropped() {
        // A single length (only dx, no dy) is malformed -> dropped.
        let sheet = parse(".a { box-shadow: 2px #000000 }");
        assert!(sheet.declarations("a").is_empty());
    }

    #[test]
    fn border_style_and_per_side_and_radius_longhands_map() {
        let sheet = parse(
            ".a { border-style: dotted; border-top-width: 2px; border-left-color: red; border-top-left-radius: 6px }",
        );
        let d = sheet.declarations("a");
        assert_eq!(val(d, BORDER_STYLE), Some("dotted"));
        assert_eq!(val(d, BORDER_TOP_WIDTH), Some("2"));
        assert_eq!(val(d, BORDER_LEFT_COLOR), Some("#ff0000"));
        assert_eq!(val(d, BORDER_TOP_LEFT_RADIUS), Some("6"));
    }

    #[test]
    fn text_decoration_aliases_and_keyword_passthrough() {
        let a = parse(".a { text-decoration: underline }");
        assert_eq!(
            a.declarations("a"),
            &[(TEXT_DECORATION, "underline".to_string())]
        );
        let b = parse(".b { text-decoration-line: line-through }");
        assert_eq!(
            b.declarations("b"),
            &[(TEXT_DECORATION, "line-through".to_string())]
        );
    }

    #[test]
    fn unknown_pseudo_class_rule_is_dropped() {
        // `:focus` is outside the subset: the whole rule is dropped, and it must not
        // be mistaken for a base `.btn` rule.
        let sheet = parse(".btn:focus { background:#000000 } .btn { background:#313244 }");
        assert_eq!(sheet.declarations("btn"), &[(BG, "#313244".to_string())]);
        assert_eq!(
            sheet.resolve(&["btn"], true),
            vec![(BG, "#313244".to_string())]
        );
    }

    #[test]
    fn apply_state_writes_hover_then_base_onto_the_node() {
        let sheet = parse(HOVER_CSS);
        let app = App::new();
        let node = app.el(COLUMN);
        app.append(NodeId::new(0), node);

        // Hover on: background becomes the hover color, padding stays.
        sheet.apply_state(&app, node, &["btn"], true);
        let mut dom = Dom::new();
        dom.apply(&app.take_batch(0)).unwrap();
        assert_eq!(dom.style(node, BG), Some("#585b70"));
        assert_eq!(dom.style(node, PADDING), Some("5"));

        // Hover off: background reverts to the base color.
        sheet.apply_state(&app, node, &["btn"], false);
        dom.apply(&app.take_batch(0)).unwrap();
        assert_eq!(dom.style(node, BG), Some("#313244"));
        assert_eq!(dom.style(node, PADDING), Some("5"));
    }

    // --- LiteEngine: the constrained-tier StyleEngine ----------------------

    /// Build a `Dom` carrying class identity (capable-style ops), as a host would.
    fn dom_with(build: impl FnOnce(&mut canopy_core::Emitter)) -> Dom {
        let mut e = canopy_core::Emitter::new();
        build(&mut e);
        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();
        dom
    }

    #[test]
    fn lite_engine_resolves_classes_to_computed_style() {
        use canopy_protocol::ElementTag;
        let (mut card, mut title) = (NodeId::new(0), NodeId::new(0));
        let dom = dom_with(|e| {
            card = e.create_element(ElementTag::new(1));
            e.append(canopy_dom::ROOT, card);
            e.set_class(card, "card");
            title = e.create_element(ElementTag::new(1));
            e.append(card, title);
            e.set_class(title, "title");
        });

        let css = ".card { background: #1c2030; padding: 14 } .title { color: #e8eaf0 }";
        let mut engine = LiteEngine::from_dom(&dom, css);

        let card_s = engine.resolve(card, None).unwrap();
        assert_eq!(
            card_s.background,
            Color {
                r: 0x1c,
                g: 0x20,
                b: 0x30,
                a: 255
            }
        );
        assert_eq!(card_s.padding, 14.0);

        let title_s = engine.resolve(title, None).unwrap();
        assert_eq!(
            title_s.color,
            Color {
                r: 0xe8,
                g: 0xea,
                b: 0xf0,
                a: 255
            }
        );
    }

    #[test]
    fn lite_engine_honors_parent_inheritance() {
        use canopy_protocol::ElementTag;
        // A child whose class sets only padding must inherit color + font-size from the
        // parent style, per the StyleEngine::resolve contract (this reduced resolver has
        // no internal tree inheritance).
        let mut child = NodeId::new(0);
        let dom = dom_with(|e| {
            let parent = e.create_element(ElementTag::new(1));
            e.append(canopy_dom::ROOT, parent);
            child = e.create_element(ElementTag::new(1));
            e.append(parent, child);
            e.set_class(child, "plain");
        });

        let mut engine = LiteEngine::from_dom(&dom, ".plain { padding: 4 }");
        let parent_style = ComputedStyle {
            color: Color {
                r: 10,
                g: 20,
                b: 30,
                a: 255,
            },
            font_size: 22.0,
            ..ComputedStyle::default()
        };
        let s = engine.resolve(child, Some(&parent_style)).unwrap();
        assert_eq!(s.color, parent_style.color, "color inherited from parent");
        assert_eq!(s.font_size, 22.0, "font-size inherited from parent");
        assert_eq!(s.padding, 4.0, "own padding still applies");
    }

    #[test]
    fn lite_engine_defaults_to_css_initials_without_a_parent() {
        // No parent and no matching rule: the node gets the CSS initial color
        // (opaque black) and font-size (16), not the all-zero ComputedStyle default.
        use canopy_protocol::ElementTag;
        let mut node = NodeId::new(0);
        let dom = dom_with(|e| {
            node = e.create_element(ElementTag::new(1));
            e.append(canopy_dom::ROOT, node);
            e.set_class(node, "unstyled");
        });
        let mut engine = LiteEngine::from_dom(&dom, ".other { color: #ffffff }");
        let s = engine.resolve(node, None).unwrap();
        assert_eq!(s.color, INITIAL_COLOR);
        assert_eq!(s.font_size, INITIAL_FONT_SIZE);
        assert_eq!(s.opacity, 1.0);
    }

    #[test]
    fn lite_engine_rejects_an_unknown_handle() {
        let dom = Dom::new();
        let mut engine = LiteEngine::from_dom(&dom, ".x { color: #fff }");
        assert!(engine.resolve(NodeId::new(999), None).is_err());
    }
}
