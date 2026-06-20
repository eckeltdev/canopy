//! `canopy-ui` — Canopy's batteries-included authoring layer.
//!
//! The core crates ([`canopy_view::App`], [`canopy_signals`], [`canopy_style_css`],
//! [`canopy_layout_taffy`]) are deliberately small and orthogonal. Composing them by
//! hand works, but a real app ends up threading the same four things through every
//! function — the op-emitting `App`, the parsed stylesheet, a registry of which nodes
//! were styled (so a hot-reload can restyle them), and which of those react to
//! `:hover`. This crate bundles exactly that into one value, [`Ui`], and gives the
//! [`rsx!`](canopy_rsx::rsx) macro a single receiver to lower onto.
//!
//! The result is a React-shaped DX with **no second runtime**: every `Ui` method is a
//! thin, allocation-light wrapper over the same core ops a hand-written tree emits, so
//! `rsx!` and an `App` builder produce byte-identical op-streams. Signals stay
//! fine-grained (one targeted `SetText` per change); the stylesheet stays the real
//! CSS-lite engine; layout stays Taffy. `Ui` just removes the boilerplate.
//!
//! ```ignore
//! use canopy_ui::prelude::*;
//!
//! let ui = Ui::with_css(".root { background: #1e1e2e; padding: 16px } .btn { background: #313244 }");
//! let count = ui.signal(0i32);
//! let root = rsx!(ui =>
//!     <div class="root">
//!         <button class="btn"
//!             on:click={ let c = count.clone(); move |_| c.update(|n| *n += 1) }>
//!             { let c = count.clone(); move || format!("count is {}", c.get()) }
//!         </button>
//!     </div>
//! );
//! ui.mount_root(root);
//! let batch = ui.take_batch(0); // hand to a host/renderer
//! ```
//!
//! # What `Ui` tracks for you
//!
//! - **Styling**: [`Ui::class`] resolves a node's classes through the stylesheet *and*
//!   records the `(node, classes)` pair. That registry is the single source of truth
//!   for [`Ui::reload_css`] (hot-reload) — a node cannot be styled without also being
//!   reloadable, so styles never silently stop updating.
//! - **Hover**: [`Ui::hoverables`] is derived (not hand-maintained) from the registry
//!   and the stylesheet's `:hover` rules, and [`Ui::hover_target`]/[`Ui::set_hover`]
//!   drive a live hover off the cursor.
//! - **Events**: [`Ui::click_handler`] hit-tests a point to the handler that should
//!   fire, walking up to the nearest ancestor with a click listener.
//!
//! This crate is `no_std` + `alloc`: it does no I/O itself (a host reads `styles.css`
//! and passes the string to [`Ui::with_css`]/[`Ui::reload_css`]), so the same
//! authoring layer runs on a desktop host or a constrained target.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use canopy_dom::{Dom, ROOT};
use canopy_layout_taffy::{hit_test, layout};
use canopy_protocol::{ElementTag, EventPayload, HandlerId, NodeId, PropId};
use canopy_signals::{Memo, Runtime, Signal};
use canopy_style_css::Stylesheet;
use canopy_traits::{Point, Size};
use canopy_view::{App, CLICK};

/// A class list as the macro and hand-written code supply it: `'static` so it can be
/// retained in the styled registry and replayed on hover/reload without allocating.
pub type Classes = &'static [&'static str];

/// The lite-tier element identity `Ui` records per styled node so it can resolve the
/// **full** selector model (type / id / class / compound) author-side — the same triple
/// the C-ABI host builds from its retained `Dom` before calling
/// [`Stylesheet::resolve_for`](canopy_style_css::Stylesheet::resolve_for). Keeping it
/// here lets the in-process Rust/`rsx!` path honor `button { … }` / `#id { … }` /
/// `button.primary { … }` rules exactly as the freestanding render path does.
#[derive(Clone, Default)]
struct Identity {
    /// The well-known [`ElementTag`] the node was created with (COLUMN/ROW/BUTTON/INPUT or
    /// an `el(TAG)` escape hatch), used to derive a canonical CSS type name when no explicit
    /// tag-name was declared. `None` for a text leaf.
    tag: Option<ElementTag>,
    /// An explicit CSS local name declared via [`Ui::tag`] (wins over the derived name).
    tag_name: Option<String>,
    /// The CSS id declared via [`Ui::set_id`].
    id: Option<String>,
}

impl Identity {
    /// The CSS type/tag name this node matches against, mirroring `canopy-abi`'s
    /// `element_type_name`: a guest-declared name wins; otherwise the canonical name of the
    /// well-known [`ElementTag`] (COLUMN→`div`, ROW→`row`, BUTTON→`button`, INPUT→`input`).
    /// Text leaves (no tag, no name) → `None`, so the two paths agree on what participates.
    fn type_name(&self) -> Option<&str> {
        if let Some(name) = self.tag_name.as_deref() {
            return Some(name);
        }
        // The reference-host ElementTag ids (canopy-view): COLUMN=1, ROW=2, BUTTON=3, INPUT=4.
        // COLUMN is the generic flex/block container, so its CSS name is the familiar `div`.
        Some(match self.tag?.raw() {
            1 => "div",
            2 => "row",
            3 => "button",
            4 => "input",
            _ => return None,
        })
    }
}

/// The Canopy authoring context: an [`App`], its stylesheet, and the registry of
/// styled nodes — the single value the [`rsx!`](canopy_rsx::rsx) macro lowers onto and
/// a host drives.
///
/// Which tier resolves styles for this `Ui` — the two implementations of the
/// `StyleEngine` seam.
///
/// - [`Lite`](Cascade::Lite): the constrained tier. `class()` resolves rules
///   **author-side** through `canopy-style-css` and emits the resulting inline styles,
///   so the host needs no style engine. The default.
/// - [`Capable`](Cascade::Capable): the desktop/SBC tier. `class()` carries the class
///   **names** (and `tag()`/`set_id()` carry tag-name/id) to the host via the op-stream,
///   so a host-side real cascade (Stylo) can run against the retained tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Cascade {
    /// Constrained tier: resolve classes author-side to inline styles.
    #[default]
    Lite,
    /// Capable tier: carry element identity to the host for a real (Stylo) cascade.
    Capable,
}

/// The Canopy authoring context: an [`App`], its stylesheet, and the registry of
/// styled nodes — the single value the [`rsx!`](canopy_rsx::rsx) macro lowers onto and
/// a host drives.
///
/// `Ui` is `RefCell`-backed so the builder methods take `&self` (the macro emits
/// `ui.method(..)` chains against one shared `&Ui`), and a host can keep styling and
/// re-styling after the initial build (e.g. on hot-reload) through the same handle.
pub struct Ui {
    app: App,
    /// Which tier resolves styles (see [`Cascade`]).
    cascade: Cascade,
    /// The raw author CSS, kept so a capable-tier host can hand it to its style engine.
    css_src: String,
    /// The current parsed stylesheet (lite tier only). Mutable so
    /// [`reload_css`](Ui::reload_css) can swap in a freshly parsed sheet on a save.
    css: RefCell<Stylesheet>,
    /// Every `(node, classes)` styled through [`class`](Ui::class), in styling order —
    /// the registry that both `:hover` and hot-reload replay from.
    styled: RefCell<Vec<(NodeId, Classes)>>,
    /// Per-node element identity (well-known tag, explicit tag-name, id) recorded as nodes
    /// are created and as [`tag`](Ui::tag)/[`set_id`](Ui::set_id) run, so the lite tier can
    /// resolve the full selector model author-side via
    /// [`resolve_for`](canopy_style_css::Stylesheet::resolve_for) — the same identity the
    /// C-ABI host builds, so both paths style an identical sheet identically.
    identity: RefCell<BTreeMap<NodeId, Identity>>,
}

impl Ui {
    /// A `Ui` with an empty stylesheet. Classes applied before a stylesheet is loaded
    /// resolve to nothing; use [`with_css`](Ui::with_css) to start with rules.
    #[must_use]
    pub fn new() -> Self {
        Self {
            app: App::new(),
            cascade: Cascade::Lite,
            css_src: String::new(),
            css: RefCell::new(Stylesheet::new()),
            styled: RefCell::new(Vec::new()),
            identity: RefCell::new(BTreeMap::new()),
        }
    }

    /// A `Ui` whose stylesheet is parsed from `src` (CSS-lite class rules; see
    /// [`canopy_style_css`]). A host typically reads `styles.css` from disk and passes
    /// its contents here — this crate stays `no_std` and does no I/O. Constrained
    /// (lite) tier: classes resolve to inline styles author-side.
    #[must_use]
    pub fn with_css(src: &str) -> Self {
        Self {
            app: App::new(),
            cascade: Cascade::Lite,
            css_src: src.to_string(),
            css: RefCell::new(canopy_style_css::parse(src)),
            styled: RefCell::new(Vec::new()),
            identity: RefCell::new(BTreeMap::new()),
        }
    }

    /// A **capable-tier** `Ui`: instead of resolving classes author-side, the authored
    /// tree carries real element identity (class names via [`class`](Ui::class), tag
    /// names via [`tag`](Ui::tag), id via [`set_id`](Ui::set_id)) to the host, which
    /// runs a full cascade (Stylo) over the retained tree. `src` is the author CSS the
    /// host will hand to its style engine — retrieve it with [`css_source`](Ui::css_source).
    #[must_use]
    pub fn capable(src: &str) -> Self {
        Self {
            app: App::new(),
            cascade: Cascade::Capable,
            css_src: src.to_string(),
            css: RefCell::new(Stylesheet::new()),
            styled: RefCell::new(Vec::new()),
            identity: RefCell::new(BTreeMap::new()),
        }
    }

    /// This `Ui`'s style tier.
    #[must_use]
    pub fn cascade(&self) -> Cascade {
        self.cascade
    }

    /// The raw author CSS (for a capable-tier host to feed its style engine).
    #[must_use]
    pub fn css_source(&self) -> &str {
        &self.css_src
    }

    /// The underlying reactive [`App`] — its op-emitting/event surface for anything the
    /// `Ui` sugar doesn't wrap.
    #[must_use]
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The signal [`Runtime`] (for `flush` and ad-hoc effects/memos).
    #[must_use]
    pub fn runtime(&self) -> Runtime {
        self.app.runtime()
    }

    /// Create a reactive [`Signal`] in this `Ui`'s runtime.
    pub fn signal<T: Clone + 'static>(&self, value: T) -> Signal<T> {
        self.app.runtime().signal(value)
    }

    /// Create a derived [`Memo`] in this `Ui`'s runtime.
    pub fn memo<T: Clone + PartialEq + 'static>(&self, f: impl Fn() -> T + 'static) -> Memo<T> {
        self.app.runtime().memo(f)
    }

    // ---- Node builders (the surface `rsx!` lowers onto) -------------------------

    /// Record the well-known [`ElementTag`] `node` was created with, so the lite tier can
    /// later derive its canonical CSS type name for `resolve_for` (see [`Identity`]).
    fn record_tag(&self, node: NodeId, tag: ElementTag) {
        self.identity.borrow_mut().entry(node).or_default().tag = Some(tag);
    }

    /// Create a column (flex, vertical) element and return its handle.
    pub fn column(&self) -> NodeId {
        let node = self.app.el(canopy_view::COLUMN);
        self.record_tag(node, canopy_view::COLUMN);
        node
    }

    /// Create a row (flex, horizontal) element and return its handle.
    pub fn row(&self) -> NodeId {
        let node = self.app.el(canopy_view::ROW);
        self.record_tag(node, canopy_view::ROW);
        node
    }

    /// Create an element of an arbitrary host-defined `tag` (the `El(TAG)` escape
    /// hatch).
    pub fn el(&self, tag: ElementTag) -> NodeId {
        let node = self.app.el(tag);
        self.record_tag(node, tag);
        node
    }

    /// Create a text leaf with static `value`.
    pub fn label(&self, value: &str) -> NodeId {
        self.app.label(value)
    }

    /// Create a text leaf whose content is **bound** to `f`: `f` runs now and again on
    /// each change of a signal it read, emitting one `SetText` per run.
    pub fn label_bound(&self, f: impl Fn() -> String + 'static) -> NodeId {
        let node = self.app.label("");
        self.app.bind_text(node, f);
        node
    }

    /// Create a button element with a static text child, returning the **button** node.
    pub fn button(&self, text: &str) -> NodeId {
        let node = self.app.button(text);
        self.record_tag(node, canopy_view::BUTTON);
        node
    }

    /// Create a button whose text child is **bound** to `f` (the reactive counterpart
    /// of [`button`](Ui::button)); returns the button node.
    pub fn button_bound(&self, f: impl Fn() -> String + 'static) -> NodeId {
        let button = self.app.el(canopy_view::BUTTON);
        self.record_tag(button, canopy_view::BUTTON);
        let label = self.label_bound(f);
        self.app.mount(button, label);
        button
    }

    /// Create a single-line text input seeded with `initial`, returning the input node.
    pub fn input(&self, initial: &str) -> NodeId {
        let node = self.app.text_input(initial);
        self.record_tag(node, canopy_view::INPUT);
        node
    }

    /// Append `child` under `parent` (source-order, so the op-stream matches the tree).
    pub fn mount(&self, parent: NodeId, child: NodeId) {
        self.app.mount(parent, child);
    }

    /// Mount `child` under the implicit host [`ROOT`] — the top-level entry point.
    pub fn mount_root(&self, child: NodeId) {
        self.app.mount(ROOT, child);
    }

    /// Resolve `classes` onto `node` through the stylesheet **and record** the pair so
    /// hover and hot-reload can replay it. This is the only styling path `rsx!` emits,
    /// which is what keeps the reload registry exactly equal to the set of styled nodes.
    ///
    /// On the [`Lite`](Cascade::Lite) tier the rules are resolved author-side through the
    /// **full** selector model ([`resolve_for`](canopy_style_css::Stylesheet::resolve_for)):
    /// the node's recorded type-name/id (from its creating tag, [`tag`](Ui::tag), and
    /// [`set_id`](Ui::set_id)) join `classes` in a [`MatchTarget`], so `button { … }`,
    /// `#id { … }`, and compound `button.primary { … }` rules style the in-process tree
    /// exactly as they style the freestanding/C-ABI render path.
    pub fn class(&self, node: NodeId, classes: Classes) {
        match self.cascade {
            // Constrained tier: resolve the rules to inline styles author-side.
            Cascade::Lite => self.apply_node(node, classes, false),
            // Capable tier: carry the class NAMES to the host for a real cascade.
            Cascade::Capable => {
                let em = self.app.emitter();
                let mut e = em.borrow_mut();
                for class in classes {
                    e.set_class(node, class);
                }
            }
        }
        self.styled.borrow_mut().push((node, classes));
    }

    /// Resolve `classes` (with the node's recorded identity) at the given `hovered` state
    /// through the lite [`Stylesheet`] and replay the resulting inline-style ops onto
    /// `node`. The identity (type-name + id, and the id exposed as an `[id]` attribute) is what
    /// makes type/id/compound and `[id…]` attribute selectors take effect on the in-process tier;
    /// for a purely class-based sheet it folds back to the legacy class-only result.
    ///
    /// **Known limitation:** `Ui` records each node's own identity but not the parent/child tree
    /// relationship (it forwards `mount` straight to the emitter and never retains the edges), so
    /// `apply_node` has **no ancestor context** and passes an empty ancestor chain. Descendant
    /// (` `) and child (`>`) combinators therefore do not yet take effect in the in-process Rust DX
    /// path; they DO work in the freestanding/C-ABI host path (`canopy-abi`), which walks the full
    /// retained tree. For the same reason there is **no sibling-position context**, so the
    /// structural pseudo-classes (`:first-child`, `:nth-child`, `:empty`, …) are a documented no-op
    /// here (the [`MatchTarget`] carries the default `StructInfo::UNKNOWN`, against which they never
    /// match) — they too resolve only on the host path. Wiring `Ui` to retain the tree so the
    /// combinators and structural pseudos resolve author-side is a follow-up. The **functional**
    /// pseudo-classes (`:not`/`:is`/`:where`) over type/id/class/`[id…]` DO work in-process, since
    /// they only inspect the node's own identity. Attribute selectors are limited to `[id…]` (the
    /// only recorded attribute).
    fn apply_node(&self, node: NodeId, classes: &[&str], hovered: bool) {
        let ident = self
            .identity
            .borrow()
            .get(&node)
            .cloned()
            .unwrap_or_default();
        let type_name = ident.type_name();
        let id = ident.id.as_deref();
        // Expose the recorded id under its CSS attribute name so `[id="x"]` / `[id^="…"]` match.
        let attrs: Vec<(&str, &str)> = id.map(|v| ("id", v)).into_iter().collect();
        // No sibling-position context is retained in-process, so the target keeps the default
        // `StructInfo::UNKNOWN`: structural pseudos are a no-op here (see the doc above), while
        // type/id/class/attr and the functional `:not`/`:is`/`:where` pseudos resolve normally.
        let target = canopy_style_css::MatchTarget::new(type_name, id, classes).with_attrs(&attrs);
        for (prop, value) in self.css.borrow().resolve_for(&target, hovered) {
            self.app.style(node, prop, &value);
        }
    }

    /// Declare `node`'s CSS local name (e.g. `"div"`, `"button"`). On the
    /// [`Capable`](Cascade::Capable) tier this carries the name to the host for its real
    /// cascade; on the [`Lite`](Cascade::Lite) tier it is **recorded locally** so the
    /// author-side `resolve_for` matches `type` and compound selectors against it (a
    /// declared name wins over the canonical name derived from the creating tag).
    pub fn tag(&self, node: NodeId, name: &str) {
        if self.cascade == Cascade::Capable {
            self.app.emitter().borrow_mut().set_tag_name(node, name);
        } else {
            self.identity.borrow_mut().entry(node).or_default().tag_name = Some(name.to_string());
        }
    }

    /// Set `node`'s CSS id. On the [`Capable`](Cascade::Capable) tier this carries the id to
    /// the host for its real cascade; on the [`Lite`](Cascade::Lite) tier it is **recorded
    /// locally** so the author-side `resolve_for` matches `#id` and compound selectors.
    pub fn set_id(&self, node: NodeId, id: &str) {
        if self.cascade == Cascade::Capable {
            self.app
                .emitter()
                .borrow_mut()
                .set_attribute(node, canopy_protocol::AttrId::ID, id);
        } else {
            self.identity.borrow_mut().entry(node).or_default().id = Some(id.to_string());
        }
    }

    /// Register a click handler on `node`; returns the [`HandlerId`] the host echoes
    /// back when the click fires.
    pub fn on_click<F: FnMut(EventPayload) + 'static>(&self, node: NodeId, f: F) -> HandlerId {
        self.app.on_click(node, f)
    }

    /// Bind an existing text node's content to `f` (see [`label_bound`](Ui::label_bound)
    /// for the common case of creating the node and binding it in one step).
    pub fn bind_text<F: Fn() -> String + 'static>(&self, node: NodeId, f: F) {
        self.app.bind_text(node, f);
    }

    /// Bind a node's inline style property `prop` to a closure — the style counterpart
    /// of [`bind_text`](Ui::bind_text), re-emitting one `SetInlineStyle` per change. An
    /// animated `Signal<f32>` (from `canopy-anim`) formatted into the value is how a
    /// size/position/color animates on the fine-grained op path. The bound property
    /// overrides any class-resolved value for that property.
    pub fn bind_style<F: Fn() -> String + 'static>(&self, node: NodeId, prop: PropId, f: F) {
        self.app.bind_style(node, prop, f);
    }

    // ---- Host driving surface ---------------------------------------------------

    /// Drain everything emitted since the last call into a `seq` op-batch (hand it to a
    /// host/renderer to apply).
    pub fn take_batch(&self, seq: u32) -> Vec<u8> {
        self.app.take_batch(seq)
    }

    /// Deliver an event to a handler and flush dependent bindings (the host calls this
    /// on a dispatched event; typically a handler writes a signal and the flush emits
    /// the resulting `SetText` ops).
    pub fn dispatch(&self, handler: HandlerId, payload: EventPayload) {
        self.app.dispatch(handler, payload);
    }

    /// A snapshot of every styled `(node, classes)` pair, in styling order — the
    /// hot-reload registry.
    #[must_use]
    pub fn styled(&self) -> Vec<(NodeId, Classes)> {
        self.styled.borrow().clone()
    }

    /// The styled nodes that actually react to `:hover` (derived from the stylesheet,
    /// not hand-maintained): the set worth tracking under the cursor.
    #[must_use]
    pub fn hoverables(&self) -> Vec<(NodeId, Classes)> {
        let css = self.css.borrow();
        self.styled
            .borrow()
            .iter()
            .copied()
            .filter(|(_, classes)| css.reacts_to_hover(classes))
            .collect()
    }

    /// Hit-test `point` (in `viewport` logical pixels) against `dom` and return the
    /// click [`HandlerId`] that should fire — the nearest ancestor of the hit node
    /// carrying a click listener — or `None` if nothing handles it.
    ///
    /// **Tier-aware.** On the [`Lite`](Cascade::Lite) tier the `dom` carries the inline
    /// styles `class()` resolved author-side, so [`canopy_layout_taffy::layout`] runs
    /// over a fully-styled tree and the hit geometry is correct.
    ///
    /// On the [`Capable`](Cascade::Capable) tier the `dom` carries element *identity*
    /// only (tag/class/id) with **no inline styles** — the real cascade (Stylo) lives in
    /// the host, outside this crate. Running Taffy over that unstyled tree would yield
    /// silently wrong geometry, so this method returns `None` rather than a bogus hit.
    /// Capable-tier hit-testing must be driven by the host against the geometry produced
    /// by the engine that painted the frame; `canopy-ui` is a workspace member and cannot
    /// depend on `canopy-style-stylo`, so it does not — and will not — guess that geometry
    /// here.
    #[must_use]
    pub fn click_handler(&self, dom: &Dom, viewport: Size, point: Point) -> Option<HandlerId> {
        // Capable tier: the styled geometry lives in the host's engine, not in this Dom.
        if self.cascade == Cascade::Capable {
            return None;
        }
        let (_scene, lay) = layout(dom, viewport);
        let mut node = hit_test(&lay, point)?;
        loop {
            let n = dom.node(node)?;
            if let Some((_, handler)) = n.listeners.iter().find(|(ev, _)| *ev == CLICK) {
                return Some(*handler);
            }
            node = n.parent?;
        }
    }

    /// Hit-test `point` and return the nearest ancestor that is a [`hoverable`](Ui::hoverables)
    /// node, or `None`. A host compares this across pointer moves to decide when a
    /// hover crossed a boundary (then calls [`set_hover`](Ui::set_hover)).
    ///
    /// **Tier-aware**, for the same reason as [`click_handler`](Ui::click_handler): the
    /// [`Lite`](Cascade::Lite) tier's `dom` carries inline styles so Taffy geometry is
    /// correct, but the [`Capable`](Cascade::Capable) tier's `dom` is unstyled (the real
    /// cascade is the host's), so this returns `None` rather than a bogus hover target.
    /// The host drives capable-tier hover off its own engine's geometry.
    #[must_use]
    pub fn hover_target(&self, dom: &Dom, viewport: Size, point: Point) -> Option<NodeId> {
        // Capable tier: the styled geometry lives in the host's engine, not in this Dom.
        if self.cascade == Cascade::Capable {
            return None;
        }
        let hoverables = self.hoverables();
        let (_scene, lay) = layout(dom, viewport);
        let mut node = hit_test(&lay, point)?;
        loop {
            if hoverables.iter().any(|(id, _)| *id == node) {
                return Some(node);
            }
            node = dom.node(node)?.parent?;
        }
    }

    /// Re-resolve `node`'s identity + classes with the given `hovered` state and emit the
    /// resulting inline-style ops (the host applies the batch and redraws). Does
    /// nothing if `node` was not styled through this `Ui`. Resolves through the full
    /// selector model so a `:hover` rule on a type/id/compound selector restyles too.
    pub fn set_hover(&self, node: NodeId, hovered: bool) {
        let classes = self
            .styled
            .borrow()
            .iter()
            .find(|(id, _)| *id == node)
            .map(|(_, classes)| *classes);
        if let Some(classes) = classes {
            self.apply_node(node, classes, hovered);
        }
    }

    /// Hot-reload: parse `src` as the new stylesheet, replay **every** styled node's
    /// classes through it (re-resolving the node named by `hovered` *with* its `:hover`
    /// rules so a live hover survives the reload), swap it in, and return the number of
    /// nodes restyled.
    ///
    /// This only emits ops on the inner `App`; the host takes [`take_batch`](Ui::take_batch)
    /// and applies it to its [`Dom`], so a malformed reload is rejected at the
    /// capability boundary rather than corrupting the tree.
    pub fn reload_css(&self, src: &str, hovered: Option<NodeId>) -> usize {
        // Swap the freshly parsed sheet in first, then replay every styled node through the
        // full selector model (`apply_node` borrows `self.css`), so type/id/compound rules in
        // the edited sheet take effect on reload exactly as they did on the initial apply.
        *self.css.borrow_mut() = canopy_style_css::parse(src);
        let nodes: Vec<(NodeId, Classes)> = self.styled.borrow().clone();
        for (node, classes) in &nodes {
            self.apply_node(*node, classes, hovered == Some(*node));
        }
        nodes.len()
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve every node in `dom` (depth-first from [`ROOT`]) through `engine`, threading
/// each node's resolved style down to its children as the `parent` argument so CSS
/// inherited properties propagate. Returns a map from node to its [`ComputedStyle`].
///
/// This is the **tier-agnostic** consume loop: it takes `&mut dyn StyleEngine`, so the
/// *same* code drives the constrained tier ([`canopy_style_css::LiteEngine`]) and the
/// capable tier (`StyloEngine`) — swap the engine, the loop is identical. Threading the
/// parent top-down satisfies the [`StyleEngine::resolve`] inheritance contract for both:
/// the lite engine reads inherited `color`/`font-size` from it, the whole-tree engine
/// ignores it. Nodes the engine cannot resolve are simply absent from the map.
#[must_use]
pub fn resolve_tree(
    engine: &mut dyn canopy_traits::StyleEngine,
    dom: &Dom,
) -> alloc::collections::BTreeMap<NodeId, canopy_traits::ComputedStyle> {
    let mut out = alloc::collections::BTreeMap::new();
    resolve_subtree(engine, dom, ROOT, None, &mut out);
    out
}

/// Resolve `node`'s children under `parent`'s style, then recurse (see [`resolve_tree`]).
fn resolve_subtree(
    engine: &mut dyn canopy_traits::StyleEngine,
    dom: &Dom,
    node: NodeId,
    parent: Option<canopy_traits::ComputedStyle>,
    out: &mut alloc::collections::BTreeMap<NodeId, canopy_traits::ComputedStyle>,
) {
    for &child in dom.children(node) {
        let style = engine.resolve(child, parent.as_ref()).ok();
        if let Some(s) = style {
            out.insert(child, s);
        }
        resolve_subtree(engine, dom, child, style, out);
    }
}

/// One-line import for authoring: the [`Ui`] context, the [`rsx!`](canopy_rsx::rsx)
/// macro, the reactive primitives, and the common host types.
pub mod prelude {
    pub use crate::{resolve_tree, Classes, Ui};
    pub use canopy_dom::{Dom, ROOT};
    pub use canopy_protocol::{EventPayload, HandlerId, NodeId, PropId};
    pub use canopy_rsx::rsx;
    pub use canopy_signals::{Memo, Runtime, Signal};
    pub use canopy_style_css::LiteEngine;
    pub use canopy_traits::{ComputedStyle, Point, Size, StyleEngine};
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_paint::{BG, FG};
    use canopy_style_css::LiteEngine;
    use canopy_traits::OpSink;
    use canopy_view::CLICK;

    const CSS: &str = "
        .root { background: #1e1e2e; padding: 8px }
        .btn  { background: #313244; color: #cdd6f4 }
        .btn:hover { background: #585b70 }
        .plain { color: #ffffff }
    ";

    fn mount(ui: &Ui) -> Dom {
        let mut dom = Dom::new();
        dom.apply(&ui.take_batch(0)).expect("mount batch");
        dom
    }

    #[test]
    fn resolve_tree_threads_parent_through_dyn_styleengine() {
        use canopy_traits::{ComputedStyle, HostError, StyleEngine};
        // A stub engine whose output is purely depth-derived, proving the helper threads
        // each resolved parent into its children through `&mut dyn StyleEngine`.
        struct DepthEngine;
        impl StyleEngine for DepthEngine {
            fn resolve(
                &mut self,
                _node: NodeId,
                parent: Option<&ComputedStyle>,
            ) -> Result<ComputedStyle, HostError> {
                let base = parent.map_or(10.0, |p| p.font_size);
                Ok(ComputedStyle {
                    font_size: base + 1.0,
                    ..ComputedStyle::default()
                })
            }
        }

        let ui = Ui::new();
        let a = ui.column();
        let b = ui.column();
        ui.mount(a, b);
        ui.mount_root(a);
        let dom = mount(&ui);

        let mut engine = DepthEngine;
        let styles = resolve_tree(&mut engine, &dom);
        assert_eq!(
            styles.get(&a).unwrap().font_size,
            11.0,
            "child of ROOT resolves with parent = None"
        );
        assert_eq!(
            styles.get(&b).unwrap().font_size,
            12.0,
            "grandchild inherits its parent's resolved style"
        );
    }

    #[test]
    fn resolve_tree_drives_the_real_lite_engine() {
        // The constrained tier through the same helper: a capable-authored Dom (carrying
        // class identity) resolves to ComputedStyle via LiteEngine behind the trait.
        let ui = Ui::capable(".card { background: #1c2030 }");
        let card = ui.column();
        ui.tag(card, "div");
        ui.class(card, &["card"]);
        ui.mount_root(card);
        let dom = mount(&ui);

        let mut engine = LiteEngine::from_dom(&dom, ui.css_source());
        let styles = resolve_tree(&mut engine, &dom);
        assert_eq!(
            styles.get(&card).unwrap().background,
            canopy_traits::Color {
                r: 0x1c,
                g: 0x20,
                b: 0x30,
                a: 255
            }
        );
    }

    /// A sheet driven entirely by the NEW selector model (type / id / compound), with no
    /// pure-class rule — proving the in-process lite path now honors it via `resolve_for`.
    const SELECTORS: &str = "
        button       { background: #111111 }
        #submit      { color: #222222 }
        button.primary { padding: 4px }
        div          { background: #333333 }
    ";

    #[test]
    fn lite_type_selector_styles_in_process() {
        // A `button { … }` TYPE rule. Before routing through resolve_for the lite tier
        // matched class-only, so a button authored from Rust got no `button {}` styling.
        let ui = Ui::with_css(SELECTORS);
        let btn = ui.button("ok");
        ui.class(btn, &[]); // styled with NO classes — only the type selector can match
        ui.mount_root(btn);
        let dom = mount(&ui);
        assert_eq!(
            dom.style(btn, BG),
            Some("#111111"),
            "button{{}} type rule styles the in-process button"
        );
    }

    #[test]
    fn lite_column_type_name_is_div() {
        // COLUMN's canonical CSS type name is `div` (mirrors canopy-abi), so `div { … }`
        // must match a `ui.column()` even though no class or tag-name was set.
        let ui = Ui::with_css(SELECTORS);
        let col = ui.column();
        ui.class(col, &[]);
        ui.mount_root(col);
        let dom = mount(&ui);
        assert_eq!(
            dom.style(col, BG),
            Some("#333333"),
            "COLUMN derives the `div` type name, so `div{{}}` matches"
        );
    }

    #[test]
    fn lite_id_selector_styles_in_process() {
        // An `#submit { … }` ID rule applied via set_id on the lite tier (previously a no-op).
        let ui = Ui::with_css(SELECTORS);
        let btn = ui.button("ok");
        ui.set_id(btn, "submit");
        ui.class(btn, &[]);
        ui.mount_root(btn);
        let dom = mount(&ui);
        assert_eq!(
            dom.style(btn, FG),
            Some("#222222"),
            "#submit id rule styles the in-process node"
        );
    }

    #[test]
    fn lite_compound_selector_styles_in_process() {
        // A compound `button.primary { … }` — needs BOTH the type and the class to match.
        const PAD: PropId = canopy_paint::PADDING;
        let ui = Ui::with_css(SELECTORS);

        let primary = ui.button("ok");
        ui.class(primary, &["primary"]);
        ui.mount_root(primary);

        // A non-button with `.primary` must NOT pick up the compound rule.
        let div = ui.column();
        ui.class(div, &["primary"]);
        ui.mount_root(div);

        let dom = mount(&ui);
        // The lite parser normalizes `4px` length values to the bare number `4`.
        assert_eq!(
            dom.style(primary, PAD),
            Some("4"),
            "button.primary compound rule matches the button"
        );
        assert_eq!(
            dom.style(div, PAD),
            None,
            "compound rule does not match a non-button with the same class"
        );
        // The button also still gets the bare `button {}` rule (cascade across selectors).
        assert_eq!(dom.style(primary, BG), Some("#111111"));
    }

    #[test]
    fn lite_id_attribute_selector_styles_in_process() {
        // The recorded id is exposed under its CSS attribute name `id`, so an `[id="…"]` attribute
        // selector (distinct from the `#submit` id selector) styles the in-process node too.
        const SHEET: &str =
            "[id=\"submit\"] { background: #abcdef } [id^=\"sub\"] { color: #102030 }";
        let ui = Ui::with_css(SHEET);
        let btn = ui.button("ok");
        ui.set_id(btn, "submit");
        ui.class(btn, &[]);
        ui.mount_root(btn);
        let dom = mount(&ui);
        assert_eq!(
            dom.style(btn, BG),
            Some("#abcdef"),
            "[id=\"submit\"] exact attribute selector matches the recorded id"
        );
        assert_eq!(
            dom.style(btn, FG),
            Some("#102030"),
            "[id^=\"sub\"] prefix attribute selector matches the recorded id"
        );
    }

    #[test]
    fn lite_descendant_combinator_is_a_documented_in_process_limitation() {
        // KNOWN LIMITATION: `Ui` records each node's own identity but not the parent/child tree,
        // so `apply_node` passes NO ancestor context. A descendant selector therefore does NOT
        // style the in-process tree (it DOES work on the freestanding/C-ABI host path, which walks
        // the full retained tree). This test pins that documented behavior; flip it when `Ui` is
        // wired to retain the tree.
        const SHEET: &str = ".card .title { background: #abcdef }";
        let ui = Ui::with_css(SHEET);
        let card = ui.column();
        ui.class(card, &["card"]);
        ui.mount_root(card);
        let title = ui.column();
        ui.class(title, &["title"]);
        ui.mount(card, title);
        let dom = mount(&ui);
        assert_eq!(
            dom.style(title, BG),
            None,
            "descendant combinators are not yet resolved author-side (no ancestor context in Ui)"
        );
    }

    #[test]
    fn lite_structural_pseudo_is_a_documented_in_process_no_op() {
        // KNOWN LIMITATION: `Ui` does not retain tree edges, so `apply_node` has no sibling-position
        // context and the target carries `StructInfo::UNKNOWN`. A structural pseudo therefore never
        // matches in-process (it DOES work on the freestanding/C-ABI host path). This test pins that
        // documented behavior; flip it when `Ui` is wired to retain the tree.
        const SHEET: &str = "button:first-child { background: #abcdef } \
                             button:nth-child(2n) { background: #fedcba } \
                             button:empty { color: #102030 }";
        let ui = Ui::with_css(SHEET);
        let btn = ui.button("ok");
        ui.class(btn, &[]);
        ui.mount_root(btn);
        let dom = mount(&ui);
        assert_eq!(
            dom.style(btn, BG),
            None,
            "structural pseudos are a no-op in-process (no sibling-position context in Ui)"
        );
        assert_eq!(
            dom.style(btn, FG),
            None,
            ":empty is likewise a no-op in-process"
        );
    }

    #[test]
    fn lite_not_over_class_works_in_process() {
        // The functional `:not(.x)` over a CLASS needs only the node's own identity, so it resolves
        // author-side. A button WITHOUT `.disabled` is styled; one WITH it is excluded.
        const SHEET: &str = "button:not(.disabled) { background: #abcdef }";
        let ui = Ui::with_css(SHEET);
        let btn = ui.button("ok");
        ui.class(btn, &[]);
        ui.mount_root(btn);
        let dom = mount(&ui);
        assert_eq!(
            dom.style(btn, BG),
            Some("#abcdef"),
            "button:not(.disabled) styles a plain button in-process"
        );

        let ui = Ui::with_css(SHEET);
        let btn = ui.button("ok");
        ui.class(btn, &["disabled"]);
        ui.mount_root(btn);
        let dom = mount(&ui);
        assert_eq!(
            dom.style(btn, BG),
            None,
            ":not(.disabled) excludes a .disabled button in-process"
        );
    }

    #[test]
    fn lite_is_over_type_and_class_works_in_process() {
        // `:is(button, .tag)` matches either a button TYPE or a `.tag` class — both resolvable from
        // the node's own identity, so it works author-side.
        const SHEET: &str = ":is(button, .tag) { background: #224466 }";
        // A button (type arm) matches.
        let ui = Ui::with_css(SHEET);
        let btn = ui.button("ok");
        ui.class(btn, &[]);
        ui.mount_root(btn);
        assert_eq!(
            mount(&ui).style(btn, BG),
            Some("#224466"),
            ":is matches the button via its type arm"
        );
        // A column carrying `.tag` (class arm) matches; one without it does not.
        let ui = Ui::with_css(SHEET);
        let tagged = ui.column();
        ui.class(tagged, &["tag"]);
        ui.mount_root(tagged);
        let plain = ui.column();
        ui.class(plain, &[]);
        ui.mount(tagged, plain);
        let dom = mount(&ui);
        assert_eq!(
            dom.style(tagged, BG),
            Some("#224466"),
            ":is matches the .tag column via its class arm"
        );
        assert_eq!(
            dom.style(plain, BG),
            None,
            ":is does not match a column with neither the type nor the class"
        );
    }

    #[test]
    fn lite_explicit_tag_overrides_derived_name() {
        // ui.tag() on the lite tier records an explicit CSS local name that wins over the
        // tag-derived one, so a COLUMN tagged "button" matches `button {}` not `div {}`.
        let ui = Ui::with_css(SELECTORS);
        let node = ui.column();
        ui.tag(node, "button");
        ui.class(node, &[]);
        ui.mount_root(node);
        let dom = mount(&ui);
        assert_eq!(
            dom.style(node, BG),
            Some("#111111"),
            "declared tag-name `button` wins over the COLUMN-derived `div`"
        );
    }

    #[test]
    fn lite_selectors_survive_reload_and_hover() {
        // reload_css and set_hover both go through resolve_for, so a type/id/compound `:hover`
        // rule restyles the in-process tree too (not just class-based ones).
        const SHEET: &str = "button { background: #111111 } button:hover { background: #999999 }";
        let ui = Ui::with_css(SHEET);
        let btn = ui.button("ok");
        ui.class(btn, &[]);
        ui.mount_root(btn);
        let mut dom = mount(&ui);
        assert_eq!(dom.style(btn, BG), Some("#111111"));

        // Type-selector :hover restyles the button.
        ui.set_hover(btn, true);
        dom.apply(&ui.take_batch(1)).unwrap();
        assert_eq!(
            dom.style(btn, BG),
            Some("#999999"),
            "button:hover type rule restyles on hover"
        );

        // A reload replays identity through the new sheet.
        let n = ui.reload_css("button { background: #abcdef }", None);
        assert_eq!(n, 1);
        dom.apply(&ui.take_batch(2)).unwrap();
        assert_eq!(
            dom.style(btn, BG),
            Some("#abcdef"),
            "reload re-resolves the type selector against the edited sheet"
        );
    }

    #[test]
    fn class_styles_and_records() {
        let ui = Ui::with_css(CSS);
        let root = ui.column();
        ui.class(root, &["root"]);
        ui.mount_root(root);
        let dom = mount(&ui);

        // The class resolved to inline styles on the node...
        assert_eq!(dom.style(root, BG), Some("#1e1e2e"));
        // ...and the node was recorded for reload.
        assert_eq!(ui.styled().len(), 1);
        assert_eq!(ui.styled()[0].0, root);
    }

    #[test]
    fn hoverables_are_derived_from_hover_rules() {
        let ui = Ui::with_css(CSS);
        let btn = ui.button("ok");
        ui.class(btn, &["btn"]);
        let plain = ui.label("hi");
        ui.class(plain, &["plain"]);
        ui.mount_root(btn);

        // Only `.btn` has a `:hover` rule, so only the button is hoverable.
        let hov = ui.hoverables();
        assert_eq!(hov.len(), 1);
        assert_eq!(hov[0].0, btn);
    }

    #[test]
    fn set_hover_swaps_the_background() {
        let ui = Ui::with_css(CSS);
        let btn = ui.button("ok");
        ui.class(btn, &["btn"]);
        ui.mount_root(btn);
        let mut dom = mount(&ui);
        assert_eq!(dom.style(btn, BG), Some("#313244"));

        ui.set_hover(btn, true);
        dom.apply(&ui.take_batch(1)).unwrap();
        assert_eq!(dom.style(btn, BG), Some("#585b70"), "hover lightens");
    }

    #[test]
    fn reload_restyles_every_node() {
        let ui = Ui::with_css(CSS);
        let btn = ui.button("ok");
        ui.class(btn, &["btn"]);
        ui.mount_root(btn);
        let mut dom = mount(&ui);
        assert_eq!(dom.style(btn, FG), Some("#cdd6f4"));

        let edited =
            ".btn { background: #313244; color: #f9e2af } .btn:hover { background: #585b70 }";
        let n = ui.reload_css(edited, None);
        assert_eq!(n, 1);
        dom.apply(&ui.take_batch(1)).unwrap();
        assert_eq!(
            dom.style(btn, FG),
            Some("#f9e2af"),
            "reload changed the color"
        );
    }

    #[test]
    fn capable_tier_carries_identity_not_inline_styles() {
        // In capable mode, class()/tag()/set_id() carry element IDENTITY to the host
        // (for a real host-side cascade) instead of expanding classes to inline styles.
        let ui = Ui::capable(CSS);
        let btn = ui.button("ok");
        ui.tag(btn, "button");
        ui.class(btn, &["btn"]);
        ui.set_id(btn, "submit");
        ui.mount_root(btn);
        let dom = mount(&ui);

        // The host retained the real identity (tag / classes / id) ...
        assert_eq!(dom.tag_name(btn), Some("button"));
        assert_eq!(dom.classes(btn), &["btn".to_string()]);
        assert_eq!(dom.id(btn), Some("submit"));
        // ... and did NOT receive pre-expanded inline styles (the lite tier would have).
        assert_eq!(
            dom.style(btn, FG),
            None,
            "capable tier does not expand classes"
        );
        // The author CSS is available for the host's style engine.
        assert_eq!(ui.css_source(), CSS);
    }

    #[test]
    fn hit_test_is_tier_aware_lite_hits_capable_defers() {
        // A clickable, sized button authored the SAME way on both tiers. Only the cascade
        // differs: lite expands `.box` to inline width/height (so Taffy lays it out and the
        // hit lands), capable carries identity only (the styled geometry lives in the host's
        // engine, outside this crate). The honest answer on capable is "no hit here", NOT a
        // bogus one computed over an unstyled tree.
        const SIZED: &str = ".box { width: 40px; height: 20px }";
        let viewport = Size { w: 100.0, h: 100.0 };
        let inside = Point { x: 5.0, y: 5.0 };

        // --- Lite tier: Dom carries inline styles -> Taffy geometry is correct, hit lands.
        let lite = Ui::with_css(SIZED);
        let btn = lite.button("ok");
        lite.class(btn, &["box"]);
        let handler = lite.on_click(btn, |_| {});
        lite.mount_root(btn);
        let dom = mount(&lite);
        assert_eq!(
            lite.click_handler(&dom, viewport, inside),
            Some(handler),
            "lite tier hit-tests over the inline-styled Dom"
        );
        assert_eq!(
            lite.hover_target(&dom, viewport, inside),
            None,
            "no :hover rule, so no hover target (but the path ran over real geometry)"
        );

        // --- Capable tier: Dom carries identity only, NO inline styles. Running Taffy here
        // would silently mis-place the box, so both helpers defer to the host with None.
        let capable = Ui::capable(SIZED);
        let cbtn = capable.button("ok");
        capable.tag(cbtn, "button");
        capable.class(cbtn, &["box"]);
        let chandler = capable.on_click(cbtn, |_| {});
        capable.mount_root(cbtn);
        let cdom = mount(&capable);

        // The click listener is still registered (so the host can route to it once it has
        // hit a node via the capable engine) ...
        assert_eq!(capable.cascade(), Cascade::Capable);
        let _ = chandler;
        // ... but this crate refuses to guess geometry it cannot compute.
        assert_eq!(
            capable.click_handler(&cdom, viewport, inside),
            None,
            "capable-tier hit-testing is the host's job (it owns the painting engine)"
        );
        assert_eq!(
            capable.hover_target(&cdom, viewport, inside),
            None,
            "capable-tier hover is likewise host-driven"
        );
    }

    #[test]
    fn bound_label_tracks_the_signal() {
        let ui = Ui::new();
        let count = ui.signal(0i32);
        let label = {
            let count = count.clone();
            ui.label_bound(move || {
                let mut s = String::from("count is ");
                s.push_str(itoa(count.get()).as_str());
                s
            })
        };
        ui.mount_root(label);
        let mut dom = mount(&ui);
        assert_eq!(dom.text_of(label), Some("count is 0"));

        count.set(5);
        ui.runtime().flush();
        dom.apply(&ui.take_batch(1)).unwrap();
        assert_eq!(dom.text_of(label), Some("count is 5"));
    }

    #[test]
    fn bind_style_tracks_a_signal() {
        let ui = Ui::new();
        let node = ui.column();
        let w = ui.signal(0i32);
        {
            let w = w.clone();
            ui.bind_style(node, BG, move || {
                // A signal-driven style value (here just a number formatted as text).
                let mut s = String::from("#0000");
                s.push((b'0' + (w.get() as u8 % 10)) as char);
                s.push('0');
                s
            });
        }
        ui.mount_root(node);
        let mut dom = mount(&ui);
        assert_eq!(dom.style(node, BG), Some("#000000"));

        w.set(5);
        ui.runtime().flush();
        dom.apply(&ui.take_batch(1)).unwrap();
        assert_eq!(
            dom.style(node, BG),
            Some("#000050"),
            "style re-emitted on change"
        );
    }

    #[test]
    fn button_carries_a_click_listener() {
        let ui = Ui::new();
        let count = ui.signal(0i32);
        let btn = ui.button("inc");
        {
            let count = count.clone();
            ui.on_click(btn, move |_| count.update(|n| *n += 1));
        }
        ui.mount_root(btn);
        let dom = mount(&ui);
        let node = dom.node(btn).unwrap();
        assert!(node.listeners.iter().any(|(ev, _)| *ev == CLICK));
    }

    /// Tiny no_std integer formatter for the test above (avoids pulling `format!`
    /// machinery into the assertion).
    fn itoa(mut n: i32) -> String {
        if n == 0 {
            return String::from("0");
        }
        let neg = n < 0;
        let mut buf = Vec::new();
        if neg {
            n = -n;
        }
        while n > 0 {
            buf.push(b'0' + (n % 10) as u8);
            n /= 10;
        }
        if neg {
            buf.push(b'-');
        }
        buf.reverse();
        String::from_utf8(buf).unwrap()
    }
}
