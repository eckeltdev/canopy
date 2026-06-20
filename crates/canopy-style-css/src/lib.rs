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
//! A selector is a **complex** selector: a sequence of **compounds** joined by
//! combinators. A compound is an optional leading type/tag name followed by any run of
//! `.class`, `#id`, `[attr]`, and pseudo-class parts (`:hover` only on the subject; the structural
//! and functional pseudo-classes on any compound). `*` is the universal (matches anything). A
//! descendant combinator is whitespace; a child combinator is `>`. Commas group several selectors
//! onto one declaration block.
//!
//! ```text
//! div                   /* type            */
//! #hero                 /* id              */
//! .card                 /* class           */
//! button.primary#go     /* compound: all parts must match */
//! .btn:hover            /* class + state   */
//! .card .title          /* descendant: a .title inside any .card */
//! nav > .item           /* child: a direct .item child of nav */
//! [data-role="nav"]     /* attribute (exact) */
//! a[href^="https"]      /* type + attribute (prefix) */
//! li:first-child        /* structural: first sibling */
//! li:nth-child(2n)      /* structural: every even sibling */
//! div:empty             /* structural: no children */
//! a:not(.disabled)      /* functional: anything but a .disabled */
//! :is(h1, h2, .title)   /* functional: any of the listed compounds */
//! :where(.theme) .card  /* functional: matches, but adds zero specificity */
//! *                     /* universal       */
//! ```
//!
//! Attribute selectors support `[name]` (present), `[name="v"]` (exact), and the substring
//! operators `[name^="v"]` (prefix), `[name$="v"]` (suffix), `[name*="v"]` (contains).
//!
//! **Structural pseudo-classes** (`:first-child`, `:last-child`, `:only-child`, `:empty`,
//! `:nth-child(An+B)`, `:nth-last-child(An+B)`) match against the element's position among its
//! siblings (and, for `:empty`, its child count), supplied via [`MatchTarget::with_structure`]. A
//! caller that does not thread that structure (the in-process `canopy-ui` path) leaves them as a
//! documented no-op. The `An+B` argument accepts `odd`/`even`/`<n>`/`An`/`An±B`/`-An+B`. Each
//! structural pseudo counts at the class/pseudo level of specificity (like `:hover`).
//!
//! **Functional pseudo-classes** take a parenthesized selector LIST of single compounds:
//! `:not(...)` matches when NONE of the inner compounds match; `:is(...)`/`:where(...)` match when
//! ANY does. `:is(...)`/`:not(...)` contribute the specificity of their most-specific argument;
//! `:where(...)` contributes ZERO (a real CSS rule). A combinator inside the argument
//! (`:is(.a > .b)`) drops just that entry (the single-compound scope limitation).
//!
//! Matching is resolved against a [`MatchTarget`] (the element's own [`ElementIdentity`] —
//! type name, id, classes, attributes — plus its ancestor chain). Complex selectors match
//! right-to-left: the subject compound must match the element, then each earlier compound is
//! satisfied by an ancestor (any depth for descendant, the immediate parent for child).
//! [`Stylesheet::resolve_for`] is the **cascade resolver**: it gathers
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
//! - Selectors support the **descendant** (` `) and **child** (`>`) combinators,
//!   **attribute selectors** (`[name]`, `[name="v"]`, `^=`/`$=`/`*=`), the **structural**
//!   pseudo-classes (`:first-child`/`:last-child`/`:only-child`/`:empty`/`:nth-child`/
//!   `:nth-last-child`), and the **functional** pseudo-classes (`:not`/`:is`/`:where`, scoped to a
//!   single compound per argument), but not the sibling combinators (`+`, `~`), the `~=`/`|=`
//!   attribute operators, or other pseudo-classes (`:focus`, `:nth-of-type`, …); no media queries.
//!   Box shorthands *are* expanded, but `!important` is only stripped (its precedence is not yet
//!   honored).
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
/// element: `button.primary#go` is `[Type("button"), Class("primary"), Id("go")]`.
#[derive(Clone, PartialEq, Eq)]
enum Simple {
    /// A type/tag selector (`button`, `div`) — matches the element's type name.
    Type(String),
    /// An id selector (`#go`) — matches the element's id.
    Id(String),
    /// A class selector (`.primary`) — matches if the element carries that class.
    Class(String),
    /// An attribute selector (`[name]`, `[name="v"]`, `[name^="v"]`, …) — matches against
    /// the element's attribute pairs per [`AttrMatch`].
    Attr(AttrSelector),
    /// A **structural** pseudo-class (`:first-child`, `:nth-child(An+B)`, `:empty`, …) — matches
    /// against the element's position among its siblings (and its child count, for `:empty`). See
    /// [`Structural`]. Counts at the class/pseudo level of specificity (like `:hover`).
    Structural(Structural),
    /// A **functional** pseudo-class (`:not(...)`, `:is(...)`, `:where(...)`) — matches against the
    /// element's own identity using an inner selector LIST of single compounds. See [`Functional`].
    Functional(Functional),
}

/// A **structural** pseudo-class: a test on the element's position among its siblings (and, for
/// [`Structural::Empty`], on whether it has any children of its own).
///
/// These are resolved against the [`StructInfo`] a [`MatchTarget`] carries. With **no** structural
/// info (the backward-compatible default — see [`StructInfo::UNKNOWN`]), a structural pseudo simply
/// does **not** match, so existing class/type/id sheets resolve exactly as before this wave.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Structural {
    /// `:first-child` — the element is the first of its siblings (index 0).
    FirstChild,
    /// `:last-child` — the element is the last of its siblings (index == count − 1).
    LastChild,
    /// `:only-child` — the element is the sole child (exactly one sibling, itself).
    OnlyChild,
    /// `:empty` — the element has no children of its own.
    Empty,
    /// `:nth-child(An+B)` — the element's 1-based index `i` satisfies `i = A·n + B` for some
    /// integer `n ≥ 0` (counting from the **first** sibling).
    NthChild(Nth),
    /// `:nth-last-child(An+B)` — like [`Structural::NthChild`] but counting from the **last**
    /// sibling.
    NthLastChild(Nth),
}

/// The `An+B` coefficients of an `:nth-child(...)` / `:nth-last-child(...)` argument. An index `i`
/// (1-based) matches when there is some integer `n ≥ 0` with `i = a·n + b`.
///
/// `odd` parses to `(2, 1)`, `even` to `(2, 0)`, a bare `B` to `(0, B)`, `An` to `(A, 0)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Nth {
    /// The step `A` (may be negative or zero).
    a: i32,
    /// The offset `B`.
    b: i32,
}

/// A **functional** pseudo-class: `:not(...)`, `:is(...)`, or `:where(...)`, each carrying an inner
/// selector **list** whose entries are single compounds (no combinators — a combinator inside the
/// argument drops just that one entry, see [`parse_functional`]).
///
/// - `:not(L)` matches when **none** of `L`'s compounds match the element.
/// - `:is(L)` / `:where(L)` match when **any** of `L`'s compounds matches the element.
#[derive(Clone, PartialEq, Eq)]
struct Functional {
    /// Which functional pseudo this is (governs the any/none match and the specificity rule).
    kind: FunctionalKind,
    /// The inner selector list: each entry is one compound's simple parts (an AND). An empty list
    /// (`:is()` / `:not()` with no parseable argument) matches nothing for `:is`/`:where` and
    /// everything for `:not` (vacuously — none of zero match).
    list: Vec<Vec<Simple>>,
}

/// Which functional pseudo-class a [`Functional`] is — selects both the match polarity and the
/// specificity rule (`:where` contributes **zero**; the others take their most-specific argument).
#[derive(Clone, Copy, PartialEq, Eq)]
enum FunctionalKind {
    /// `:not(...)` — matches when none of the inner compounds match.
    Not,
    /// `:is(...)` — matches when any inner compound matches.
    Is,
    /// `:where(...)` — matches when any inner compound matches, but contributes zero specificity.
    Where,
}

/// One attribute selector: the attribute name plus the test applied to its value.
#[derive(Clone, PartialEq, Eq)]
struct AttrSelector {
    /// The attribute name (case-sensitive), e.g. `id` or `data-role`.
    name: String,
    /// How the attribute's value must relate to [`AttrSelector::value`].
    op: AttrMatch,
    /// The comparison value (empty + [`AttrMatch::Present`] for a bare `[name]`).
    value: String,
}

/// The test an [`AttrSelector`] applies to an attribute's value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttrMatch {
    /// `[name]` — the attribute is present (value ignored).
    Present,
    /// `[name="v"]` — the attribute equals `v` exactly.
    Exact,
    /// `[name^="v"]` — the attribute value starts with `v`.
    Prefix,
    /// `[name$="v"]` — the attribute value ends with `v`.
    Suffix,
    /// `[name*="v"]` — the attribute value contains `v`.
    Contains,
}

/// A combinator describing how the compound on its **left** relates to the compound on its
/// **right** in a complex selector (e.g. `.card > .title`: the `.card` compound carries a
/// [`Combinator::Child`] edge to `.title`). The subject (rightmost) compound has no edge.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Combinator {
    /// A descendant combinator (whitespace): some ancestor at any depth must match.
    Descendant,
    /// A child combinator (`>`): the immediate parent must match.
    Child,
}

/// One compound in a complex selector, plus the [`Combinator`] tying it to the compound on
/// its right. The subject compound (the last in the sequence) carries the placeholder
/// [`Combinator::Descendant`], which is never consulted (matching starts from the subject and
/// walks leftward, reading each *earlier* compound's edge).
#[derive(Clone, PartialEq, Eq)]
struct ComplexPart {
    /// How this compound relates to the compound on its right.
    combinator: Combinator,
    /// The simple selectors AND-ed against one element (`button.primary` → two simples).
    simples: Vec<Simple>,
}

/// CSS **specificity** as the standard `(a, b, c)` tuple — `a` = id count, `b` = class +
/// pseudo-class count, `c` = type count — compared lexicographically (it derives `Ord`, so
/// the tuple order *is* the comparison order). Unlike a packed `a*100 + b*10 + c` integer, this
/// never overflows or mis-orders past 10 of any kind (e.g. 11 classes still beats 1 id correctly,
/// and 11 classes outrank 10). Ties break on source order at the call site.
type Spec = (u32, u32, u32);

/// A **complex** selector — a sequence of compounds joined by combinators — plus its
/// pseudo-state and CSS **specificity** (see [`Spec`]).
///
/// `parts` reads left-to-right as written: `parts.last()` is the **subject** (matches the
/// element itself) and each earlier part carries the [`Combinator`] relating it to the part on
/// its right. A single-compound selector (`button.primary`) is a one-element `parts`.
struct Selector {
    parts: Vec<ComplexPart>,
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

/// One element's identity for selector matching: its type/tag name, id, classes, and
/// attribute pairs. The element a stylesheet is resolved against ([`MatchTarget`]) carries its
/// own identity plus its ancestors' as a slice of these, so a complex (descendant/child)
/// selector can walk the chain.
///
/// A `Type`/`Id`/`Class`/`Attr` simple only matches when the corresponding field is present and
/// the test passes, so an identity with empty `attrs` (and a target with empty `ancestors`)
/// matches exactly the pure type/id/class/compound rules it did before this wave.
#[derive(Clone, Copy)]
pub struct ElementIdentity<'a> {
    /// The element's type/tag name (e.g. `"button"`), or `None`.
    pub type_name: Option<&'a str>,
    /// The element's id, or `None`.
    pub id: Option<&'a str>,
    /// The element's classes.
    pub classes: &'a [&'a str],
    /// The element's attribute `(name, value)` pairs (for attribute selectors). Empty when no
    /// attribute context is available — attribute selectors then simply do not match.
    pub attrs: &'a [(&'a str, &'a str)],
    /// The element's sibling position + child count, for structural pseudo-classes
    /// (`:first-child`, `:nth-child`, `:empty`, …). Defaults to [`StructInfo::UNKNOWN`] — with no
    /// structural context every structural pseudo simply does not match.
    pub structure: StructInfo,
}

impl<'a> ElementIdentity<'a> {
    /// An identity with the given type/id/classes, **no** attributes, and **no** structural info —
    /// the common case for a caller that has no attribute or sibling-position context.
    #[must_use]
    pub fn new(type_name: Option<&'a str>, id: Option<&'a str>, classes: &'a [&'a str]) -> Self {
        Self {
            type_name,
            id,
            classes,
            attrs: &[],
            structure: StructInfo::UNKNOWN,
        }
    }
}

/// An element's position among its siblings (0-based `index` of `count` total) and its own
/// `child_count`, for resolving structural pseudo-classes.
///
/// A caller that does not retain tree edges (the in-process `canopy-ui` path) leaves this at
/// [`StructInfo::UNKNOWN`] (`known == false`): every structural pseudo then **does not match**, so a
/// sheet with structural selectors is a documented no-op there, exactly like the descendant/child
/// combinators. A caller that walks the retained tree (the `canopy-abi` host cascade) fills it in
/// via [`MatchTarget::with_structure`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct StructInfo {
    /// Whether this info is populated. When `false` no structural pseudo matches (the back-compat
    /// default); the other fields are meaningless.
    known: bool,
    /// The element's 0-based index among its siblings.
    index: u32,
    /// The total number of siblings (including this element); always `>= 1` when `known`.
    count: u32,
    /// The number of children this element has (for `:empty`).
    child_count: u32,
}

impl StructInfo {
    /// The back-compatible "no structural context" state: structural pseudo-classes do not match.
    pub const UNKNOWN: StructInfo = StructInfo {
        known: false,
        index: 0,
        count: 0,
        child_count: 0,
    };
}

/// The element a stylesheet is resolved against: its own [`ElementIdentity`] (type/tag name,
/// id, classes, attrs) plus its `ancestors` — each ancestor's identity ordered **nearest-first**
/// (`ancestors[0]` is the parent, `ancestors[1]` the grandparent, …), which complex
/// (descendant/child) selectors walk leftward.
///
/// Build one with [`MatchTarget::new`] (defaults attrs + ancestors to empty, so a caller with no
/// such context matches exactly the pure type/id/class/compound rules it always did) and layer on
/// [`MatchTarget::with_attrs`] / [`MatchTarget::with_ancestors`] when the context is available.
pub struct MatchTarget<'a> {
    /// This element's own identity.
    own: ElementIdentity<'a>,
    /// The ancestor chain, nearest-first (index 0 = parent). Empty by default.
    ancestors: &'a [ElementIdentity<'a>],
}

impl<'a> MatchTarget<'a> {
    /// A target for an element with the given type/id/classes, **no** attributes, and **no**
    /// ancestor context. This is the backward-compatible entry point: against a stylesheet with
    /// no combinators or attribute selectors it resolves exactly as the pre-wave engine did.
    #[must_use]
    pub fn new(type_name: Option<&'a str>, id: Option<&'a str>, classes: &'a [&'a str]) -> Self {
        Self {
            own: ElementIdentity::new(type_name, id, classes),
            ancestors: &[],
        }
    }

    /// Attach the element's attribute `(name, value)` pairs, enabling attribute selectors.
    #[must_use]
    pub fn with_attrs(mut self, attrs: &'a [(&'a str, &'a str)]) -> Self {
        self.own.attrs = attrs;
        self
    }

    /// Attach the ancestor chain (nearest-first; index 0 = parent), enabling descendant/child
    /// combinators.
    #[must_use]
    pub fn with_ancestors(mut self, ancestors: &'a [ElementIdentity<'a>]) -> Self {
        self.ancestors = ancestors;
        self
    }

    /// Attach the element's structural context — its 0-based sibling `index`, the total sibling
    /// `count` (≥ 1), and its own `child_count` — enabling the structural pseudo-classes
    /// (`:first-child`, `:last-child`, `:only-child`, `:empty`, `:nth-child`, `:nth-last-child`) on
    /// this element. Without this builder a target carries [`StructInfo::UNKNOWN`] and every
    /// structural pseudo simply does not match (the backward-compatible default).
    #[must_use]
    pub fn with_structure(mut self, index: u32, count: u32, child_count: u32) -> Self {
        self.own.structure = StructInfo {
            known: true,
            index,
            count,
            child_count,
        };
        self
    }
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
                && rule.selector.parts.len() == 1
                && rule.selector.parts[0].simples.len() == 1
                && matches!(&rule.selector.parts[0].simples[0], Simple::Class(c) if c == class)
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
            if state_ok && complex_matches(&rule.selector.parts, target) {
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
        self.resolve_for(&MatchTarget::new(None, None, classes), hovered)
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
                    .parts
                    .iter()
                    .flat_map(|part| part.simples.iter())
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
        // Split on TOP-LEVEL commas only, so a functional pseudo's own comma-separated argument
        // list (`:is(.a, .b)`) is not mistaken for a grouping separator.
        for sel in split_top_level_commas(selector_list) {
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

/// Parse one selector — a **complex** selector (`.card > button.primary#go`, descendant/child
/// combinators between compounds), an optional `:hover` on the subject, and any structural /
/// functional pseudo-classes (`:first-child`, `:nth-child(2n)`, `:not(.x)`, `:is(.a, .b)`, …) on any
/// compound — into a [`Selector`] with its specificity. Returns `None` for an empty selector, an
/// empty compound, or one carrying an unsupported pseudo-class (`:focus`, `::before`, …), so it is
/// dropped (not mistaken for a base).
///
/// Specificity sums over every compound: each `id` adds to `a`; each class / attribute / structural
/// pseudo / `:hover` adds to `b`; each type adds to `c`. A functional `:is(...)` / `:not(...)` adds
/// the specificity of its **most-specific** argument; `:where(...)` adds **zero** (see
/// [`functional_specificity`]).
fn parse_selector(sel: &str) -> Option<Selector> {
    let sel = sel.trim();
    if sel.is_empty() {
        return None;
    }
    // Tokenize the complex selector into `(combinator-to-the-right, compound-text)` parts.
    let tokens = tokenize_complex(sel)?;
    if tokens.is_empty() {
        return None;
    }

    let mut parts: Vec<ComplexPart> = Vec::with_capacity(tokens.len());
    let mut spec: Spec = (0, 0, 0);
    let last = tokens.len() - 1;
    let mut state = State::Base;

    for (idx, (combinator, compound_text)) in tokens.into_iter().enumerate() {
        // `:hover` is the one pseudo that lifts to the rule's interaction State, and it is only
        // honored on the subject (rightmost) compound. On any compound, structural/functional
        // pseudos parse into Simples; an unsupported pseudo drops the whole selector.
        let (simples, has_hover) = parse_compound(compound_text)?;
        if has_hover && idx != last {
            return None; // `:hover` on a non-subject compound is unsupported -> drop the selector
        }
        if has_hover {
            state = State::Hover;
            spec.1 += 1; // a pseudo-class counts at the class/pseudo level of specificity
        }
        for simple in &simples {
            add_simple_specificity(&mut spec, simple);
        }
        parts.push(ComplexPart {
            combinator,
            simples,
        });
    }

    Some(Selector {
        parts,
        state,
        specificity: spec,
    })
}

/// Fold one simple selector's specificity into the running `(a, b, c)` tuple: an id adds to `a`; a
/// class, attribute, or structural pseudo-class adds to `b`; a type adds to `c`. A functional
/// `:is`/`:not`/`:where` contributes per [`functional_specificity`].
fn add_simple_specificity(spec: &mut Spec, simple: &Simple) {
    match simple {
        Simple::Id(_) => spec.0 += 1,
        Simple::Class(_) | Simple::Attr(_) | Simple::Structural(_) => spec.1 += 1,
        Simple::Type(_) => spec.2 += 1,
        Simple::Functional(f) => {
            let (a, b, c) = functional_specificity(f);
            spec.0 += a;
            spec.1 += b;
            spec.2 += c;
        }
    }
}

/// The specificity a functional pseudo-class contributes:
/// - `:is(...)` / `:not(...)` take the specificity of their **most-specific** argument compound
///   (the standard CSS rule); an empty argument list contributes nothing.
/// - `:where(...)` always contributes **zero** — it filters but adds no weight (a real CSS rule).
fn functional_specificity(f: &Functional) -> Spec {
    if f.kind == FunctionalKind::Where {
        return (0, 0, 0);
    }
    // The max over each argument's own compound specificity, compared lexicographically.
    let mut best: Spec = (0, 0, 0);
    for compound in &f.list {
        let mut s: Spec = (0, 0, 0);
        for simple in compound {
            add_simple_specificity(&mut s, simple);
        }
        if s > best {
            best = s;
        }
    }
    best
}

/// Tokenize a complex selector into a left-to-right `Vec` of `(combinator-relating-this-compound-
/// to-the-one-on-its-right, compound-text)`. The subject (last) part carries the placeholder
/// [`Combinator::Descendant`] (never consulted, since matching starts at the subject and reads each
/// *earlier* part's edge). Whitespace is a descendant combinator; `>` is a child combinator
/// (surrounding whitespace is absorbed). Returns `None` on a malformed run (a leading, trailing, or
/// doubled `>`).
fn tokenize_complex(sel: &str) -> Option<Vec<(Combinator, &str)>> {
    // Collect each compound's text and the combinator that PRECEDES it (relates the compound on its
    // left to it). The first compound has no preceding edge; default the rest to descendant unless a
    // `>` set it to child.
    let mut compounds: Vec<&str> = Vec::new();
    let mut preceding: Vec<Combinator> = Vec::new(); // preceding[k] relates compound k-1 -> k (k>=1)
    let bytes = sel.as_bytes();
    let mut i = 0;
    let mut start = 0;
    let mut have_open = false; // a compound's bytes are accumulating from `start`
    let mut pending_child = false; // a `>` was seen since the last compound was closed
    let mut depth = 0i32; // parenthesis nesting: a `>`/whitespace inside `(...)` is part of a
                          // functional pseudo's argument (`:is(.a > .b)`), not a real combinator.

    /// Close the open compound (if any), recording the combinator that precedes it.
    fn close<'s>(
        compounds: &mut Vec<&'s str>,
        preceding: &mut Vec<Combinator>,
        text: &'s str,
        pending_child: &mut bool,
    ) -> Option<()> {
        let text = text.trim();
        if text.is_empty() {
            return Some(());
        }
        if !compounds.is_empty() {
            preceding.push(if *pending_child {
                Combinator::Child
            } else {
                Combinator::Descendant
            });
        } else if *pending_child {
            return None; // leading `>` with no compound on its left
        }
        *pending_child = false;
        compounds.push(text);
        Some(())
    }

    while i < bytes.len() {
        let b = bytes[i];
        // Track parenthesis depth so a functional pseudo's argument is treated as opaque text: its
        // own `>` / whitespace / commas never split the outer complex selector.
        if b == b'(' {
            if !have_open {
                start = i;
                have_open = true;
            }
            depth += 1;
            i += 1;
            continue;
        }
        if b == b')' {
            depth -= 1;
            i += 1;
            continue;
        }
        if depth > 0 {
            // Inside a functional argument: accumulate verbatim (a stray `(` already opened the
            // compound above; ensure one is open in case the arg itself started this token).
            if !have_open {
                start = i;
                have_open = true;
            }
            i += 1;
            continue;
        }
        if b == b'>' {
            if have_open {
                close(
                    &mut compounds,
                    &mut preceding,
                    &sel[start..i],
                    &mut pending_child,
                )?;
                have_open = false;
            }
            if pending_child {
                return None; // `>>` is malformed
            }
            pending_child = true;
            i += 1;
            start = i;
            continue;
        }
        if b.is_ascii_whitespace() {
            if have_open {
                close(
                    &mut compounds,
                    &mut preceding,
                    &sel[start..i],
                    &mut pending_child,
                )?;
                have_open = false;
            }
            i += 1;
            start = i;
            continue;
        }
        if !have_open {
            start = i;
            have_open = true;
        }
        i += 1;
    }
    if have_open {
        close(
            &mut compounds,
            &mut preceding,
            &sel[start..],
            &mut pending_child,
        )?;
    }
    if pending_child {
        return None; // trailing `>` with no following compound
    }
    if depth != 0 {
        return None; // unbalanced parentheses (a malformed functional pseudo) -> drop the selector
    }
    if compounds.is_empty() {
        return None;
    }

    // Our model stores, per compound, the combinator tying it to the compound on ITS RIGHT — i.e.
    // compound k carries `preceding[k]` (the edge between k and k+1, which we recorded as preceding
    // k+1). The subject (last) compound carries the never-consulted placeholder.
    let mut out: Vec<(Combinator, &str)> = Vec::with_capacity(compounds.len());
    for (k, text) in compounds.iter().enumerate() {
        let comb = preceding.get(k).copied().unwrap_or(Combinator::Descendant);
        out.push((comb, *text));
    }
    Some(out)
}

/// A byte that terminates a simple-selector token within a compound: the start of the next
/// `.class` / `#id` / `[attr]` / `:pseudo` part.
fn is_simple_boundary(b: u8) -> bool {
    b == b'.' || b == b'#' || b == b'[' || b == b':'
}

/// Parse a compound selector into its simple parts plus whether it carried `:hover` (which the
/// caller lifts to the rule's interaction [`State`]). A compound is an optional leading **type**
/// name, then a run of `.class` / `#id` / `[attr]` / `:pseudo` parts in any order. A bare `*`
/// (universal) yields an empty (always-matching) list. Returns `None` on a malformed identifier,
/// attribute selector, or unsupported pseudo-class, or an empty compound.
fn parse_compound(compound: &str) -> Option<(Vec<Simple>, bool)> {
    let compound = compound.trim();
    if compound.is_empty() {
        return None;
    }
    let mut simples = Vec::new();
    let mut has_hover = false;
    let bytes = compound.as_bytes();
    let mut i = 0;
    // Optional leading type/tag name (anything before the first `.`/`#`/`[`/`:`).
    while i < bytes.len() && !is_simple_boundary(bytes[i]) {
        i += 1;
    }
    let head = &compound[..i];
    if !head.is_empty() && head != "*" {
        if !is_ident(head) {
            return None;
        }
        simples.push(Simple::Type(head.to_string()));
    }
    // Then a run of `.class` / `#id` / `[attr...]` / `:pseudo` parts.
    while i < bytes.len() {
        let kind = bytes[i];
        if kind == b'[' {
            // Read the bracketed attribute selector up to the matching `]`.
            let close = compound[i..].find(']')? + i;
            let inner = &compound[i + 1..close];
            simples.push(Simple::Attr(parse_attr_selector(inner)?));
            i = close + 1;
            continue;
        }
        if kind == b':' {
            // A pseudo-class: read its name, then (for a functional pseudo) a balanced `(...)` arg.
            i += 1;
            let name_start = i;
            while i < bytes.len() && !is_simple_boundary(bytes[i]) && bytes[i] != b'(' {
                i += 1;
            }
            let name = &compound[name_start..i];
            // A functional pseudo (`:not(`, `:is(`, …) is followed by a parenthesized argument that
            // we read by balancing the parens (its argument may itself contain `()`).
            let arg = if i < bytes.len() && bytes[i] == b'(' {
                let (inner, after) = read_balanced_parens(compound, i)?;
                i = after;
                Some(inner)
            } else {
                None
            };
            match parse_pseudo(name, arg)? {
                Pseudo::Hover => has_hover = true,
                Pseudo::Simple(s) => simples.push(s),
            }
            continue;
        }
        i += 1;
        let name_start = i;
        while i < bytes.len() && !is_simple_boundary(bytes[i]) {
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
    Some((simples, has_hover))
}

/// The outcome of parsing one pseudo-class: `:hover` lifts to the rule's [`State`]; every other
/// supported pseudo becomes a [`Simple`] AND-ed into the compound.
enum Pseudo {
    /// `:hover` — handled at the [`Selector`] level (not a per-compound simple).
    Hover,
    /// A structural or functional pseudo, matched as part of the compound.
    Simple(Simple),
}

/// Read a balanced `(...)` group starting at `open` (which must index a `(`), returning the **inner**
/// text (between the parens) and the index **just past** the closing `)`. Tracks nesting so an
/// argument containing its own parens (`:not(:is(.x))`, a `(` in an attribute value) is read whole.
/// Returns `None` if the parens are unbalanced.
fn read_balanced_parens(s: &str, open: usize) -> Option<(&str, usize)> {
    let bytes = s.as_bytes();
    debug_assert_eq!(bytes[open], b'(');
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[open + 1..i], i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None // never closed
}

/// Parse one pseudo-class `name` (lowercased) and its optional functional `arg` into a [`Pseudo`].
///
/// Supported: `:hover` (lifts to State); the structural pseudos `:first-child`, `:last-child`,
/// `:only-child`, `:empty`, `:nth-child(An+B)`, `:nth-last-child(An+B)`; and the functional pseudos
/// `:not(...)`, `:is(...)`, `:where(...)`. Any other name (`:focus`, a `::`-element, a structural
/// pseudo given an unexpected argument, a malformed `An+B`) returns `None`, dropping the selector.
fn parse_pseudo(name: &str, arg: Option<&str>) -> Option<Pseudo> {
    // ASCII-lowercase the pseudo name without allocating beyond a small buffer.
    let lname = name.to_ascii_lowercase();
    match (lname.as_str(), arg) {
        ("hover", None) => Some(Pseudo::Hover),
        ("first-child", None) => Some(Pseudo::Simple(Simple::Structural(Structural::FirstChild))),
        ("last-child", None) => Some(Pseudo::Simple(Simple::Structural(Structural::LastChild))),
        ("only-child", None) => Some(Pseudo::Simple(Simple::Structural(Structural::OnlyChild))),
        ("empty", None) => Some(Pseudo::Simple(Simple::Structural(Structural::Empty))),
        ("nth-child", Some(a)) => Some(Pseudo::Simple(Simple::Structural(Structural::NthChild(
            parse_nth(a)?,
        )))),
        ("nth-last-child", Some(a)) => Some(Pseudo::Simple(Simple::Structural(
            Structural::NthLastChild(parse_nth(a)?),
        ))),
        ("not", Some(a)) => Some(Pseudo::Simple(Simple::Functional(parse_functional(
            FunctionalKind::Not,
            a,
        )))),
        ("is", Some(a)) => Some(Pseudo::Simple(Simple::Functional(parse_functional(
            FunctionalKind::Is,
            a,
        )))),
        ("where", Some(a)) => Some(Pseudo::Simple(Simple::Functional(parse_functional(
            FunctionalKind::Where,
            a,
        )))),
        _ => None, // unsupported pseudo-class (or a structural pseudo given a bad/missing arg)
    }
}

/// Parse a functional pseudo's parenthesized argument — a selector **list** of single compounds — into
/// a [`Functional`]. Each comma-separated entry is parsed as one compound; an entry that is empty,
/// malformed, carries `:hover`, or contains a combinator (a space/`>` — i.e. tokenizes to more than
/// one compound) is **dropped gracefully** (a documented limitation: functional args are scoped to a
/// single compound each). The remaining entries form the list.
fn parse_functional(kind: FunctionalKind, arg: &str) -> Functional {
    let mut list: Vec<Vec<Simple>> = Vec::new();
    for entry in split_top_level_commas(arg) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        // Reject a combinator inside the arg: if the entry tokenizes to more than one compound (a
        // descendant/child relation), drop just this entry (the single-compound scope limitation).
        match tokenize_complex(entry) {
            Some(tokens) if tokens.len() == 1 => {
                // A single compound — parse it; drop the entry if it is malformed or carries :hover.
                if let Some((simples, has_hover)) = parse_compound(tokens[0].1) {
                    if !has_hover {
                        list.push(simples);
                    }
                }
            }
            _ => { /* a combinator (or malformed) inside the functional arg: drop this entry */ }
        }
    }
    Functional { kind, list }
}

/// Parse an `An+B` micro-grammar (the `:nth-child(...)` / `:nth-last-child(...)` argument) into an
/// [`Nth`]. Accepts `odd` (`2n+1`), `even` (`2n`), a bare integer `B` (`(0, B)`), `An` (`(A, 0)`),
/// `An+B`, `An-B`, `-An+B`, `n` (`(1, 0)`), `-n` (`(-1, 0)`), and a leading-sign `+`/`-`. Whitespace
/// around the `n`, the sign, and `B` is tolerated. Returns `None` on anything malformed.
fn parse_nth(arg: &str) -> Option<Nth> {
    let s = arg.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    if s == "odd" {
        return Some(Nth { a: 2, b: 1 });
    }
    if s == "even" {
        return Some(Nth { a: 2, b: 0 });
    }
    // Split on the `n`: everything before it is the `A` coefficient, everything after is `+B`/`-B`.
    if let Some(n_pos) = s.find('n') {
        let a_str = s[..n_pos].trim();
        let b_str = s[n_pos + 1..].trim();
        // The coefficient before `n`: empty -> 1, `-` -> -1, `+` -> 1, else parse the integer.
        let a = match a_str {
            "" | "+" => 1,
            "-" => -1,
            other => parse_signed_int(other)?,
        };
        // The offset after `n`: empty -> 0; otherwise a signed `+B` / `-B` (the sign is required, as
        // CSS writes `2n + 1`, and whitespace around it is allowed).
        let b = if b_str.is_empty() {
            0
        } else {
            // Must start with an explicit sign (CSS `An±B`), then an integer.
            let sign_byte = b_str.as_bytes()[0];
            if sign_byte != b'+' && sign_byte != b'-' {
                return None;
            }
            let neg = sign_byte == b'-';
            let mag: i32 = b_str[1..].trim().parse().ok()?;
            if neg {
                -mag
            } else {
                mag
            }
        };
        Some(Nth { a, b })
    } else {
        // No `n`: a plain integer `B` (matches exactly the `B`-th element), e.g. `:nth-child(3)`.
        Some(Nth {
            a: 0,
            b: parse_signed_int(&s)?,
        })
    }
}

/// Parse an optionally `+`/`-`-signed base-10 integer (`"3"`, `"+3"`, `"-3"`), trimming whitespace.
/// Returns `None` on anything that is not a clean signed integer.
fn parse_signed_int(s: &str) -> Option<i32> {
    let s = s.trim();
    let (neg, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let mag: i32 = digits.trim().parse().ok()?;
    Some(if neg { -mag } else { mag })
}

/// Parse the inside of an attribute selector (the text between `[` and `]`): `name`, `name="v"`,
/// `name^="v"`, `name$="v"`, or `name*="v"`. The value's surrounding quotes (single or double) are
/// optional and stripped. Returns `None` on a malformed name or operator.
fn parse_attr_selector(inner: &str) -> Option<AttrSelector> {
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }
    // Find the operator (`=`, `^=`, `$=`, `*=`) if any.
    if let Some(eq) = inner.find('=') {
        let (name_part, op) = match inner.as_bytes().get(eq.wrapping_sub(1)) {
            Some(b'^') => (&inner[..eq - 1], AttrMatch::Prefix),
            Some(b'$') => (&inner[..eq - 1], AttrMatch::Suffix),
            Some(b'*') => (&inner[..eq - 1], AttrMatch::Contains),
            _ => (&inner[..eq], AttrMatch::Exact),
        };
        let name = name_part.trim();
        if !is_ident(name) {
            return None;
        }
        let value = strip_attr_quotes(inner[eq + 1..].trim());
        Some(AttrSelector {
            name: name.to_string(),
            op,
            value: value.to_string(),
        })
    } else {
        // Bare `[name]`: presence test.
        if !is_ident(inner) {
            return None;
        }
        Some(AttrSelector {
            name: inner.to_string(),
            op: AttrMatch::Present,
            value: String::new(),
        })
    }
}

/// Strip a matching pair of surrounding single or double quotes from an attribute value; an
/// unquoted value passes through unchanged.
fn strip_attr_quotes(v: &str) -> &str {
    let bytes = v.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

/// A valid (lite) CSS identifier: non-empty, only `[A-Za-z0-9_-]`.
fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether a **complex** selector matches `target`, by standard right-to-left CSS matching: the
/// subject compound (`parts.last()`) must match the target's own identity, then walk leftward —
/// for a [`Combinator::Descendant`] edge SOME ancestor (at any depth) must match the next compound
/// (and matching continues from above that ancestor); for a [`Combinator::Child`] edge the
/// IMMEDIATE parent must match. A single-compound selector matches iff its subject matches.
fn complex_matches(parts: &[ComplexPart], target: &MatchTarget) -> bool {
    let Some((subject, rest)) = parts.split_last() else {
        return false;
    };
    if !compound_matches(&subject.simples, &target.own) {
        return false;
    }
    // The combinator on each compound describes how it relates to the compound on its RIGHT; so
    // walking the `rest` from rightmost to leftmost, the edge to consult for compound `rest[k]` is
    // `rest[k].combinator` (its relation to compound k+1, which we just satisfied). We track a
    // cursor into `target.ancestors` (0 = parent) marking how far up the chain we've consumed.
    let ancestors = target.ancestors;
    let mut cursor = 0usize; // index of the next ancestor to consider (0 = immediate parent)
    for part in rest.iter().rev() {
        match part.combinator {
            Combinator::Child => {
                // The immediate parent (relative to the cursor) must match this compound.
                let Some(parent) = ancestors.get(cursor) else {
                    return false;
                };
                if !compound_matches(&part.simples, parent) {
                    return false;
                }
                cursor += 1;
            }
            Combinator::Descendant => {
                // Some ancestor at or above the cursor must match; continue from above it.
                let mut found = None;
                for (offset, anc) in ancestors[cursor..].iter().enumerate() {
                    if compound_matches(&part.simples, anc) {
                        found = Some(cursor + offset + 1);
                        break;
                    }
                }
                match found {
                    Some(next) => cursor = next,
                    None => return false,
                }
            }
        }
    }
    true
}

/// Whether a single compound selector's simple parts all match `identity` (an AND).
///
/// Type names are matched ASCII case-insensitively (`BUTTON` matches `<button>`), per HTML's
/// case-insensitive tag names. Classes, ids, and attribute names/values stay case-**sensitive**,
/// per CSS.
fn compound_matches(simples: &[Simple], identity: &ElementIdentity) -> bool {
    simples
        .iter()
        .all(|simple| simple_matches(simple, identity))
}

/// Whether a single simple selector matches `identity`. Factored out of [`compound_matches`] so the
/// functional pseudos (`:not`/`:is`/`:where`) can re-use it against their inner compounds.
fn simple_matches(simple: &Simple, identity: &ElementIdentity) -> bool {
    match simple {
        Simple::Type(t) => identity
            .type_name
            .is_some_and(|name| name.eq_ignore_ascii_case(t)),
        Simple::Id(id) => identity.id == Some(id.as_str()),
        Simple::Class(c) => identity.classes.contains(&c.as_str()),
        Simple::Attr(attr) => attr_matches(attr, identity.attrs),
        Simple::Structural(s) => structural_matches(*s, identity.structure),
        Simple::Functional(f) => functional_matches(f, identity),
    }
}

/// Whether a structural pseudo-class matches given the element's [`StructInfo`].
///
/// **Default:** with no structural context (`info.known == false`, the back-compat
/// [`StructInfo::UNKNOWN`]) every structural pseudo returns `false` — a sheet that uses them is a
/// no-op against a target that carries no sibling-position info (the in-process `canopy-ui` path),
/// exactly like the unsupported combinators there. Only a caller that threads real structure (the
/// `canopy-abi` host cascade) makes these match.
fn structural_matches(s: Structural, info: StructInfo) -> bool {
    if !info.known {
        return false;
    }
    match s {
        Structural::FirstChild => info.index == 0,
        Structural::LastChild => info.count > 0 && info.index == info.count - 1,
        Structural::OnlyChild => info.count == 1,
        Structural::Empty => info.child_count == 0,
        // `:nth-child(An+B)` counts from the FIRST sibling: the 1-based index is `index + 1`.
        Structural::NthChild(nth) => nth_matches(nth, info.index + 1),
        // `:nth-last-child(An+B)` counts from the LAST sibling: the 1-based index from the end is
        // `count - index`.
        Structural::NthLastChild(nth) => {
            info.count > 0 && nth_matches(nth, info.count - info.index)
        }
    }
}

/// Whether a 1-based sibling index `i` (≥ 1) satisfies the `An+B` test: there is some integer
/// `n ≥ 0` with `i = a·n + b`.
///
/// - `a == 0`: matches iff `i == b` (a single element).
/// - `a != 0`: `i - b` must be divisible by `a` with a **non-negative** quotient `n`.
fn nth_matches(nth: Nth, i: u32) -> bool {
    let i = i as i32;
    let Nth { a, b } = nth;
    if a == 0 {
        return i == b;
    }
    let diff = i - b;
    // `n = diff / a` must be an integer (`diff % a == 0`) and `n >= 0`.
    diff % a == 0 && diff / a >= 0
}

/// Whether a functional pseudo (`:not`/`:is`/`:where`) matches `identity`: `:not` matches when NONE
/// of its inner compounds match; `:is`/`:where` match when ANY does. Each inner compound is matched
/// against the element's own identity (the functional arg is scoped to a single compound — no tree
/// walk needed).
fn functional_matches(f: &Functional, identity: &ElementIdentity) -> bool {
    let any = f
        .list
        .iter()
        .any(|compound| compound.iter().all(|s| simple_matches(s, identity)));
    match f.kind {
        FunctionalKind::Not => !any,
        FunctionalKind::Is | FunctionalKind::Where => any,
    }
}

/// Whether `attr` matches some `(name, value)` pair in `attrs` per its [`AttrMatch`] test. The
/// name must match exactly (case-sensitive); the value test is presence / exact / prefix / suffix
/// / contains. An empty substring value (`[x*=""]`) never matches (mirrors CSS).
fn attr_matches(attr: &AttrSelector, attrs: &[(&str, &str)]) -> bool {
    attrs.iter().any(|(name, value)| {
        if *name != attr.name {
            return false;
        }
        match attr.op {
            AttrMatch::Present => true,
            AttrMatch::Exact => *value == attr.value,
            AttrMatch::Prefix => !attr.value.is_empty() && value.starts_with(&attr.value),
            AttrMatch::Suffix => !attr.value.is_empty() && value.ends_with(&attr.value),
            AttrMatch::Contains => !attr.value.is_empty() && value.contains(&attr.value),
        }
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
        let hit = MatchTarget::new(Some("button"), Some("go"), &["btn"]);
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
        let miss = MatchTarget::new(Some("div"), None, &[]);
        assert!(
            sheet.resolve_for(&miss, false).is_empty(),
            "no selector matches a bare div"
        );
    }

    #[test]
    fn compound_selector_requires_all_parts() {
        let sheet = parse("button.primary { background:#abcdef }");
        let both = MatchTarget::new(Some("button"), None, &["primary"]);
        assert_eq!(
            sheet.resolve_for(&both, false),
            vec![(BG, "#abcdef".to_string())]
        );
        let only_type = MatchTarget::new(Some("button"), None, &[]);
        assert!(
            sheet.resolve_for(&only_type, false).is_empty(),
            "missing the .primary class"
        );
        let only_class = MatchTarget::new(Some("div"), None, &["primary"]);
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
        let hit = MatchTarget::new(Some("button"), Some("x"), &["c"]);
        assert_eq!(
            sheet.resolve_for(&hit, false),
            vec![(BG, "#111111".to_string())]
        );
    }

    #[test]
    fn selector_grouping_shares_declarations() {
        let sheet = parse(".a, .b, button { color:#445566 }");
        let targets = [
            MatchTarget::new(None, None, &["a"]),
            MatchTarget::new(None, None, &["b"]),
            MatchTarget::new(Some("button"), None, &[]),
        ];
        for target in &targets {
            assert_eq!(
                sheet.resolve_for(target, false),
                vec![(FG, "#445566".to_string())]
            );
        }
    }

    // --- Wave 3a: descendant/child combinators + attribute selectors -------

    #[test]
    fn descendant_combinator_matches_a_title_inside_a_card_not_outside() {
        // `.card .title` styles a `.title` nested anywhere under a `.card`, but not a `.title`
        // with no `.card` ancestor.
        let sheet = parse(".card .title { color:#abcdef }");
        // A `.title` whose parent is a `.card`.
        let card = ElementIdentity::new(None, None, &["card"]);
        let wrapper = ElementIdentity::new(None, None, &["wrapper"]);
        let parent_card = [card];
        let inside = MatchTarget::new(None, None, &["title"]).with_ancestors(&parent_card);
        assert_eq!(
            sheet.resolve_for(&inside, false),
            vec![(FG, "#abcdef".to_string())]
        );
        // A `.title` nested deeper (grandparent is the `.card`) still matches (any depth).
        let deep_chain = [wrapper, card];
        let deep = MatchTarget::new(None, None, &["title"]).with_ancestors(&deep_chain);
        assert_eq!(
            sheet.resolve_for(&deep, false),
            vec![(FG, "#abcdef".to_string())]
        );
        // A `.title` with no `.card` ancestor does NOT match.
        let no_card = [wrapper];
        let outside = MatchTarget::new(None, None, &["title"]).with_ancestors(&no_card);
        assert!(sheet.resolve_for(&outside, false).is_empty());
        // A `.title` with NO ancestors does not match either.
        let bare = MatchTarget::new(None, None, &["title"]);
        assert!(sheet.resolve_for(&bare, false).is_empty());
    }

    #[test]
    fn child_combinator_matches_only_direct_children() {
        // `nav > .item` matches only a `.item` whose IMMEDIATE parent is `nav`.
        let sheet = parse("nav > .item { background:#222222 }");
        let nav = ElementIdentity::new(Some("nav"), None, &[]);
        let list = ElementIdentity::new(None, None, &["list"]);
        // Direct child of nav: matches.
        let parent_nav = [nav];
        let direct = MatchTarget::new(None, None, &["item"]).with_ancestors(&parent_nav);
        assert_eq!(
            sheet.resolve_for(&direct, false),
            vec![(BG, "#222222".to_string())]
        );
        // A `.item` whose immediate parent is a `.list` (nav is the grandparent): NO match.
        let list_then_nav = [list, nav];
        let grandchild = MatchTarget::new(None, None, &["item"]).with_ancestors(&list_then_nav);
        assert!(
            sheet.resolve_for(&grandchild, false).is_empty(),
            "child combinator requires the immediate parent"
        );
    }

    #[test]
    fn child_then_descendant_chain_matches_mixed_combinators() {
        // `#root > .card .title`: subject `.title` has SOME `.card` ancestor, and that `.card`
        // is a DIRECT child of `#root`.
        let sheet = parse("#root > .card .title { color:#0a0b0c }");
        let root = ElementIdentity::new(None, Some("root"), &[]);
        let card = ElementIdentity::new(None, None, &["card"]);
        let inner = ElementIdentity::new(None, None, &["inner"]);
        let wrap = ElementIdentity::new(None, None, &["wrap"]);
        // title <- inner <- card <- root: card is a direct child of root, title descends from card.
        let chain = [inner, card, root];
        let hit = MatchTarget::new(None, None, &["title"]).with_ancestors(&chain);
        assert_eq!(
            sheet.resolve_for(&hit, false),
            vec![(FG, "#0a0b0c".to_string())]
        );
        // If `.card` is NOT a direct child of root (a wrapper sits between), the `>` edge fails.
        let chain_with_wrap = [inner, card, wrap, root];
        let miss = MatchTarget::new(None, None, &["title"]).with_ancestors(&chain_with_wrap);
        assert!(sheet.resolve_for(&miss, false).is_empty());
    }

    #[test]
    fn combinator_specificity_sums_all_compounds() {
        // `.card .title` (two classes -> spec (0,2,0)) must beat a single `.title` (0,1,0) even
        // though the single-class rule appears later in source order.
        let sheet = parse(".title { color:#111111 } .card .title { color:#222222 }");
        let card = ElementIdentity::new(None, None, &["card"]);
        let parent_card = [card];
        let target = MatchTarget::new(None, None, &["title"]).with_ancestors(&parent_card);
        assert_eq!(
            sheet.resolve_for(&target, false),
            vec![(FG, "#222222".to_string())],
            "the 2-compound rule outranks the 1-compound rule by specificity"
        );
        // And `nav > .item.lead` (type + 2 classes -> (0,2,1)) outscores `.item` (0,1,0).
        let sheet2 = parse(".item { color:#111111 } nav > .item.lead { color:#333333 }");
        let nav = ElementIdentity::new(Some("nav"), None, &[]);
        let parent_nav = [nav];
        let item = MatchTarget::new(None, None, &["item", "lead"]).with_ancestors(&parent_nav);
        assert_eq!(
            sheet2.resolve_for(&item, false),
            vec![(FG, "#333333".to_string())]
        );
    }

    #[test]
    fn attribute_present_selector_matches_when_attr_exists() {
        let sheet = parse("[data-role] { background:#010203 }");
        let role_attr = [("data-role", "nav")];
        let with = MatchTarget::new(Some("div"), None, &[]).with_attrs(&role_attr);
        assert_eq!(
            sheet.resolve_for(&with, false),
            vec![(BG, "#010203".to_string())]
        );
        let other_attr = [("other", "x")];
        let without = MatchTarget::new(Some("div"), None, &[]).with_attrs(&other_attr);
        assert!(sheet.resolve_for(&without, false).is_empty());
        // No attribute context at all: the attribute selector cannot match.
        let none = MatchTarget::new(Some("div"), None, &[]);
        assert!(sheet.resolve_for(&none, false).is_empty());
    }

    #[test]
    fn attribute_exact_selector_matches_value() {
        let sheet = parse("[data-role=\"nav\"] { color:#445566 }");
        let nav_attr = [("data-role", "nav")];
        let hit = MatchTarget::new(None, None, &[]).with_attrs(&nav_attr);
        assert_eq!(
            sheet.resolve_for(&hit, false),
            vec![(FG, "#445566".to_string())]
        );
        // A different value does not match exact.
        let main_attr = [("data-role", "main")];
        let miss = MatchTarget::new(None, None, &[]).with_attrs(&main_attr);
        assert!(sheet.resolve_for(&miss, false).is_empty());
    }

    #[test]
    fn attribute_prefix_suffix_contains_operators_match_substrings() {
        let sheet = parse(
            "a[href^=\"https\"] { color:#111111 } \
             a[href$=\".pdf\"] { color:#222222 } \
             a[href*=\"docs\"] { color:#333333 }",
        );
        // prefix: starts with https
        let all_attr = [("href", "https://example.com/docs/x.pdf")];
        let pre = MatchTarget::new(Some("a"), None, &[]).with_attrs(&all_attr);
        let r = sheet.resolve_for(&pre, false);
        // All three match this url (starts with https, ends with .pdf, contains docs); the last in
        // source order wins on equal specificity (each is type+attr = (0,1,1)).
        assert_eq!(r, vec![(FG, "#333333".to_string())]);

        // A url matching only the prefix rule.
        let pre_attr = [("href", "https://example.com/page")];
        let only_pre = MatchTarget::new(Some("a"), None, &[]).with_attrs(&pre_attr);
        assert_eq!(
            sheet.resolve_for(&only_pre, false),
            vec![(FG, "#111111".to_string())]
        );
        // A url matching only the suffix rule.
        let suf_attr = [("href", "ftp://host/file.pdf")];
        let only_suf = MatchTarget::new(Some("a"), None, &[]).with_attrs(&suf_attr);
        assert_eq!(
            sheet.resolve_for(&only_suf, false),
            vec![(FG, "#222222".to_string())]
        );
    }

    #[test]
    fn attribute_selector_adds_class_level_specificity() {
        // An attribute simple counts at the class level (10): `[data-x="1"]` (0,1,0) beats a bare
        // type rule (0,0,1) regardless of source order.
        let sheet = parse("div { color:#111111 } [data-x=\"1\"] { color:#222222 }");
        let data_x = [("data-x", "1")];
        let target = MatchTarget::new(Some("div"), None, &[]).with_attrs(&data_x);
        assert_eq!(
            sheet.resolve_for(&target, false),
            vec![(FG, "#222222".to_string())]
        );
    }

    #[test]
    fn plain_class_and_type_sheet_is_unaffected_by_the_new_engine() {
        // A sheet with NO combinators/attribute selectors, resolved against a target with empty
        // ancestors/attrs, must behave exactly as before this wave.
        let sheet = parse(".a { color:#445566 } button { background:#111111 } #x { padding:4px }");
        // Class only.
        assert_eq!(
            sheet.resolve_for(&MatchTarget::new(None, None, &["a"]), false),
            vec![(FG, "#445566".to_string())]
        );
        // Type only.
        assert_eq!(
            sheet.resolve_for(&MatchTarget::new(Some("button"), None, &[]), false),
            vec![(BG, "#111111".to_string())]
        );
        // Id only.
        assert_eq!(
            sheet.resolve_for(&MatchTarget::new(None, Some("x"), &[]), false),
            vec![(PADDING, "4".to_string())]
        );
        // A non-matching target stays empty.
        assert!(sheet
            .resolve_for(&MatchTarget::new(Some("div"), None, &["nope"]), false)
            .is_empty());
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
        let target = MatchTarget::new(
            None,
            Some("x"),
            &[
                "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11",
            ],
        );
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
        let target = MatchTarget::new(Some("button"), None, &[]);
        assert_eq!(
            sheet.resolve_for(&target, false),
            vec![(BG, "#abcdef".to_string())]
        );
    }

    #[test]
    fn class_match_stays_case_sensitive() {
        // Classes remain case-sensitive per CSS: `.Btn` does not match the `btn` class.
        let sheet = parse(".Btn { background:#abcdef }");
        let target = MatchTarget::new(None, None, &["btn"]);
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

    // --- Wave 3b: structural + functional pseudo-classes -------------------

    #[test]
    fn first_last_only_child_match_by_position() {
        let sheet = parse(
            "li:first-child { color:#111111 } \
             li:last-child  { color:#222222 } \
             li:only-child  { color:#333333 }",
        );
        let resolve = |index: u32, count: u32| {
            sheet.resolve_for(
                &MatchTarget::new(Some("li"), None, &[]).with_structure(index, count, 0),
                false,
            )
        };
        // First of three -> :first-child.
        assert_eq!(resolve(0, 3), vec![(FG, "#111111".to_string())]);
        // Last of three -> :last-child.
        assert_eq!(resolve(2, 3), vec![(FG, "#222222".to_string())]);
        // A middle child matches none of the three.
        assert!(resolve(1, 3).is_empty());
        // Sole child -> :only-child (and also :first/:last; :only-child is last in source so wins).
        assert_eq!(resolve(0, 1), vec![(FG, "#333333".to_string())]);
    }

    #[test]
    fn nth_child_even_odd_and_an_plus_b() {
        // :nth-child(2n) = evens (2nd, 4th, …); odd = 1st,3rd,…; 3n+1 = 1st,4th,7th,…
        let even = parse("li:nth-child(2n) { color:#0a0a0a }");
        let odd = parse("li:nth-child(odd) { color:#0b0b0b }");
        let three_n1 = parse("li:nth-child(3n+1) { color:#0c0c0c }");
        let count = 7u32;
        let matches = |sheet: &Stylesheet, index: u32| {
            !sheet
                .resolve_for(
                    &MatchTarget::new(Some("li"), None, &[]).with_structure(index, count, 0),
                    false,
                )
                .is_empty()
        };
        // 1-based positions: even matches 2,4,6 (0-based index 1,3,5).
        let even_hits: Vec<u32> = (0..count).filter(|&i| matches(&even, i)).collect();
        assert_eq!(even_hits, vec![1, 3, 5], "2n -> 2nd,4th,6th");
        // odd matches 1,3,5,7 (0-based index 0,2,4,6).
        let odd_hits: Vec<u32> = (0..count).filter(|&i| matches(&odd, i)).collect();
        assert_eq!(odd_hits, vec![0, 2, 4, 6], "odd -> 1st,3rd,5th,7th");
        // 3n+1 matches 1,4,7 (0-based index 0,3,6).
        let tn1_hits: Vec<u32> = (0..count).filter(|&i| matches(&three_n1, i)).collect();
        assert_eq!(tn1_hits, vec![0, 3, 6], "3n+1 -> 1st,4th,7th");
    }

    #[test]
    fn nth_last_child_counts_from_the_end() {
        // :nth-last-child(1) is the LAST element; (2) the second-to-last.
        let last = parse("li:nth-last-child(1) { color:#aa0000 }");
        let second_last = parse("li:nth-last-child(2) { color:#00aa00 }");
        let count = 4u32;
        let hits = |sheet: &Stylesheet| -> Vec<u32> {
            (0..count)
                .filter(|&i| {
                    !sheet
                        .resolve_for(
                            &MatchTarget::new(Some("li"), None, &[]).with_structure(i, count, 0),
                            false,
                        )
                        .is_empty()
                })
                .collect()
        };
        assert_eq!(
            hits(&last),
            vec![3],
            "nth-last-child(1) is the last (index 3)"
        );
        assert_eq!(
            hits(&second_last),
            vec![2],
            "nth-last-child(2) is the second-to-last (index 2)"
        );
    }

    #[test]
    fn empty_matches_a_childless_element() {
        let sheet = parse("div:empty { color:#445566 }");
        // A div with no children matches :empty.
        let empty = MatchTarget::new(Some("div"), None, &[]).with_structure(0, 1, 0);
        assert_eq!(
            sheet.resolve_for(&empty, false),
            vec![(FG, "#445566".to_string())]
        );
        // A div WITH children does not.
        let parent = MatchTarget::new(Some("div"), None, &[]).with_structure(0, 1, 2);
        assert!(sheet.resolve_for(&parent, false).is_empty());
    }

    #[test]
    fn structural_pseudo_is_a_no_op_without_structure_info() {
        // The default StructInfo::UNKNOWN: structural pseudos simply do not match (the back-compat
        // contract for callers that don't thread sibling position, e.g. canopy-ui).
        let sheet = parse("li:first-child { color:#111111 } li:nth-child(2n) { color:#222222 }");
        let no_info = MatchTarget::new(Some("li"), None, &[]); // no .with_structure
        assert!(
            sheet.resolve_for(&no_info, false).is_empty(),
            "no structural info -> structural pseudos do not match"
        );
    }

    #[test]
    fn not_excludes_the_matching_compound() {
        // `a:not(.disabled)` styles every `a` EXCEPT one carrying `.disabled`.
        let sheet = parse("a:not(.disabled) { color:#123456 }");
        let plain = MatchTarget::new(Some("a"), None, &[]);
        assert_eq!(
            sheet.resolve_for(&plain, false),
            vec![(FG, "#123456".to_string())],
            "a without .disabled is styled"
        );
        let disabled = MatchTarget::new(Some("a"), None, &["disabled"]);
        assert!(
            sheet.resolve_for(&disabled, false).is_empty(),
            ":not(.disabled) excludes the .disabled anchor"
        );
    }

    #[test]
    fn is_matches_any_of_the_listed_compounds() {
        // `:is(.a, .b)` matches an element carrying EITHER class.
        let sheet = parse(":is(.a, .b) { color:#0099ff }");
        let a = MatchTarget::new(None, None, &["a"]);
        let b = MatchTarget::new(None, None, &["b"]);
        let c = MatchTarget::new(None, None, &["c"]);
        assert_eq!(
            sheet.resolve_for(&a, false),
            vec![(FG, "#0099ff".to_string())],
            ":is matches .a"
        );
        assert_eq!(
            sheet.resolve_for(&b, false),
            vec![(FG, "#0099ff".to_string())],
            ":is matches .b"
        );
        assert!(
            sheet.resolve_for(&c, false).is_empty(),
            ":is does not match an element with neither class"
        );
    }

    #[test]
    fn where_matches_but_contributes_zero_specificity() {
        // A `:where(.high)` rule and a plain `.low` class rule both set `color` on an element
        // carrying BOTH classes. `:where` adds ZERO specificity, so it is (0,0,0)+nothing while
        // `.low` is (0,1,0); the plain class rule WINS even though the :where rule comes later in
        // source order (specificity dominates source order).
        let sheet = parse(":where(.high) { color:#ffffff } .low { color:#000000 }");
        let both = MatchTarget::new(None, None, &["high", "low"]);
        assert_eq!(
            sheet.resolve_for(&both, false),
            vec![(FG, "#000000".to_string())],
            ":where adds 0 specificity, so the plain .low class rule wins"
        );
        // Sanity: :where still FILTERS — an element without .high is not matched by the :where rule.
        let only_low = MatchTarget::new(None, None, &["low"]);
        assert_eq!(
            sheet.resolve_for(&only_low, false),
            vec![(FG, "#000000".to_string())]
        );
        let only_high = MatchTarget::new(None, None, &["high"]);
        assert_eq!(
            sheet.resolve_for(&only_high, false),
            vec![(FG, "#ffffff".to_string())],
            ":where(.high) still matches when .high is present"
        );
    }

    #[test]
    fn is_takes_its_most_specific_argument_for_specificity() {
        // `:is(#id, .cls)` takes the #id arm's specificity (1,0,0). Against an element matching the
        // class arm, the :is rule still outranks a plain `.cls` (0,1,0) rule on the same property —
        // because :is's specificity is fixed to its MOST-specific argument regardless of which arm
        // actually matched.
        let sheet = parse(":is(#hero, .cls) { color:#111111 } .cls { color:#222222 }");
        let target = MatchTarget::new(None, None, &["cls"]);
        assert_eq!(
            sheet.resolve_for(&target, false),
            vec![(FG, "#111111".to_string())],
            ":is(#id, .cls) carries the #id specificity and beats a plain .cls rule"
        );
    }

    #[test]
    fn not_takes_its_argument_specificity() {
        // `:not(.x)` contributes its argument's specificity (a class = (0,1,0)). So `a:not(.x)`
        // ((0,1,1)) beats a bare `a` ((0,0,1)) rule on the same property.
        let sheet = parse("a { color:#111111 } a:not(.x) { color:#222222 }");
        let plain_a = MatchTarget::new(Some("a"), None, &[]);
        assert_eq!(
            sheet.resolve_for(&plain_a, false),
            vec![(FG, "#222222".to_string())],
            "a:not(.x) outranks a bare a by the :not argument's class specificity"
        );
    }

    #[test]
    fn functional_arg_with_a_combinator_drops_that_entry() {
        // A combinator inside the functional arg is unsupported (single-compound scope). `:is(.a,
        // .b > .c)` keeps `.a` but DROPS the `.b > .c` entry. So `.b > .c` never matches via :is,
        // but `.a` still does.
        let sheet = parse(":is(.a, .b > .c) { color:#334455 }");
        let a = MatchTarget::new(None, None, &["a"]);
        assert_eq!(
            sheet.resolve_for(&a, false),
            vec![(FG, "#334455".to_string())],
            "the valid single-compound `.a` entry is kept"
        );
        // An element carrying .c (the subject of the dropped combinator entry) is NOT matched —
        // the whole `.b > .c` entry was dropped.
        let c = MatchTarget::new(None, None, &["c"]);
        assert!(
            sheet.resolve_for(&c, false).is_empty(),
            "the combinator entry was dropped, so .c does not match via :is"
        );
    }

    #[test]
    fn nth_parsing_edge_cases() {
        // Direct unit tests for the An+B micro-parser.
        assert_eq!(parse_nth("odd"), Some(Nth { a: 2, b: 1 }));
        assert_eq!(parse_nth("even"), Some(Nth { a: 2, b: 0 }));
        assert_eq!(parse_nth("2n"), Some(Nth { a: 2, b: 0 }));
        assert_eq!(parse_nth("2n+1"), Some(Nth { a: 2, b: 1 }));
        assert_eq!(parse_nth("2n-1"), Some(Nth { a: 2, b: -1 }));
        assert_eq!(parse_nth("-n+3"), Some(Nth { a: -1, b: 3 }));
        assert_eq!(parse_nth("-2n+3"), Some(Nth { a: -2, b: 3 }));
        assert_eq!(parse_nth("n"), Some(Nth { a: 1, b: 0 }));
        assert_eq!(parse_nth("3"), Some(Nth { a: 0, b: 3 }));
        assert_eq!(parse_nth("  2n + 1 "), Some(Nth { a: 2, b: 1 }));
        assert_eq!(parse_nth("+3"), Some(Nth { a: 0, b: 3 }));
        // Malformed forms return None.
        assert_eq!(parse_nth(""), None);
        assert_eq!(parse_nth("2x"), None);
        assert_eq!(parse_nth("2n+"), None);
        assert_eq!(parse_nth("2n 1"), None, "missing sign before B");
        assert_eq!(parse_nth("abc"), None);
    }

    #[test]
    fn nth_match_semantics_for_negative_and_zero_step() {
        // a == 0: matches exactly position b.
        assert!(nth_matches(Nth { a: 0, b: 3 }, 3));
        assert!(!nth_matches(Nth { a: 0, b: 3 }, 4));
        // -n+3 matches the first three (1,2,3) and nothing past.
        for i in 1..=3 {
            assert!(nth_matches(Nth { a: -1, b: 3 }, i), "-n+3 matches {i}");
        }
        assert!(!nth_matches(Nth { a: -1, b: 3 }, 4), "-n+3 stops after 3");
        // 2n+1 (odd) matches 1,3,5 but not 2,4.
        assert!(nth_matches(Nth { a: 2, b: 1 }, 1));
        assert!(!nth_matches(Nth { a: 2, b: 1 }, 2));
        assert!(nth_matches(Nth { a: 2, b: 1 }, 3));
    }

    #[test]
    fn plain_sheet_unaffected_by_pseudo_engine() {
        // A sheet with NO structural/functional pseudos, resolved against a target carrying full
        // structure info, must resolve exactly as a plain class/type sheet always did.
        let sheet = parse(".a { color:#445566 } button { background:#111111 }");
        let with_struct = MatchTarget::new(Some("button"), None, &["a"]).with_structure(2, 5, 1);
        // Decls are returned in first-appearance order across matched rules (lowest specificity
        // applied first): `button` (BG) folds before `.a` (FG).
        assert_eq!(
            sheet.resolve_for(&with_struct, false),
            vec![(BG, "#111111".to_string()), (FG, "#445566".to_string())],
            "structure info present but unused by a plain sheet -> same result"
        );
    }

    #[test]
    fn structural_pseudo_adds_class_level_specificity() {
        // `li:first-child` ((0,1,1)) beats a bare `li` ((0,0,1)) on the same property.
        let sheet = parse("li { color:#111111 } li:first-child { color:#222222 }");
        let first = MatchTarget::new(Some("li"), None, &[]).with_structure(0, 3, 0);
        assert_eq!(
            sheet.resolve_for(&first, false),
            vec![(FG, "#222222".to_string())],
            "li:first-child outranks bare li by the pseudo's class-level specificity"
        );
    }

    #[test]
    fn malformed_pseudo_rules_are_dropped_not_mistaken_for_a_base() {
        // A `::before` pseudo-ELEMENT (double colon -> empty pseudo name), an unbalanced `:not(`,
        // and a bogus `:nth-child(2x)` all drop their selector cleanly. The companion plain rule
        // still parses, proving the bad selector did not poison the sheet.
        for bad in [
            "p::before { color:#000000 } p { color:#abcdef }",
            "p:not(.x { color:#000000 } p { color:#abcdef }",
            "p:nth-child(2x) { color:#000000 } p { color:#abcdef }",
            "p:focus { color:#000000 } p { color:#abcdef }",
        ] {
            let sheet = parse(bad);
            let p = MatchTarget::new(Some("p"), None, &[]).with_structure(0, 1, 0);
            assert_eq!(
                sheet.resolve_for(&p, false),
                vec![(FG, "#abcdef".to_string())],
                "the bad selector in `{bad}` was dropped; the plain `p` rule still applies"
            );
        }
    }

    #[test]
    fn functional_pseudo_combines_with_a_complex_combinator() {
        // `.card :is(.a, .b)` — a functional subject under a descendant combinator. The paren-aware
        // tokenizer must keep `:is(.a, .b)` as ONE compound (its inner comma/space are opaque), so
        // the selector is `.card` (descendant) `:is(.a,.b)`.
        let sheet = parse(".card :is(.a, .b) { color:#123456 }");
        let parent_card = [ElementIdentity::new(None, None, &["card"])];
        // A `.a` nested under a `.card` matches.
        let inside = MatchTarget::new(None, None, &["a"]).with_ancestors(&parent_card);
        assert_eq!(
            sheet.resolve_for(&inside, false),
            vec![(FG, "#123456".to_string())],
            ":is(.a,.b) under a .card matches via the descendant combinator"
        );
        // A `.b` with no `.card` ancestor does NOT match (the combinator is unsatisfied).
        let no_card = MatchTarget::new(None, None, &["b"]);
        assert!(
            sheet.resolve_for(&no_card, false).is_empty(),
            ":is(.a,.b) with no .card ancestor does not match"
        );
    }

    #[test]
    fn empty_functional_arg_matches_nothing_for_is_and_everything_for_not() {
        // `:is()` (no parseable argument) matches nothing; `:not()` matches everything (vacuously —
        // none of zero inner compounds match).
        let is_sheet = parse("p:is() { color:#111111 }");
        let not_sheet = parse("p:not() { color:#222222 }");
        let p = MatchTarget::new(Some("p"), None, &[]);
        assert!(
            is_sheet.resolve_for(&p, false).is_empty(),
            ":is() matches nothing"
        );
        assert_eq!(
            not_sheet.resolve_for(&p, false),
            vec![(FG, "#222222".to_string())],
            ":not() matches everything (no inner compound to exclude)"
        );
    }
}
