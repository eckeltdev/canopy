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
use canopy_protocol::{ElementTag, EventKind, EventPayload, HandlerId, NodeId};
use canopy_signals::Runtime;

/// A shared, mutable [`Emitter`] that reactive bindings write into.
pub type SharedEmitter = Rc<RefCell<Emitter>>;

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
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
