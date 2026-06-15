//! Canopy guest-side core: the virtual-node tree, string interning, and the
//! reconciler that turns a re-render into a minimal, batched op-stream.
//!
//! This is the part of Canopy that *is* a guest program. It is `no_std` + `alloc`
//! and depends only on [`canopy_protocol`], so the exact same reconciler runs
//! compiled-in on a constrained target or inside a WASM sandbox — it only ever
//! produces op bytes, never touches a renderer.
//!
//! The [`Reconciler`] in this M0 scaffold mounts a tree and diffs structurally
//! identical trees (the counter case: only text changes emit ops). Keyed-list
//! reconciliation and attribute/style diffing reuse the same op vocabulary and
//! land next; the encoder and protocol already support them.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use canopy_protocol::{ElementTag, NodeId, Op, OpEncoder, StrId};

/// A lightweight virtual node the guest produces each render.
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

/// Allocates node handles and interns strings, then emits a batched op-stream for
/// each frame. Persistent across frames so handles and the string table are stable.
pub struct Reconciler {
    next_node: u64,
    next_str: u32,
    interned: BTreeMap<String, StrId>,
}

impl Reconciler {
    /// New reconciler. Node handles start at 1 so `0` can serve as a host root.
    pub fn new() -> Self {
        Self {
            next_node: 1,
            next_str: 0,
            interned: BTreeMap::new(),
        }
    }

    /// Mount `node` under `root_parent`, returning the realized tree and the op
    /// bytes that build it (wrapped in a `seq = 0` batch).
    pub fn mount(&mut self, root_parent: NodeId, node: &VNode) -> (Rendered, Vec<u8>) {
        let mut enc = OpEncoder::new();
        enc.begin_batch(0);
        let rendered = self.mount_node(node, root_parent, NodeId::NULL, &mut enc);
        enc.end_batch();
        (rendered, enc.into_bytes())
    }

    /// Diff `prev` against `next` (both under `root_parent`), returning the new
    /// realized tree and the op bytes for the change (wrapped in a `seq` batch).
    pub fn diff(
        &mut self,
        prev: Rendered,
        next: &VNode,
        root_parent: NodeId,
        seq: u32,
    ) -> (Rendered, Vec<u8>) {
        let mut enc = OpEncoder::new();
        enc.begin_batch(seq);
        let rendered = self.diff_node(prev, next, root_parent, &mut enc);
        enc.end_batch();
        (rendered, enc.into_bytes())
    }

    fn alloc_node(&mut self) -> NodeId {
        let id = NodeId::new(self.next_node);
        self.next_node += 1;
        id
    }

    fn intern(&mut self, s: &str, enc: &mut OpEncoder) -> StrId {
        if let Some(id) = self.interned.get(s) {
            return *id;
        }
        let id = StrId::new(self.next_str);
        self.next_str += 1;
        self.interned.insert(s.to_string(), id);
        enc.push(&Op::InternString {
            id,
            bytes: s.as_bytes().to_vec(),
        });
        id
    }

    fn mount_node(
        &mut self,
        node: &VNode,
        parent: NodeId,
        anchor: NodeId,
        enc: &mut OpEncoder,
    ) -> Rendered {
        match node {
            VNode::Element(ve) => {
                let id = self.alloc_node();
                enc.push(&Op::CreateElement {
                    node: id,
                    tag: ve.tag,
                });
                enc.push(&Op::InsertBefore {
                    parent,
                    child: id,
                    anchor,
                });
                let children = ve
                    .children
                    .iter()
                    .map(|c| self.mount_node(c, id, NodeId::NULL, enc))
                    .collect();
                Rendered::Element {
                    id,
                    tag: ve.tag,
                    children,
                }
            }
            VNode::Text(s) => {
                let id = self.alloc_node();
                let sid = self.intern(s, enc);
                enc.push(&Op::CreateText {
                    node: id,
                    text: sid,
                });
                enc.push(&Op::InsertBefore {
                    parent,
                    child: id,
                    anchor,
                });
                Rendered::Text {
                    id,
                    value: s.clone(),
                }
            }
        }
    }

    fn diff_node(
        &mut self,
        prev: Rendered,
        next: &VNode,
        parent: NodeId,
        enc: &mut OpEncoder,
    ) -> Rendered {
        match (prev, next) {
            // Same element kind and child count: recurse in place.
            (Rendered::Element { id, tag, children }, VNode::Element(ve))
                if tag == ve.tag && children.len() == ve.children.len() =>
            {
                let new_children = children
                    .into_iter()
                    .zip(ve.children.iter())
                    .map(|(child, next_child)| self.diff_node(child, next_child, id, enc))
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
                    let sid = self.intern(s, enc);
                    enc.push(&Op::SetText {
                        node: id,
                        text: sid,
                    });
                }
                Rendered::Text {
                    id,
                    value: s.clone(),
                }
            }
            // Anything else: replace the old subtree with a freshly mounted one.
            // (Keyed reconciliation for reorders/insertions lands next.)
            (prev, next) => {
                enc.push(&Op::RemoveNode { node: prev.id() });
                self.mount_node(next, parent, NodeId::NULL, enc)
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
}
