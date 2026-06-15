//! Canopy's fine-grained reactive runtime: signals, effects, and a batched flush.
//!
//! This is the engine that makes a counter emit *one* `SetText` per click instead
//! of re-diffing a tree. Per the design, it lives in the shared core (not in each
//! language wrapper) so every language's authoring layer is thin syntax over the
//! same dependency-tracking runtime — otherwise only Rust would get good DX.
//!
//! M0 is single-threaded by design (`Rc`/`RefCell`), which matches the WASM guest
//! and the constrained-target event loop. A `Send`/`Sync` variant for the native
//! compiled-in transport (where the host may drive the guest from a worker thread)
//! is a deliberate follow-up — the seam is isolated here, not spread through the
//! core.
//!
//! Note: effects form reference cycles with the runtime (an effect captures
//! signals which hold the runtime which holds the effect). For M0 that means the
//! runtime is leaked for the program's lifetime, which is fine for an app root and
//! is documented here rather than hidden.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

type EffectId = u64;
type EffectFn = Rc<RefCell<Box<dyn FnMut()>>>;

struct RuntimeInner {
    running: Cell<Option<EffectId>>,
    next_effect: Cell<EffectId>,
    effects: RefCell<BTreeMap<EffectId, EffectFn>>,
    dirty: RefCell<Vec<EffectId>>,
}

/// A reactive scope. Cheap to clone (it is `Rc`-backed); all clones share state.
#[derive(Clone)]
pub struct Runtime(Rc<RuntimeInner>);

impl Runtime {
    /// Create a new reactive runtime.
    pub fn new() -> Self {
        Runtime(Rc::new(RuntimeInner {
            running: Cell::new(None),
            next_effect: Cell::new(0),
            effects: RefCell::new(BTreeMap::new()),
            dirty: RefCell::new(Vec::new()),
        }))
    }

    /// Create a signal holding `value`.
    pub fn signal<T: Clone + 'static>(&self, value: T) -> Signal<T> {
        Signal(Rc::new(RefCell::new(SignalInner {
            value,
            subscribers: Vec::new(),
            rt: self.clone(),
        })))
    }

    /// Register an effect and run it once, capturing the signals it reads as
    /// dependencies. It re-runs on [`Runtime::flush`] after any dependency changes.
    pub fn create_effect(&self, f: impl FnMut() + 'static) {
        let id = self.0.next_effect.get();
        self.0.next_effect.set(id + 1);
        let cell: EffectFn = Rc::new(RefCell::new(Box::new(f)));
        self.0.effects.borrow_mut().insert(id, cell.clone());
        self.run_effect(id, &cell);
    }

    /// Re-run every effect marked dirty since the last flush, until the queue
    /// drains (effects may dirty further effects).
    pub fn flush(&self) {
        loop {
            let batch = {
                let mut dirty = self.0.dirty.borrow_mut();
                if dirty.is_empty() {
                    break;
                }
                core::mem::take(&mut *dirty)
            };
            for id in batch {
                let cell = self.0.effects.borrow().get(&id).cloned();
                if let Some(cell) = cell {
                    self.run_effect(id, &cell);
                }
            }
        }
    }

    fn run_effect(&self, id: EffectId, cell: &EffectFn) {
        let previous = self.0.running.replace(Some(id));
        {
            let mut f = cell.borrow_mut();
            (f)();
        }
        self.0.running.set(previous);
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

struct SignalInner<T> {
    value: T,
    subscribers: Vec<EffectId>,
    rt: Runtime,
}

/// A reactive value. Reading it inside an effect subscribes that effect; writing
/// it marks subscribers dirty (applied on the next [`Runtime::flush`]).
pub struct Signal<T>(Rc<RefCell<SignalInner<T>>>);

impl<T> Clone for Signal<T> {
    fn clone(&self) -> Self {
        Signal(self.0.clone())
    }
}

impl<T: Clone + 'static> Signal<T> {
    /// Read the value, subscribing the currently-running effect (if any).
    pub fn get(&self) -> T {
        let mut inner = self.0.borrow_mut();
        if let Some(eid) = inner.rt.0.running.get() {
            if !inner.subscribers.contains(&eid) {
                inner.subscribers.push(eid);
            }
        }
        inner.value.clone()
    }

    /// Replace the value and mark subscribers dirty.
    pub fn set(&self, value: T) {
        let (subscribers, rt) = {
            let mut inner = self.0.borrow_mut();
            inner.value = value;
            (inner.subscribers.clone(), inner.rt.clone())
        };
        let mut dirty = rt.0.dirty.borrow_mut();
        for eid in subscribers {
            if !dirty.contains(&eid) {
                dirty.push(eid);
            }
        }
    }

    /// Mutate the value in place, then mark subscribers dirty.
    pub fn update(&self, f: impl FnOnce(&mut T)) {
        let mut value = self.get();
        f(&mut value);
        self.set(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_runs_once_then_reruns_on_flush() {
        let rt = Runtime::new();
        let count = rt.signal(0i32);

        let observed = Rc::new(Cell::new(-1i32));
        {
            let count = count.clone();
            let observed = observed.clone();
            rt.create_effect(move || observed.set(count.get()));
        }

        assert_eq!(observed.get(), 0, "effect runs once on registration");

        count.set(5);
        assert_eq!(observed.get(), 0, "set alone does not re-run; flush does");

        rt.flush();
        assert_eq!(observed.get(), 5, "flush re-runs the dependent effect");
    }

    #[test]
    fn untracked_signal_does_not_trigger_unrelated_effects() {
        let rt = Runtime::new();
        let a = rt.signal(0i32);
        let b = rt.signal(100i32);

        let runs = Rc::new(Cell::new(0u32));
        {
            let a = a.clone();
            let runs = runs.clone();
            rt.create_effect(move || {
                let _ = a.get();
                runs.set(runs.get() + 1);
            });
        }
        assert_eq!(runs.get(), 1);

        // Changing `b` (not read by the effect) must not schedule it.
        b.set(101);
        rt.flush();
        assert_eq!(runs.get(), 1);

        // Changing `a` must.
        a.set(1);
        rt.flush();
        assert_eq!(runs.get(), 2);
    }

    #[test]
    fn update_reads_then_writes() {
        let rt = Runtime::new();
        let n = rt.signal(41i32);
        n.update(|v| *v += 1);
        assert_eq!(n.get(), 42);
    }
}
