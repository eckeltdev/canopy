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

/// A keyed list reconciler: it remembers, per render, the realized child for each
/// stable key and computes the *smallest* op-stream to turn the previous order into
/// the next one.
///
/// The op vocabulary has no dedicated "move" op, so a reorder is expressed as an
/// [`Op::InsertBefore`] of an already-realized child to a new position — the host
/// `Dom` treats `InsertBefore` of an existing node as detach-then-insert (i.e. a
/// move). New keys are mounted (`CreateElement`/`CreateText` + `InsertBefore`), and
/// dropped keys are [`Op::RemoveNode`]d.
///
/// To keep reorders minimal, the surviving items that *did not move* — a longest
/// stable subsequence of survivors, in their previous relative order — are left
/// untouched and emit no op; only the out-of-place items are re-inserted.
pub struct KeyedList<K> {
    /// The realized children in document order, paired with their keys.
    items: Vec<(K, Rendered)>,
}

impl<K: Ord + Clone> KeyedList<K> {
    /// A new, empty keyed list.
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// The current realized children, in document order.
    pub fn items(&self) -> &[(K, Rendered)] {
        &self.items
    }

    /// The keys currently realized, in document order.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.items.iter().map(|(k, _)| k)
    }

    /// Reconcile the realized list under `parent` toward `next`, a slice of
    /// `(key, VNode)` in the desired document order. Keys must be unique within
    /// `next`. Returns the realized child handles in the new order.
    ///
    /// - A key present in `next` but not before is **mounted** (one create-subtree
    ///   plus an `InsertBefore` at its target slot).
    /// - A key present before but not in `next` is **removed** (`RemoveNode`).
    /// - A surviving key is **kept in place** when it belongs to the longest stable
    ///   subsequence (emits no op); otherwise it is **moved** with a single
    ///   `InsertBefore` to its target slot.
    ///
    /// `emitter` receives every op; call [`Emitter::take_batch`] to drain them.
    pub fn reconcile(
        &mut self,
        emitter: &mut Emitter,
        parent: NodeId,
        next: &[(K, VNode)],
    ) -> &[(K, Rendered)] {
        // Number the survivors by their *previous document position* so the LIS pass
        // can find the run whose relative order is already correct. Capture this from
        // the in-order `items` before we tear them apart.
        let mut old_pos: BTreeMap<K, usize> = BTreeMap::new();
        for (i, (k, _)) in self.items.iter().enumerate() {
            old_pos.insert(k.clone(), i);
        }

        // Index the previous realized entries by key so survivors can be reused.
        // (Take ownership; anything left over after the walk is a removal.)
        let mut prev: BTreeMap<K, Rendered> = BTreeMap::new();
        for (k, r) in self.items.drain(..) {
            prev.insert(k, r);
        }

        // Remove keys that disappear from the new order. BTreeMap iterates in sorted
        // key order, so removals are emitted deterministically.
        let mut keep: BTreeMap<&K, ()> = BTreeMap::new();
        for (k, _) in next.iter() {
            keep.insert(k, ());
        }
        let mut to_remove: Vec<K> = Vec::new();
        for k in prev.keys() {
            if !keep.contains_key(k) {
                to_remove.push(k.clone());
            }
        }
        for k in &to_remove {
            if let Some(r) = prev.remove(k) {
                emitter.remove(r.id());
            }
        }

        // For each new slot, record the survivor's old position (or None if it is a
        // freshly mounted item). The LIS over these decides which survivors stay put.
        let mut survivor_prev_pos: Vec<Option<usize>> = Vec::with_capacity(next.len());
        for (k, _) in next.iter() {
            survivor_prev_pos.push(old_pos.get(k).copied());
        }
        let stable = longest_increasing_subsequence_mask(&survivor_prev_pos);

        // Build the realized list and emit moves/mounts. Walk right-to-left so the
        // anchor (the next already-placed sibling, or NULL = append) is known.
        let mut realized: Vec<Option<(K, Rendered)>> = (0..next.len()).map(|_| None).collect();
        let mut anchor = NodeId::NULL;
        for i in (0..next.len()).rev() {
            let (k, vnode) = &next[i];
            let rendered = match prev.remove(k) {
                Some(r) => {
                    // Survivor: move only if it is not part of the stable run.
                    if !stable[i] {
                        emitter.insert(parent, r.id(), anchor);
                    }
                    r
                }
                // New key: mount its subtree at this slot.
                None => mount_into(emitter, vnode, parent, anchor),
            };
            anchor = rendered.id();
            realized[i] = Some((k.clone(), rendered));
        }

        self.items = realized.into_iter().map(|e| e.unwrap()).collect();
        &self.items
    }
}

impl<K: Ord + Clone> Default for KeyedList<K> {
    fn default() -> Self {
        Self::new()
    }
}

/// Mount `node` under `parent` before `anchor`, emitting creates + an insert, and
/// returning the realized subtree. (Free function so [`KeyedList`] can mount without
/// owning a [`Reconciler`].)
fn mount_into(emitter: &mut Emitter, node: &VNode, parent: NodeId, anchor: NodeId) -> Rendered {
    match node {
        VNode::Element(ve) => {
            let id = emitter.create_element(ve.tag);
            emitter.insert(parent, id, anchor);
            let children = ve
                .children
                .iter()
                .map(|c| mount_into(emitter, c, id, NodeId::NULL))
                .collect();
            Rendered::Element {
                id,
                tag: ve.tag,
                children,
            }
        }
        VNode::Text(s) => {
            let id = emitter.create_text(s);
            emitter.insert(parent, id, anchor);
            Rendered::Text {
                id,
                value: s.clone(),
            }
        }
    }
}

/// Given one optional old-position per new slot (`None` = a freshly mounted item),
/// return a mask marking the slots that form a longest strictly increasing run of
/// *present* old-positions. Those slots are already in the correct relative order and
/// need no move op. `None` slots are never marked stable (a fresh node is always
/// inserted at its target position).
fn longest_increasing_subsequence_mask(seq: &[Option<usize>]) -> Vec<bool> {
    let n = seq.len();
    let mut stable = alloc::vec![false; n];

    // Patience-sorting LIS over the present values, remembering predecessors so the
    // participating indices can be reconstructed.
    // `tails[len-1]` holds the index (into `seq`) of the smallest tail of an
    // increasing run of length `len`; `prev_idx[i]` is `i`'s predecessor in its run.
    let mut tails: Vec<usize> = Vec::new();
    let mut prev_idx: Vec<Option<usize>> = alloc::vec![None; n];

    for (i, slot) in seq.iter().enumerate() {
        let v = match slot {
            Some(v) => *v,
            None => continue, // new items don't participate in the stable run
        };
        // First tail whose value is >= v (strictly increasing subsequence).
        let mut lo = 0usize;
        let mut hi = tails.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            let tail_v = seq[tails[mid]].unwrap();
            if tail_v < v {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo > 0 {
            prev_idx[i] = Some(tails[lo - 1]);
        }
        if lo == tails.len() {
            tails.push(i);
        } else {
            tails[lo] = i;
        }
    }

    // Reconstruct the LIS by walking predecessors back from the last tail.
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        stable[i] = true;
        cur = prev_idx[i];
    }

    stable
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

    // ---- Keyed-list reconciliation -------------------------------------------

    const LIST_PARENT: NodeId = NodeId::new(0);

    /// One item per key: a text node carrying the key's label.
    fn item(label: &str) -> VNode {
        text(label)
    }

    /// Build the `(key, VNode)` slice the reconcile API expects.
    fn rows(keys: &[&'static str]) -> Vec<(&'static str, VNode)> {
        keys.iter().map(|k| (*k, item(k))).collect()
    }

    fn count<F: Fn(&Op) -> bool>(ops: &[Op], pred: F) -> usize {
        ops.iter().filter(|o| pred(o)).count()
    }

    fn creates(ops: &[Op]) -> usize {
        count(ops, |o| {
            matches!(o, Op::CreateElement { .. } | Op::CreateText { .. })
        })
    }
    fn removes(ops: &[Op]) -> usize {
        count(ops, |o| matches!(o, Op::RemoveNode { .. }))
    }
    fn inserts(ops: &[Op]) -> usize {
        count(ops, |o| matches!(o, Op::InsertBefore { .. }))
    }

    #[test]
    fn keyed_initial_render_creates_and_inserts_each_item() {
        let mut e = Emitter::new();
        let mut list: KeyedList<&'static str> = KeyedList::new();

        list.reconcile(&mut e, LIST_PARENT, &rows(&["a", "b", "c"]));
        let ops = decode_all(&e.take_batch(0)).unwrap();

        // Three text items: three creates and three inserts, no removes.
        assert_eq!(creates(&ops), 3, "one create per new item");
        assert_eq!(inserts(&ops), 3, "one insert per new item");
        assert_eq!(removes(&ops), 0);

        let order: Vec<_> = list.keys().copied().collect();
        assert_eq!(order, ["a", "b", "c"]);
    }

    #[test]
    fn keyed_remove_middle_emits_one_remove_and_no_creates() {
        let mut e = Emitter::new();
        let mut list: KeyedList<&'static str> = KeyedList::new();
        list.reconcile(&mut e, LIST_PARENT, &rows(&["a", "b", "c"]));
        let _ = e.take_batch(0);

        list.reconcile(&mut e, LIST_PARENT, &rows(&["a", "c"]));
        let ops = decode_all(&e.take_batch(1)).unwrap();

        assert_eq!(removes(&ops), 1, "exactly the dropped key is removed");
        assert_eq!(creates(&ops), 0, "kept items are not recreated");
        assert_eq!(inserts(&ops), 0, "a and c keep their relative order");

        let order: Vec<_> = list.keys().copied().collect();
        assert_eq!(order, ["a", "c"]);
    }

    #[test]
    fn keyed_insert_at_front_creates_only_the_new_item() {
        let mut e = Emitter::new();
        let mut list: KeyedList<&'static str> = KeyedList::new();
        list.reconcile(&mut e, LIST_PARENT, &rows(&["a", "b", "c"]));
        let _ = e.take_batch(0);

        list.reconcile(&mut e, LIST_PARENT, &rows(&["z", "a", "b", "c"]));
        let ops = decode_all(&e.take_batch(1)).unwrap();

        assert_eq!(creates(&ops), 1, "only the new key 'z' is created");
        assert_eq!(removes(&ops), 0);
        // The new item is inserted before 'a'; the kept items a/b/c stay put and
        // emit no further insert.
        assert_eq!(inserts(&ops), 1, "only the new item is inserted");

        let order: Vec<_> = list.keys().copied().collect();
        assert_eq!(order, ["z", "a", "b", "c"]);
    }

    #[test]
    fn keyed_reorder_swap_emits_a_move_and_no_create_or_remove() {
        let mut e = Emitter::new();
        let mut list: KeyedList<&'static str> = KeyedList::new();
        list.reconcile(&mut e, LIST_PARENT, &rows(&["a", "b"]));
        let _ = e.take_batch(0);

        // [a,b] -> [b,a]: a single existing child is re-inserted (a move).
        list.reconcile(&mut e, LIST_PARENT, &rows(&["b", "a"]));
        let ops = decode_all(&e.take_batch(1)).unwrap();

        assert_eq!(creates(&ops), 0, "a reorder never creates");
        assert_eq!(removes(&ops), 0, "a reorder never removes");
        assert_eq!(inserts(&ops), 1, "exactly one element moves");

        let order: Vec<_> = list.keys().copied().collect();
        assert_eq!(order, ["b", "a"]);
    }

    #[test]
    fn keyed_kept_item_handle_is_stable_across_reconciles() {
        let mut e = Emitter::new();
        let mut list: KeyedList<&'static str> = KeyedList::new();
        list.reconcile(&mut e, LIST_PARENT, &rows(&["a", "b", "c"]));
        let _ = e.take_batch(0);
        let id_b = list
            .items()
            .iter()
            .find(|(k, _)| *k == "b")
            .map(|(_, r)| r.id())
            .unwrap();

        list.reconcile(&mut e, LIST_PARENT, &rows(&["c", "b", "a"]));
        let _ = e.take_batch(1);
        let id_b2 = list
            .items()
            .iter()
            .find(|(k, _)| *k == "b")
            .map(|(_, r)| r.id())
            .unwrap();

        assert_eq!(id_b, id_b2, "a surviving key reuses its host handle");
    }

    #[test]
    fn keyed_reorder_moves_minimal_count_via_lis() {
        let mut e = Emitter::new();
        let mut list: KeyedList<&'static str> = KeyedList::new();
        list.reconcile(&mut e, LIST_PARENT, &rows(&["a", "b", "c", "d"]));
        let _ = e.take_batch(0);

        // [a,b,c,d] -> [a,c,b,d]: the LIS keeps a/.../d (and one of b,c) stable, so
        // only a single item needs to move.
        list.reconcile(&mut e, LIST_PARENT, &rows(&["a", "c", "b", "d"]));
        let ops = decode_all(&e.take_batch(1)).unwrap();

        assert_eq!(creates(&ops), 0);
        assert_eq!(removes(&ops), 0);
        assert_eq!(inserts(&ops), 1, "LIS keeps all but one item in place");

        let order: Vec<_> = list.keys().copied().collect();
        assert_eq!(order, ["a", "c", "b", "d"]);
    }

    #[test]
    fn keyed_mounts_multi_node_subtrees() {
        let mut e = Emitter::new();
        let mut list: KeyedList<&'static str> = KeyedList::new();
        // Each item is an element with a text child: two creates per item.
        let next: Vec<(&'static str, VNode)> = ["a", "b"]
            .iter()
            .map(|k| (*k, element(ROW, vec![text(k)])))
            .collect();
        list.reconcile(&mut e, LIST_PARENT, &next);
        let ops = decode_all(&e.take_batch(0)).unwrap();

        assert_eq!(
            count(&ops, |o| matches!(o, Op::CreateElement { .. })),
            2,
            "one element per item"
        );
        assert_eq!(
            count(&ops, |o| matches!(o, Op::CreateText { .. })),
            2,
            "one text child per item"
        );
    }
}
