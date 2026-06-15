//! Canopy's signal-based reactivity layer.
//!
//! [`App`] ties the [`canopy_signals`] runtime to a shared [`canopy_core::Emitter`]
//! so the reactive model is: **a changed signal emits one targeted op**, not a
//! whole-tree diff. Reading a signal inside a binding subscribes it; writing it
//! (e.g. from an event handler) re-runs only the dependent bindings on the next
//! flush, each emitting a single mutation (e.g. one `SetText`).
//!
//! This is the shared engine that every language wrapper drives — the first-party
//! Rust `rsx!` macro will lower to exactly these `App` calls, and a C/Zig/Swift
//! wrapper binds the same surface over the ABI, so good DX is not Rust-only.
//!
//! `no_std` + `alloc`; single-threaded by design (matches the WASM guest and the
//! constrained-target loop). The op bytes it produces are transport-agnostic.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use canopy_core::Emitter;
use canopy_protocol::{ElementTag, EventKind, EventPayload, HandlerId, NodeId, PropId};
use canopy_signals::Runtime;

/// A shared, mutable [`Emitter`] that reactive bindings write into.
pub type SharedEmitter = Rc<RefCell<Emitter>>;

/// Well-known element kind: a vertical stack.
///
/// These ids are a **convention** a host/demo agrees on — the protocol leaves
/// [`ElementTag`] values to the host widget registry (see `canopy-protocol`). The
/// first-party demo and the `canopy-paint` stack use exactly these, so the builder
/// helpers and the [`rsx!`] examples below are immediately runnable end-to-end.
pub const COLUMN: ElementTag = ElementTag::new(1);
/// Well-known element kind: a horizontal stack. See [`COLUMN`] for why these ids
/// are a host convention rather than a protocol-level constant.
pub const ROW: ElementTag = ElementTag::new(2);
/// Well-known element kind: a clickable button. See [`COLUMN`] for the convention.
pub const BUTTON: ElementTag = ElementTag::new(3);
/// Well-known event kind: a pointer click. See [`COLUMN`] for why this is a host
/// convention; [`App::on_click`] is sugar for `on(node, CLICK, ..)`.
pub const CLICK: EventKind = EventKind::new(1);

type HandlerMap = Rc<RefCell<BTreeMap<HandlerId, Box<dyn FnMut(EventPayload)>>>>;

/// The reactive application surface: a signal runtime, an op-stream emitter, and an
/// event-handler registry, wired so signal writes turn into targeted ops.
pub struct App {
    rt: Runtime,
    emitter: SharedEmitter,
    handlers: HandlerMap,
    next_handler: Cell<u32>,
}

impl App {
    /// Create a new app with a fresh runtime and emitter.
    pub fn new() -> Self {
        Self {
            rt: Runtime::new(),
            emitter: Rc::new(RefCell::new(Emitter::new())),
            handlers: Rc::new(RefCell::new(BTreeMap::new())),
            next_handler: Cell::new(0),
        }
    }

    /// The reactive runtime (clone shares state).
    pub fn runtime(&self) -> Runtime {
        self.rt.clone()
    }

    /// The shared emitter (clone shares state).
    pub fn emitter(&self) -> SharedEmitter {
        self.emitter.clone()
    }

    /// Create an element node and return its handle.
    pub fn element(&self, tag: ElementTag) -> NodeId {
        self.emitter.borrow_mut().create_element(tag)
    }

    /// Create a text node with initial content and return its handle.
    pub fn text(&self, initial: &str) -> NodeId {
        self.emitter.borrow_mut().create_text(initial)
    }

    /// Append `child` to `parent`.
    pub fn append(&self, parent: NodeId, child: NodeId) {
        self.emitter.borrow_mut().append(parent, child);
    }

    /// Set an inline style property on a node.
    pub fn style(&self, node: NodeId, prop: PropId, value: &str) {
        self.emitter
            .borrow_mut()
            .set_inline_style(node, prop, value);
    }

    /// Bind a text node to a closure. `f` is run now (subscribing the signals it
    /// reads) and again after any of those signals change, each run emitting one
    /// `SetText`. This is the fine-grained reactive hot path.
    pub fn bind_text<F: Fn() -> String + 'static>(&self, node: NodeId, f: F) {
        let emitter = self.emitter.clone();
        self.rt.create_effect(move || {
            let value = f();
            emitter.borrow_mut().set_text(node, &value);
        });
    }

    /// Subscribe `node` to `event`, registering `handler`. Returns the handler id
    /// the host echoes back when the event fires. The `AddListener` op is emitted so
    /// the host knows to route the event.
    pub fn on<F: FnMut(EventPayload) + 'static>(
        &self,
        node: NodeId,
        event: EventKind,
        handler: F,
    ) -> HandlerId {
        let id = HandlerId::new(self.next_handler.get());
        self.next_handler.set(self.next_handler.get() + 1);
        self.emitter.borrow_mut().add_listener(node, event, id);
        self.handlers.borrow_mut().insert(id, Box::new(handler));
        id
    }

    /// Deliver an event to a handler, then flush. In a real host this is called by
    /// the transport loop when the host dispatches a `DispatchEvent` back to the
    /// guest. The handler typically writes a signal; the flush re-runs dependent
    /// bindings, which emit the resulting ops.
    pub fn dispatch(&self, handler: HandlerId, payload: EventPayload) {
        // Take the handler out so it may re-enter the app without a borrow clash.
        let taken = self.handlers.borrow_mut().remove(&handler);
        if let Some(mut f) = taken {
            f(payload);
            self.handlers.borrow_mut().insert(handler, f);
        }
        self.rt.flush();
    }

    /// Wrap and drain everything emitted since the last call into a `seq` batch.
    pub fn take_batch(&self, seq: u32) -> Vec<u8> {
        self.emitter.borrow_mut().take_batch(seq)
    }

    // ---- Ergonomic UI builders -------------------------------------------
    //
    // These are thin, allocation-free sugar over the primitive ops above
    // (`element`/`text`/`append`/`style`/`on`). They exist so hand-written
    // UI and the [`rsx!`] macro read like a tree, while still lowering to the
    // exact same targeted op-stream — there is no second code path.

    /// Create an element node of `tag` and return its handle. Alias for
    /// [`App::element`] that reads naturally inside a builder chain.
    pub fn el(&self, tag: ElementTag) -> NodeId {
        self.element(tag)
    }

    /// Append `child` to `parent`. Alias for [`App::append`]; named `mount`
    /// because that is how the [`rsx!`] macro reads when nesting subtrees.
    pub fn mount(&self, parent: NodeId, child: NodeId) {
        self.append(parent, child);
    }

    /// Create a standalone text node with `value` and return its handle. Alias for
    /// [`App::text`].
    pub fn label(&self, value: &str) -> NodeId {
        self.text(value)
    }

    /// Create a [`BUTTON`] element with a single text child set to `text`, append the
    /// text to the button, and return the **button** node (so the caller can mount it
    /// and attach an [`App::on_click`] handler).
    pub fn button(&self, text: &str) -> NodeId {
        let btn = self.element(BUTTON);
        let lbl = self.text(text);
        self.append(btn, lbl);
        btn
    }

    /// Subscribe `node` to [`CLICK`], registering `f`. Sugar for
    /// `self.on(node, CLICK, f)`; returns the handler id the host echoes back.
    pub fn on_click<F: FnMut(EventPayload) + 'static>(&self, node: NodeId, f: F) -> HandlerId {
        self.on(node, CLICK, f)
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// Declarative subtree builder that lowers to [`App`] calls and returns the **root**
/// [`NodeId`] of the subtree it built.
///
/// `rsx!` is intentionally decoupled from `canopy-paint`: it never hard-codes a
/// [`PropId`] — style attributes are written as `(prop_expr => "value")` pairs, so the
/// caller supplies whatever property ids their host understands. Everything it emits
/// goes through `el`/`text`/`style`/`mount`, i.e. the same ops a hand-written tree
/// produces. The macro does **not** append the returned root to anything; the caller
/// mounts it where they want (often the host root).
///
/// # Grammar
///
/// ```text
/// rsx!($app, $tag)                              // bare element
/// rsx!($app, $tag, [ $( ($prop => $val) ),* ])  // element + inline styles
/// rsx!($app, $tag, { $($child)* })              // element + children
/// rsx!($app, $tag, [ styles ], { $($child)* })  // element + styles + children
/// ```
///
/// `$app` is an expression of type `&App` (or anything that derefs to it). `$tag` is
/// an [`ElementTag`] expression (e.g. [`COLUMN`]). Each `$prop` is a [`PropId`]
/// expression and `$val` a `&str`. A `$child` is one of:
///
/// - `text("literal")` — a text leaf appended to this element;
/// - `node($expr)` — an already-built [`NodeId`] (e.g. from [`App::button`])
///   appended to this element;
/// - `rsx!($app, ...)` — a nested element, recursively built and appended.
///
/// Children are appended left-to-right, so the emitted op order matches source order.
///
/// # Example
///
/// ```
/// use canopy_view::{rsx, App, COLUMN, ROW};
/// use canopy_protocol::PropId;
///
/// const BG: PropId = PropId::new(1);
///
/// let app = App::new();
/// let root = rsx!(&app, COLUMN, [ (BG => "#101010") ], {
///     text("Canopy");
///     rsx!(&app, ROW, {
///         node(app.button("OK"));
///     });
/// });
/// // `root` is the column; mount it wherever the host wants it.
/// app.mount(canopy_protocol::NodeId::new(0), root);
/// ```
#[macro_export]
macro_rules! rsx {
    // element + styles + children
    ($app:expr, $tag:expr, [ $( ($prop:expr => $val:expr) ),* $(,)? ], { $($child:tt)* }) => {{
        let __app = &$app;
        let __node = __app.el($tag);
        $( __app.style(__node, $prop, $val); )*
        $crate::__rsx_children!(__app, __node, $($child)*);
        __node
    }};
    // element + children
    ($app:expr, $tag:expr, { $($child:tt)* }) => {{
        let __app = &$app;
        let __node = __app.el($tag);
        $crate::__rsx_children!(__app, __node, $($child)*);
        __node
    }};
    // element + styles
    ($app:expr, $tag:expr, [ $( ($prop:expr => $val:expr) ),* $(,)? ]) => {{
        let __app = &$app;
        let __node = __app.el($tag);
        $( __app.style(__node, $prop, $val); )*
        __node
    }};
    // bare element
    ($app:expr, $tag:expr $(,)?) => {{
        (&$app).el($tag)
    }};
}

/// Internal: expand the child list of an [`rsx!`] element, appending each child to the
/// already-created parent. Not part of the public grammar — call [`rsx!`] instead.
#[doc(hidden)]
#[macro_export]
macro_rules! __rsx_children {
    // done
    ($app:expr, $parent:expr, ) => {};
    // text leaf
    ($app:expr, $parent:expr, text($val:expr); $($rest:tt)*) => {
        let __child = $app.label($val);
        $app.mount($parent, __child);
        $crate::__rsx_children!($app, $parent, $($rest)*);
    };
    // an already-built NodeId
    ($app:expr, $parent:expr, node($val:expr); $($rest:tt)*) => {
        let __child = $val;
        $app.mount($parent, __child);
        $crate::__rsx_children!($app, $parent, $($rest)*);
    };
    // nested element (styles + children)
    ($app:expr, $parent:expr, rsx!($capp:expr, $tag:expr, [ $($styles:tt)* ], { $($kids:tt)* }); $($rest:tt)*) => {
        let __child = $crate::rsx!($capp, $tag, [ $($styles)* ], { $($kids)* });
        $app.mount($parent, __child);
        $crate::__rsx_children!($app, $parent, $($rest)*);
    };
    // nested element (children only)
    ($app:expr, $parent:expr, rsx!($capp:expr, $tag:expr, { $($kids:tt)* }); $($rest:tt)*) => {
        let __child = $crate::rsx!($capp, $tag, { $($kids)* });
        $app.mount($parent, __child);
        $crate::__rsx_children!($app, $parent, $($rest)*);
    };
    // nested element (styles only)
    ($app:expr, $parent:expr, rsx!($capp:expr, $tag:expr, [ $($styles:tt)* ]); $($rest:tt)*) => {
        let __child = $crate::rsx!($capp, $tag, [ $($styles)* ]);
        $app.mount($parent, __child);
        $crate::__rsx_children!($app, $parent, $($rest)*);
    };
    // nested bare element
    ($app:expr, $parent:expr, rsx!($capp:expr, $tag:expr); $($rest:tt)*) => {
        let __child = $crate::rsx!($capp, $tag);
        $app.mount($parent, __child);
        $crate::__rsx_children!($app, $parent, $($rest)*);
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_protocol::{decode_all, NodeId, Op, PropId};

    /// The demo host root (matches `canopy-dom`'s `ROOT = NodeId::new(0)`).
    const ROOT: NodeId = NodeId::new(0);
    /// A demo background-color property id (the value is up to the host).
    const BG: PropId = PropId::new(1);

    fn count<F: Fn(&Op) -> bool>(ops: &[Op], pred: F) -> usize {
        ops.iter().filter(|o| pred(o)).count()
    }
    fn create_elements(ops: &[Op]) -> usize {
        count(ops, |o| matches!(o, Op::CreateElement { .. }))
    }
    fn create_texts(ops: &[Op]) -> usize {
        count(ops, |o| matches!(o, Op::CreateText { .. }))
    }
    fn inserts(ops: &[Op]) -> usize {
        count(ops, |o| matches!(o, Op::InsertBefore { .. }))
    }

    #[test]
    fn well_known_ids_are_stable() {
        assert_eq!(COLUMN.raw(), 1);
        assert_eq!(ROW.raw(), 2);
        assert_eq!(BUTTON.raw(), 3);
        assert_eq!(CLICK.raw(), 1);
    }

    #[test]
    fn button_builds_a_button_element_with_a_text_child() {
        let app = App::new();
        let btn = app.button("OK");
        let ops = decode_all(&app.take_batch(0)).unwrap();

        // One BUTTON element + one text child, with the text appended to the button.
        assert_eq!(create_elements(&ops), 1);
        assert_eq!(create_texts(&ops), 1);
        assert!(matches!(
            ops.iter().find(|o| matches!(o, Op::CreateElement { .. })),
            Some(Op::CreateElement { tag, .. }) if *tag == BUTTON
        ));
        // The lone InsertBefore appends the text child under the returned button.
        let insert = ops
            .iter()
            .find_map(|o| match o {
                Op::InsertBefore { parent, anchor, .. } => Some((*parent, *anchor)),
                _ => None,
            })
            .expect("text child is inserted");
        assert_eq!(insert.0, btn, "text child is appended to the button node");
        assert!(insert.1.is_null(), "append uses the NULL anchor");
    }

    #[test]
    fn builders_assemble_a_column_of_labels_and_a_button() {
        let app = App::new();

        // A column holding two labels and a button, built from the helpers.
        let col = app.el(COLUMN);
        let title = app.label("Canopy");
        let subtitle = app.label("native UI");
        let btn = app.button("Click me");
        app.mount(col, title);
        app.mount(col, subtitle);
        app.mount(col, btn);
        app.mount(ROOT, col);

        // Attach a click handler so the listener op is exercised too.
        let _h = app.on_click(btn, |_payload| {});

        let ops = decode_all(&app.take_batch(0)).unwrap();

        // Elements: column + button = 2. Texts: two labels + the button's label = 3.
        assert_eq!(create_elements(&ops), 2);
        assert_eq!(create_texts(&ops), 3);
        // Inserts: button's text child, 2 labels, button, column-under-root = 5.
        assert_eq!(inserts(&ops), 5);
        // The click listener was registered on the button for the CLICK event.
        assert!(ops.iter().any(|o| matches!(
            o,
            Op::AddListener { node, event, .. } if *node == btn && *event == CLICK
        )));
        // The column is the child appended under the host root.
        assert!(ops.iter().any(|o| matches!(
            o,
            Op::InsertBefore { parent, child, anchor }
                if *parent == ROOT && *child == col && anchor.is_null()
        )));
    }

    #[test]
    fn rsx_builds_a_nested_subtree_and_returns_the_root() {
        let app = App::new();

        // column > [ text("Canopy"), row > [ text("a"), button("OK") ] ]
        let root = rsx!(&app, COLUMN, [ (BG => "#101010") ], {
            text("Canopy");
            rsx!(&app, ROW, {
                text("a");
                node(app.button("OK"));
            });
        });
        app.mount(ROOT, root);

        let ops = decode_all(&app.take_batch(0)).unwrap();

        // Elements: column + row + button = 3.
        assert_eq!(create_elements(&ops), 3);
        // Texts: "Canopy", "a", and the button's "OK" label = 3.
        assert_eq!(create_texts(&ops), 3);
        // The column carries the inline style the macro applied.
        assert!(ops.iter().any(|o| matches!(
            o,
            Op::SetInlineStyle { node, prop, .. } if *node == root && *prop == BG
        )));
        // The first element created is the COLUMN, i.e. the returned root.
        assert!(matches!(
            ops.iter().find(|o| matches!(o, Op::CreateElement { .. })),
            Some(Op::CreateElement { node, tag }) if *node == root && *tag == COLUMN
        ));
        // The root is the child appended under the host root.
        assert!(ops.iter().any(|o| matches!(
            o,
            Op::InsertBefore { parent, child, anchor }
                if *parent == ROOT && *child == root && anchor.is_null()
        )));
    }

    #[test]
    fn rsx_bare_and_styled_elements_emit_expected_ops() {
        let app = App::new();
        let bare = rsx!(&app, ROW);
        let styled = rsx!(&app, COLUMN, [ (BG => "#fff") ]);
        let ops = decode_all(&app.take_batch(0)).unwrap();

        assert_eq!(create_elements(&ops), 2, "two elements, no children");
        assert_eq!(create_texts(&ops), 0);
        assert!(ops.iter().any(|o| matches!(
            o, Op::CreateElement { node, tag } if *node == bare && *tag == ROW
        )));
        assert!(ops.iter().any(|o| matches!(
            o, Op::SetInlineStyle { node, prop, .. } if *node == styled && *prop == BG
        )));
    }
}
