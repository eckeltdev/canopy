//! Canopy guest-side core: the op-stream [`Emitter`], a virtual-node tree, string
//! interning, and a [`Reconciler`] for static / keyed subtrees.
//!
//! This is the part of Canopy that *is* a guest program. It is `no_std` + `alloc`
//! and depends only on [`canopy_protocol`], so the exact same code runs compiled-in
//! on a constrained target or inside a WASM sandbox — it only ever produces op
//! bytes, never touches a renderer.
//!
//! With Canopy's **signal-based** reactivity, the steady-state update path is an
//! [`Emitter`] driven by fine-grained effects (see the `canopy-signals` and
//! `canopy-view` crates): a changed signal emits one targeted op (e.g. a single
//! `SetText`), not a whole-tree diff. The [`Reconciler`] here is the complementary
//! piece for *initial mount* and for *keyed lists*, where structure — not a single
//! value — changes.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use canopy_protocol::{ElementTag, EventKind, HandlerId, NodeId, Op, OpEncoder, PropId, StrId};

/// Builds a batched op-stream: allocates node handles, interns strings, and
/// accumulates ops until [`Emitter::take_batch`] wraps and drains them.
///
/// This is the single op-building primitive. The [`Reconciler`] uses it, and the
/// signal-reactive layer writes targeted ops into it from inside effects, so both
/// reactivity paths produce the *same* wire format.
pub struct Emitter {
    next_node: u64,
    next_str: u32,
    interned: BTreeMap<String, StrId>,
    pending: Vec<Op>,
}

impl Emitter {
    /// New emitter. Node handles start at 1 so `0` can serve as a host root.
    pub fn new() -> Self {
        Self {
            next_node: 1,
            next_str: 0,
            interned: BTreeMap::new(),
            pending: Vec::new(),
        }
    }

    /// Allocate a fresh node handle.
    pub fn alloc_node(&mut self) -> NodeId {
        let id = NodeId::new(self.next_node);
        self.next_node += 1;
        id
    }

    /// Intern a string, emitting `InternString` the first time it is seen.
    pub fn intern(&mut self, s: &str) -> StrId {
        if let Some(id) = self.interned.get(s) {
            return *id;
        }
        let id = StrId::new(self.next_str);
        self.next_str += 1;
        self.interned.insert(s.to_string(), id);
        self.pending.push(Op::InternString {
            id,
            bytes: s.as_bytes().to_vec(),
        });
        id
    }

    /// Create a detached element and return its handle.
    pub fn create_element(&mut self, tag: ElementTag) -> NodeId {
        let id = self.alloc_node();
        self.pending.push(Op::CreateElement { node: id, tag });
        id
    }

    /// Create a detached text node with initial content and return its handle.
    pub fn create_text(&mut self, initial: &str) -> NodeId {
        let id = self.alloc_node();
        let text = self.intern(initial);
        self.pending.push(Op::CreateText { node: id, text });
        id
    }

    /// Insert `child` under `parent` before `anchor` ([`NodeId::NULL`] = append).
    pub fn insert(&mut self, parent: NodeId, child: NodeId, anchor: NodeId) {
        self.pending.push(Op::InsertBefore {
            parent,
            child,
            anchor,
        });
    }

    /// Append `child` to the end of `parent`'s children.
    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        self.insert(parent, child, NodeId::NULL);
    }

    /// Replace a text node's content (the signal-reactive hot path).
    pub fn set_text(&mut self, node: NodeId, value: &str) {
        let text = self.intern(value);
        self.pending.push(Op::SetText { node, text });
    }

    /// Set one inline style property.
    pub fn set_inline_style(&mut self, node: NodeId, prop: PropId, value: &str) {
        let value = self.intern(value);
        self.pending.push(Op::SetInlineStyle { node, prop, value });
    }

    /// Add a class.
    pub fn set_class(&mut self, node: NodeId, class: &str) {
        let class = self.intern(class);
        self.pending.push(Op::SetClass { node, class });
    }

    /// Subscribe `node` to `event`, routing matches to `handler`.
    pub fn add_listener(&mut self, node: NodeId, event: EventKind, handler: HandlerId) {
        self.pending.push(Op::AddListener {
            node,
            event,
            handler,
        });
    }

    /// Remove a node (and, by host contract, its subtree).
    pub fn remove(&mut self, node: NodeId) {
        self.pending.push(Op::RemoveNode { node });
    }

    /// Whether any ops are pending.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Wrap all pending ops in a `seq` batch, returning the bytes and clearing the
    /// pending list. The string table and node counter persist across batches.
    pub fn take_batch(&mut self, seq: u32) -> Vec<u8> {
        let mut enc = OpEncoder::new();
        enc.begin_batch(seq);
        for op in self.pending.drain(..) {
            enc.push(&op);
        }
        enc.end_batch();
        enc.into_bytes()
    }
}

impl Default for Emitter {
    fn default() -> Self {
        Self::new()
    }
}

/// A lightweight virtual node the guest produces for a static subtree.
#[derive(Clone, Debug, PartialEq)]
pub enum VNode {
    /// An element with a kind and children.
    Element(VElement),
    /// A text leaf.
    Text(String),
}

/// An element vnode.
#[derive(Clone, Debug, PartialEq)]
pub struct VElement {
    /// Element kind.
    pub tag: ElementTag,
    /// Children.
    pub children: Vec<VNode>,
}

/// Build an element vnode.
pub fn element(tag: ElementTag, children: Vec<VNode>) -> VNode {
    VNode::Element(VElement { tag, children })
}

/// Build a text vnode.
pub fn text(value: &str) -> VNode {
    VNode::Text(value.to_string())
}

/// The realized counterpart of a [`VNode`]: it remembers the host node handles so
/// the next frame can be diffed against it.
#[derive(Clone, Debug, PartialEq)]
pub enum Rendered {
    /// A realized element.
    Element {
        /// Host node handle.
        id: NodeId,
        /// Element kind.
        tag: ElementTag,
        /// Realized children.
        children: Vec<Rendered>,
    },
    /// A realized text node.
    Text {
        /// Host node handle.
        id: NodeId,
        /// Current text.
        value: String,
    },
}

impl Rendered {
    /// The host handle of this node.
    pub fn id(&self) -> NodeId {
        match self {
            Rendered::Element { id, .. } | Rendered::Text { id, .. } => *id,
        }
    }
}

/// Mounts a static vnode tree and diffs structurally-identical trees, emitting a
/// batched op-stream via an internal [`Emitter`]. The signal layer is preferred for
/// single-value updates; this is for initial structure and (next) keyed lists.
pub struct Reconciler {
    emitter: Emitter,
}

impl Reconciler {
    /// New reconciler.
    pub fn new() -> Self {
        Self {
            emitter: Emitter::new(),
        }
    }

    /// Mount `node` under `root_parent`, returning the realized tree and the op
    /// bytes that build it (a `seq = 0` batch).
    pub fn mount(&mut self, root_parent: NodeId, node: &VNode) -> (Rendered, Vec<u8>) {
        let rendered = self.mount_node(node, root_parent, NodeId::NULL);
        (rendered, self.emitter.take_batch(0))
    }

    /// Diff `prev` against `next` (both under `root_parent`), returning the new
    /// realized tree and the op bytes for the change (a `seq` batch).
    pub fn diff(
        &mut self,
        prev: Rendered,
        next: &VNode,
        root_parent: NodeId,
        seq: u32,
    ) -> (Rendered, Vec<u8>) {
        let rendered = self.diff_node(prev, next, root_parent);
        (rendered, self.emitter.take_batch(seq))
    }

    fn mount_node(&mut self, node: &VNode, parent: NodeId, anchor: NodeId) -> Rendered {
        match node {
            VNode::Element(ve) => {
                let id = self.emitter.create_element(ve.tag);
                self.emitter.insert(parent, id, anchor);
                let children = ve
                    .children
                    .iter()
                    .map(|c| self.mount_node(c, id, NodeId::NULL))
                    .collect();
                Rendered::Element {
                    id,
                    tag: ve.tag,
                    children,
                }
            }
            VNode::Text(s) => {
                let id = self.emitter.create_text(s);
                self.emitter.insert(parent, id, anchor);
                Rendered::Text {
                    id,
                    value: s.clone(),
                }
            }
        }
    }

    fn diff_node(&mut self, prev: Rendered, next: &VNode, parent: NodeId) -> Rendered {
        match (prev, next) {
            // Same element kind and child count: recurse in place.
            (Rendered::Element { id, tag, children }, VNode::Element(ve))
                if tag == ve.tag && children.len() == ve.children.len() =>
            {
                let new_children = children
                    .into_iter()
                    .zip(ve.children.iter())
                    .map(|(child, next_child)| self.diff_node(child, next_child, id))
                    .collect();
                Rendered::Element {
                    id,
                    tag,
                    children: new_children,
                }
            }
            // Text in place: emit a SetText only when the content actually changed.
            (Rendered::Text { id, value }, VNode::Text(s)) => {
                if value != *s {
                    self.emitter.set_text(id, s);
                }
                Rendered::Text {
                    id,
                    value: s.clone(),
                }
            }
            // Anything else: replace the old subtree with a freshly mounted one.
            // (Keyed reconciliation for reorders/insertions lands next.)
            (prev, next) => {
                self.emitter.remove(prev.id());
                self.mount_node(next, parent, NodeId::NULL)
            }
        }
    }
}

impl Default for Reconciler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;
    use canopy_protocol::decode_all;

    // Stand-in element kinds; the real ids come from the host widget registry.
    const COLUMN: ElementTag = ElementTag::new(1);
    const ROW: ElementTag = ElementTag::new(2);
    const BUTTON: ElementTag = ElementTag::new(3);

    fn counter(n: i32) -> VNode {
        element(
            COLUMN,
            vec![
                text(&format!("Count: {n}")),
                element(
                    ROW,
                    vec![
                        element(BUTTON, vec![text("+")]),
                        element(BUTTON, vec![text("-")]),
                    ],
                ),
            ],
        )
    }

    const HOST_ROOT: NodeId = NodeId::new(0);

    #[test]
    fn mount_emits_a_create_tree() {
        let mut r = Reconciler::new();
        let (_rendered, bytes) = r.mount(HOST_ROOT, &counter(0));
        let ops = decode_all(&bytes).unwrap();

        let creates = ops
            .iter()
            .filter(|o| matches!(o, Op::CreateElement { .. }))
            .count();
        // column + row + two buttons = 4 elements.
        assert_eq!(creates, 4);
        assert!(ops.iter().any(|o| matches!(o, Op::CreateText { .. })));
        assert!(ops.iter().any(|o| matches!(o, Op::InternString { .. })));
    }

    #[test]
    fn diff_text_only_emits_one_set_text_and_no_creates() {
        let mut r = Reconciler::new();
        let (rendered, _) = r.mount(HOST_ROOT, &counter(0));

        let (_rendered2, bytes) = r.diff(rendered, &counter(1), HOST_ROOT, 1);
        let ops = decode_all(&bytes).unwrap();

        let set_texts = ops
            .iter()
            .filter(|o| matches!(o, Op::SetText { .. }))
            .count();
        assert_eq!(set_texts, 1, "only the changed label should update");
        assert!(
            !ops.iter().any(|o| matches!(o, Op::CreateElement { .. })),
            "a stable structure must not recreate nodes"
        );
    }

    #[test]
    fn identical_frame_emits_no_mutations() {
        let mut r = Reconciler::new();
        let (rendered, _) = r.mount(HOST_ROOT, &counter(0));
        let (_r2, bytes) = r.diff(rendered, &counter(0), HOST_ROOT, 1);
        let ops = decode_all(&bytes).unwrap();
        // Only the BeginBatch/EndBatch bracket, nothing in between.
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], Op::BeginBatch { .. }));
        assert!(matches!(ops[1], Op::EndBatch));
    }

    #[test]
    fn emitter_interns_each_string_once() {
        let mut e = Emitter::new();
        let n = e.create_text("hi");
        e.set_text(n, "hi"); // same string -> no second InternString
        e.set_text(n, "bye"); // new string -> one InternString
        let ops = decode_all(&e.take_batch(0)).unwrap();
        let interns = ops
            .iter()
            .filter(|o| matches!(o, Op::InternString { .. }))
            .count();
        assert_eq!(interns, 2);
    }
}
