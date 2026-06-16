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

    /// Create a column (flex, vertical) element and return its handle.
    pub fn column(&self) -> NodeId {
        self.app.el(canopy_view::COLUMN)
    }

    /// Create a row (flex, horizontal) element and return its handle.
    pub fn row(&self) -> NodeId {
        self.app.el(canopy_view::ROW)
    }

    /// Create an element of an arbitrary host-defined `tag` (the `El(TAG)` escape
    /// hatch).
    pub fn el(&self, tag: ElementTag) -> NodeId {
        self.app.el(tag)
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
        self.app.button(text)
    }

    /// Create a button whose text child is **bound** to `f` (the reactive counterpart
    /// of [`button`](Ui::button)); returns the button node.
    pub fn button_bound(&self, f: impl Fn() -> String + 'static) -> NodeId {
        let button = self.app.el(canopy_view::BUTTON);
        let label = self.label_bound(f);
        self.app.mount(button, label);
        button
    }

    /// Create a single-line text input seeded with `initial`, returning the input node.
    pub fn input(&self, initial: &str) -> NodeId {
        self.app.text_input(initial)
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
    pub fn class(&self, node: NodeId, classes: Classes) {
        match self.cascade {
            // Constrained tier: resolve the rules to inline styles author-side.
            Cascade::Lite => self.css.borrow().apply(&self.app, node, classes),
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

    /// Declare `node`'s CSS local name (e.g. `"div"`, `"button"`) for a host-side
    /// cascade. No-op on the lite tier (which has no host cascade to use it).
    pub fn tag(&self, node: NodeId, name: &str) {
        if self.cascade == Cascade::Capable {
            self.app.emitter().borrow_mut().set_tag_name(node, name);
        }
    }

    /// Set `node`'s CSS id for a host-side cascade. No-op on the lite tier.
    pub fn set_id(&self, node: NodeId, id: &str) {
        if self.cascade == Cascade::Capable {
            self.app
                .emitter()
                .borrow_mut()
                .set_attribute(node, canopy_protocol::AttrId::ID, id);
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
    #[must_use]
    pub fn click_handler(&self, dom: &Dom, viewport: Size, point: Point) -> Option<HandlerId> {
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
    #[must_use]
    pub fn hover_target(&self, dom: &Dom, viewport: Size, point: Point) -> Option<NodeId> {
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

    /// Re-resolve `node`'s classes with the given `hovered` state and emit the
    /// resulting inline-style ops (the host applies the batch and redraws). Does
    /// nothing if `node` was not styled through this `Ui`.
    pub fn set_hover(&self, node: NodeId, hovered: bool) {
        let css = self.css.borrow();
        if let Some((_, classes)) = self.styled.borrow().iter().find(|(id, _)| *id == node) {
            css.apply_state(&self.app, node, classes, hovered);
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
        let css = canopy_style_css::parse(src);
        let styled = self.styled.borrow();
        for (node, classes) in styled.iter() {
            css.apply_state(&self.app, *node, classes, hovered == Some(*node));
        }
        let n = styled.len();
        drop(styled);
        *self.css.borrow_mut() = css;
        n
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

/// One-line import for authoring: the [`Ui`] context, the [`rsx!`](canopy_rsx::rsx)
/// macro, the reactive primitives, and the common host types.
pub mod prelude {
    pub use crate::{Classes, Ui};
    pub use canopy_dom::{Dom, ROOT};
    pub use canopy_protocol::{EventPayload, HandlerId, NodeId, PropId};
    pub use canopy_rsx::rsx;
    pub use canopy_signals::{Memo, Runtime, Signal};
    pub use canopy_traits::{Point, Size};
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_paint::{BG, FG};
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
