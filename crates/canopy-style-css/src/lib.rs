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
//! `color`, `width`/`height`, `min-width`/`min-height`/`max-width`/`max-height`,
//! `margin`, `padding`, `gap`, `flex-grow`, `border-width`, `border-color`,
//! `radius`, `opacity`, `direction`, `align`, `justify`, `text-align`, and the
//! `translate-x`/`translate-y` offsets. Lengths accept a bare number or a `px`
//! suffix. Colors accept a named keyword (`navy`, `red`, …), `#rgb`, `#rrggbb`, or
//! `rgb(r, g, b)` — all normalized to `#rrggbb` (`transparent` is intentionally
//! absent, so it falls through to paint-nothing).
//!
//! # What this is *not*
//!
//! This is a deliberate subset, not a full CSS engine:
//!
//! - Selectors are a single compound only — no **descendant/child/sibling
//!   combinators**, no attribute selectors, and no pseudo-classes beyond `:hover`;
//!   no media queries, `!important`, or shorthand expansion.
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
    ALIGN, BG, BORDER_COLOR, BORDER_WIDTH, DIRECTION, FG, FLEX_GROW, GAP, HEIGHT, JUSTIFY, MARGIN,
    MAX_HEIGHT, MAX_WIDTH, MIN_HEIGHT, MIN_WIDTH, OPACITY, PADDING, RADIUS, TEXT_ALIGN,
    TRANSLATE_X, TRANSLATE_Y, WIDTH,
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

/// A compound selector plus its pseudo-state and CSS **specificity**. Specificity is
/// `ids*100 + (classes + pseudos)*10 + types*1`, the standard `(a, b, c)` collapsed to one
/// number (no part exceeds 99 for any realistic lite stylesheet); ties break on source order.
struct Selector {
    simples: Vec<Simple>,
    state: State,
    specificity: u32,
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
        // cascade correctly regardless of the order classes appear on the element.
        let mut matched: Vec<(u32, usize)> = Vec::new();
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
        Some((compound, "hover")) => (compound, State::Hover),
        Some(_) => return None, // unsupported pseudo-class -> drop this selector
        None => (sel, State::Base),
    };
    let simples = parse_compound(compound)?;
    let (mut ids, mut tens, mut types) = (0u32, 0u32, 0u32);
    for simple in &simples {
        match simple {
            Simple::Id(_) => ids += 1,
            Simple::Class(_) => tens += 1,
            Simple::Type(_) => types += 1,
        }
    }
    if state == State::Hover {
        tens += 1; // a pseudo-class counts at the class level of specificity
    }
    Some(Selector {
        simples,
        state,
        specificity: ids * 100 + tens * 10 + types,
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
fn selector_matches(simples: &[Simple], target: &MatchTarget) -> bool {
    simples.iter().all(|simple| match simple {
        Simple::Type(t) => target.type_name == Some(t.as_str()),
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
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let Some(prop) = map_property(name) else {
            continue;
        };
        decls.push((prop, normalize_value(prop, value)));
    }
    decls
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
        // Box model: outer margin + min/max sizing (all px lengths).
        "margin" => Some(MARGIN),
        "min-width" => Some(MIN_WIDTH),
        "min-height" => Some(MIN_HEIGHT),
        "max-width" => Some(MAX_WIDTH),
        "max-height" => Some(MAX_HEIGHT),
        // Flex grow factor (unitless) + a border frame (width px + color).
        "flex-grow" => Some(FLEX_GROW),
        "border-width" => Some(BORDER_WIDTH),
        "border-color" => Some(BORDER_COLOR),
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
    // Colors (background / color / border-color): expand to `#rrggbb` so the renderers' hex
    // `parse_color` accepts named colors, `#rgb`, and `rgb()` too. An unrecognized value is left
    // verbatim (the renderer then ignores it — e.g. `background: transparent` paints nothing).
    if prop == BG || prop == FG || prop == BORDER_COLOR {
        return normalize_color(value);
    }
    let is_length = prop == WIDTH
        || prop == HEIGHT
        || prop == GAP
        || prop == PADDING
        || prop == RADIUS
        || prop == TRANSLATE_X
        || prop == TRANSLATE_Y
        || prop == MARGIN
        || prop == MIN_WIDTH
        || prop == MIN_HEIGHT
        || prop == MAX_WIDTH
        || prop == MAX_HEIGHT
        || prop == BORDER_WIDTH;
    if is_length {
        if let Some(num) = value.strip_suffix("px") {
            return num.trim().to_string();
        }
    }
    value.to_string()
}

/// Normalize a CSS color to `#rrggbb`: a 6-digit hex passes through, `#rgb` expands, `rgb(r,g,b)`
/// / `rgb(r g b)` converts, and a CSS named color maps via a small table. An unrecognized value is
/// returned verbatim so the renderer's `parse_color` simply rejects it (no paint).
fn normalize_color(value: &str) -> String {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return value.to_string();
        }
        if hex.len() == 3 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let mut out = String::with_capacity(7);
            out.push('#');
            for ch in hex.chars() {
                out.push(ch); // `#abc` -> `#aabbcc`: each nibble doubled
                out.push(ch);
            }
            return out;
        }
    }
    if let Some(inner) = value.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let mut chans = inner
            .split([',', ' ', '/'])
            .filter(|p| !p.trim().is_empty());
        if let (Some(r), Some(g), Some(b), None) =
            (chans.next(), chans.next(), chans.next(), chans.next())
        {
            if let (Ok(r), Ok(g), Ok(b)) = (
                r.trim().parse::<u8>(),
                g.trim().parse::<u8>(),
                b.trim().parse::<u8>(),
            ) {
                let mut out = String::with_capacity(7);
                out.push('#');
                push_hex_byte(&mut out, r);
                push_hex_byte(&mut out, g);
                push_hex_byte(&mut out, b);
                return out;
            }
        }
    }
    if let Some(hex) = named_color(value) {
        return hex.to_string();
    }
    value.to_string()
}

/// Append `byte` as two lowercase hex digits (no `format!`, to stay `no_std`-clean).
fn push_hex_byte(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(HEX[(byte >> 4) as usize] as char);
    out.push(HEX[(byte & 0x0f) as usize] as char);
}

/// Map a CSS named color (case-insensitive) to its `#rrggbb` hex. The 16 HTML basic colors plus a
/// handful of common extras. `transparent` is intentionally absent — the lite `#rrggbb` color has
/// no alpha, so it falls through to verbatim and paints nothing (the right result for a background).
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
    ];
    table.iter().find(|(kw, _)| eq(kw)).map(|(_, hex)| *hex)
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
        let sheet = parse(".x { background: #fff; border: 1px; outline: none }");
        // Only `background` maps; the `border` shorthand and `outline` are outside this subset
        // and skipped (`border-width`/`border-color` ARE mapped, but the `border` shorthand is
        // not). The `#fff` value is normalized to the 6-digit `#ffffff` form.
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
