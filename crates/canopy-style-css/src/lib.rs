//! Canopy CSS-lite: a tiny, dependency-free CSS subset that lets authors style
//! with **class rules** instead of per-node inline calls.
//!
//! A stylesheet is a sequence of class rules:
//!
//! ```text
//! .name  { prop: value; prop: value }
//! .other { prop: value }
//! ```
//!
//! [`parse`] turns that string into a [`Stylesheet`]. Each declaration's property
//! *name* is mapped to the matching [`canopy_paint`] [`PropId`] const and its value
//! is normalized (a trailing `px` is stripped, colors and directions pass through),
//! so the resolved pairs feed the **existing inline-style path unchanged**:
//! [`Stylesheet::apply`] simply replays them through [`canopy_view::App::style`].
//!
//! # Selectors and the `:hover` cascade
//!
//! The only selector is a single class, optionally with a `:hover` pseudo-class:
//!
//! ```text
//! .btn       { background: #313244 }
//! .btn:hover { background: #585b70 }
//! ```
//!
//! A selector parses into `(class, Option<state>)`. Base rules (no state) always
//! apply; `:hover` rules apply only when a node is hovered. [`Stylesheet::resolve`]
//! is the **cascade resolver**: for each class in the list, in order, it appends the
//! class's base declarations and then (when `hovered`) its `:hover` declarations,
//! with **last-wins** semantics on each [`PropId`] — a later rule overrides an
//! earlier one on the same property, while properties no rule touches are preserved.
//! [`Stylesheet::apply_state`] replays that resolution onto an [`App`]; the host
//! re-calls it whenever a node's hover state flips.
//!
//! # What this is *not*
//!
//! This is a deliberate subset, not a CSS engine:
//!
//! - The only selectors are a bare class (`.name`) and `.name:hover`. No element,
//!   id, descendant, or compound selectors; no pseudo-classes beyond `:hover`; no
//!   media queries.
//! - There is **no cascade across the tree** and no specificity. [`apply`] and
//!   [`apply_state`] expand a node's classes into inline-style ops on *that node
//!   only*; "later overrides earlier" applies within the class list you pass,
//!   exactly like writing those inline styles by hand in that order.
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
    BG, DIRECTION, FG, GAP, HEIGHT, OPACITY, PADDING, RADIUS, TRANSLATE_X, TRANSLATE_Y, WIDTH,
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
    /// A plain class rule with no pseudo-class; always applies.
    Base,
    /// A `:hover` rule; applies only when the node is hovered.
    Hover,
}

/// One parsed class rule: the class name, its interaction state, and its resolved
/// declarations.
struct Rule {
    /// The class selector, without the leading `.`.
    class: String,
    /// Whether this rule is a base rule or a `:hover` rule.
    state: State,
    /// Declarations whose property name mapped to a known [`PropId`], in order.
    decls: Vec<Decl>,
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
            if rule.class == class && rule.state == State::Base {
                return &rule.decls;
            }
        }
        &[]
    }

    /// Resolve the final declarations for a node from its `classes`, applying the
    /// **last-wins cascade**.
    ///
    /// For each class in `classes`, in order, this layers in:
    /// 1. that class's base (`.name`) rules, then
    /// 2. its `:hover` (`.name:hover`) rules, but only when `hovered` is `true`.
    ///
    /// Within that sequence a later declaration overrides an earlier one on the same
    /// [`PropId`] (so a `:hover` rule overrides the base rule for the property it
    /// sets, while leaving other properties from the base intact), and later classes
    /// override earlier classes. Properties no matching rule touches are absent from
    /// the result. Classes with no matching rule contribute nothing; an empty or
    /// all-unknown `classes` list yields an empty `Vec`.
    ///
    /// The returned pairs are ordered by first appearance of each property, which is
    /// the order the inline-style ops are replayed in [`apply_state`].
    pub fn resolve(&self, classes: &[&str], hovered: bool) -> Vec<Decl> {
        let mut resolved: Vec<Decl> = Vec::new();
        for class in classes {
            // Base rules first, then `:hover` rules, both in source order, so a
            // `:hover` declaration overrides the base for the same property.
            for rule in &self.rules {
                if rule.class != *class {
                    continue;
                }
                let matches = match rule.state {
                    State::Base => true,
                    State::Hover => hovered,
                };
                if !matches {
                    continue;
                }
                for (prop, value) in &rule.decls {
                    cascade(&mut resolved, *prop, value);
                }
            }
        }
        resolved
    }

    /// Whether any of `classes` has a `:hover` rule, i.e. the node would restyle when
    /// the pointer enters or leaves it.
    ///
    /// A host uses this to decide which nodes are worth tracking for hover: a node
    /// whose classes carry no `:hover` rule never changes under the cursor, so there is
    /// no point re-resolving it on every pointer move. It is the cheap, allocation-free
    /// predicate behind a "hoverables" registry (compare [`resolve`] with both states,
    /// which this avoids the cost of).
    ///
    /// [`resolve`]: Stylesheet::resolve
    #[must_use]
    pub fn reacts_to_hover(&self, classes: &[&str]) -> bool {
        self.rules
            .iter()
            .any(|rule| rule.state == State::Hover && classes.contains(&rule.class.as_str()))
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
        // Find the next selector start `.`.
        if bytes[i] != b'.' {
            i += 1;
            continue;
        }
        i += 1; // consume the dot

        // Read the selector (class plus optional `:state`) up to whitespace or `{`.
        let name_start = i;
        while i < bytes.len() && bytes[i] != b'{' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let selector = &css[name_start..i];
        // Split the selector into a class and an optional pseudo-class state.
        let (class, state) = match selector.split_once(':') {
            Some((class, "hover")) => (class.to_string(), State::Hover),
            // An unknown pseudo-class (`:focus`, `::before`, …) is outside this subset:
            // drop the whole rule so it can't masquerade as a base rule.
            Some(_) => {
                skip_rule(bytes, &mut i);
                continue;
            }
            None => (selector.to_string(), State::Base),
        };

        // Skip to the opening brace; bail if the rule is truncated.
        while i < bytes.len() && bytes[i] != b'{' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        i += 1; // consume `{`

        // Capture the block body up to the matching `}`.
        let body_start = i;
        while i < bytes.len() && bytes[i] != b'}' {
            i += 1;
        }
        let body = &css[body_start..i];
        if i < bytes.len() {
            i += 1; // consume `}`
        }

        if class.is_empty() {
            continue;
        }
        let decls = parse_block(body);
        rules.push(Rule {
            class,
            state,
            decls,
        });
    }

    Stylesheet { rules }
}

/// Advance `i` past the rest of the current rule's block: skip to the opening `{`,
/// then to and past the matching `}`. Used to drop a rule whose selector is outside
/// this subset without mis-parsing its body as a fresh rule.
fn skip_rule(bytes: &[u8], i: &mut usize) {
    while *i < bytes.len() && bytes[*i] != b'{' {
        *i += 1;
    }
    while *i < bytes.len() && bytes[*i] != b'}' {
        *i += 1;
    }
    if *i < bytes.len() {
        *i += 1; // consume `}`
    }
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
    let is_length = prop == WIDTH
        || prop == HEIGHT
        || prop == GAP
        || prop == PADDING
        || prop == RADIUS
        || prop == TRANSLATE_X
        || prop == TRANSLATE_Y;
    if is_length {
        if let Some(num) = value.strip_suffix("px") {
            return num.trim().to_string();
        }
    }
    value.to_string()
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
        // Only `background` maps; `border` and `outline` are outside this subset and
        // skipped. (`opacity`, once unknown, is now a mapped prop — see
        // `opacity_parses_as_a_unitless_float`.)
        assert_eq!(sheet.declarations("x"), &[(BG, "#fff".to_string())]);
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
        // hovered, walking classes in order:
        // class `a`: base #111111 then its `:hover` #333333 -> #333333;
        // class `b`: `:hover` #222222 overrides -> #222222 (later class wins).
        assert_eq!(
            sheet.resolve(&["a", "b"], true),
            vec![(BG, "#222222".to_string())]
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
}
