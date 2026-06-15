//! Canopy host-side retained tree and the consuming end of the op-stream.
//!
//! [`Dom`] is a node arena that implements [`OpSink`]: it decodes a batch of
//! `canopy-protocol` bytes (from either transport — the bytes are identical) and
//! applies the mutations. It is `no_std` + `alloc` because the tree itself needs no
//! OS; the `std` parts of a real host (window, GPU) live in separate backend crates
//! and feed off this tree.
//!
//! This is also where the **capability** guarantee is enforced at runtime: a
//! mutating op that names a node the guest never created is rejected with
//! [`HostError::BadHandle`] rather than silently aliasing something. The host mints
//! and validates handles; a guest can only touch what it was handed.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use canopy_protocol::{ElementTag, EventKind, HandlerId, NodeId, Op, OpReader, StrId};
use canopy_traits::{HostError, OpSink};

/// The implicit host root. Top-level nodes are inserted under this id; it is never
/// a real arena node and cannot be removed by a guest.
pub const ROOT: NodeId = NodeId::new(0);

/// One retained node.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    /// Element kind, or `None` for a text node.
    pub tag: Option<ElementTag>,
    /// Text content, for text nodes.
    pub text: Option<String>,
    /// Parent, or `None` if detached or top-level (parented to [`ROOT`]).
    pub parent: Option<NodeId>,
    /// Children, in order.
    pub children: Vec<NodeId>,
    /// `(event, handler)` subscriptions registered on this node.
    pub listeners: Vec<(EventKind, HandlerId)>,
}

/// A retained node tree built by applying op-stream batches.
#[derive(Default)]
pub struct Dom {
    nodes: BTreeMap<NodeId, Node>,
    strings: BTreeMap<StrId, String>,
    roots: Vec<NodeId>,
}

impl Dom {
    /// An empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live nodes (excluding the implicit [`ROOT`]).
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// The text content of a text node, if `node` exists and is text.
    pub fn text_of(&self, node: NodeId) -> Option<&str> {
        self.nodes.get(&node)?.text.as_deref()
    }

    /// The children of `node` (or of [`ROOT`] for the top level).
    pub fn children(&self, node: NodeId) -> &[NodeId] {
        if node == ROOT {
            &self.roots
        } else {
            self.nodes
                .get(&node)
                .map(|n| n.children.as_slice())
                .unwrap_or(&[])
        }
    }

    /// Look up an interned string.
    pub fn string(&self, id: StrId) -> Option<&str> {
        self.strings.get(&id).map(String::as_str)
    }

    /// Whether `node` exists in the arena.
    pub fn contains(&self, node: NodeId) -> bool {
        self.nodes.contains_key(&node)
    }

    fn require(&self, node: NodeId) -> Result<(), HostError> {
        if self.nodes.contains_key(&node) {
            Ok(())
        } else {
            Err(HostError::BadHandle)
        }
    }

    fn resolve_str(&self, id: StrId) -> Result<String, HostError> {
        self.strings.get(&id).cloned().ok_or(HostError::Decode)
    }

    fn detach(&mut self, node: NodeId) {
        let parent = self.nodes.get(&node).and_then(|n| n.parent);
        match parent {
            Some(p) => {
                if let Some(pn) = self.nodes.get_mut(&p) {
                    pn.children.retain(|c| *c != node);
                }
            }
            None => self.roots.retain(|c| *c != node),
        }
    }

    fn remove_subtree(&mut self, node: NodeId) {
        // Collect descendants first to avoid borrowing while mutating.
        let mut stack = alloc::vec![node];
        let mut to_remove = Vec::new();
        while let Some(n) = stack.pop() {
            to_remove.push(n);
            if let Some(found) = self.nodes.get(&n) {
                stack.extend(found.children.iter().copied());
            }
        }
        for n in to_remove {
            self.nodes.remove(&n);
        }
    }

    fn apply_op(&mut self, op: Op) -> Result<(), HostError> {
        match op {
            // Batch brackets carry no state in this M1 sink (version handling lands
            // with multi-version support).
            Op::BeginBatch { .. } | Op::EndBatch => {}

            Op::InternString { id, bytes } => {
                let s = String::from_utf8(bytes).map_err(|_| HostError::Decode)?;
                self.strings.insert(id, s);
            }

            Op::CreateElement { node, tag } => {
                self.nodes.insert(
                    node,
                    Node {
                        tag: Some(tag),
                        ..Node::default()
                    },
                );
            }
            Op::CreateText { node, text } => {
                let value = self.resolve_str(text)?;
                self.nodes.insert(
                    node,
                    Node {
                        text: Some(value),
                        ..Node::default()
                    },
                );
            }

            Op::InsertBefore {
                parent,
                child,
                anchor,
            } => {
                self.require(child)?;
                if parent != ROOT {
                    self.require(parent)?;
                }
                // Re-parenting: detach from any current position first.
                self.detach(child);
                if let Some(c) = self.nodes.get_mut(&child) {
                    c.parent = if parent == ROOT { None } else { Some(parent) };
                }
                let siblings = if parent == ROOT {
                    &mut self.roots
                } else {
                    &mut self
                        .nodes
                        .get_mut(&parent)
                        .ok_or(HostError::BadHandle)?
                        .children
                };
                let pos = if anchor.is_null() {
                    siblings.len()
                } else {
                    siblings
                        .iter()
                        .position(|s| *s == anchor)
                        .ok_or(HostError::BadHandle)?
                };
                siblings.insert(pos, child);
            }

            Op::RemoveNode { node } => {
                self.require(node)?;
                self.detach(node);
                self.remove_subtree(node);
            }

            Op::SetText { node, text } => {
                let value = self.resolve_str(text)?;
                self.nodes.get_mut(&node).ok_or(HostError::BadHandle)?.text = Some(value);
            }

            Op::AddListener {
                node,
                event,
                handler,
            } => {
                let n = self.nodes.get_mut(&node).ok_or(HostError::BadHandle)?;
                n.listeners.push((event, handler));
            }
            Op::RemoveListener { node, event } => {
                let n = self.nodes.get_mut(&node).ok_or(HostError::BadHandle)?;
                n.listeners.retain(|(e, _)| *e != event);
            }

            // Style/attribute application is a no-op until the style engine lands;
            // accept (don't reject) so a guest can author them now. They still
            // require a live target so a bad handle is caught.
            Op::SetAttribute { node, .. }
            | Op::SetInlineStyle { node, .. }
            | Op::SetClass { node, .. }
            | Op::RemoveClass { node, .. } => {
                self.require(node)?;
            }

            // host -> guest op; never valid in a guest -> host batch.
            Op::DispatchEvent { .. } => return Err(HostError::Decode),
        }
        Ok(())
    }
}

impl OpSink for Dom {
    fn apply(&mut self, ops: &[u8]) -> Result<(), HostError> {
        for op in OpReader::new(ops) {
            let op = op.map_err(|_| HostError::Decode)?;
            self.apply_op(op)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_core::Emitter;

    #[test]
    fn applies_a_mounted_tree() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let label = e.create_text("hello");
        e.append(col, label);

        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();

        assert_eq!(dom.node_count(), 2);
        assert_eq!(dom.children(ROOT), &[col]);
        assert_eq!(dom.children(col), &[label]);
        assert_eq!(dom.text_of(label), Some("hello"));
    }

    #[test]
    fn set_text_updates_in_place() {
        let mut e = Emitter::new();
        let t = e.create_text("a");
        e.append(ROOT, t);
        e.set_text(t, "b");
        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();
        assert_eq!(dom.text_of(t), Some("b"));
    }

    #[test]
    fn forged_handle_is_rejected() {
        // A batch that mutates a node the guest never created must be refused —
        // this is the capability boundary enforced at the host.
        let mut e = Emitter::new();
        let real = e.create_text("ok");
        e.append(ROOT, real);
        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();

        // Hand-roll a second batch that targets a fabricated handle.
        let mut forged = Emitter::new();
        // Burn handles so this one doesn't collide with `real`.
        for _ in 0..100 {
            forged.alloc_node();
        }
        let ghost = forged.alloc_node();
        forged.set_text(ghost, "haxx");
        assert_eq!(dom.apply(&forged.take_batch(1)), Err(HostError::BadHandle));
    }

    #[test]
    fn remove_drops_the_subtree() {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let a = e.create_text("a");
        e.append(col, a);
        let b = e.create_text("b");
        e.append(col, b);
        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();
        assert_eq!(dom.node_count(), 3);

        let mut rm = Emitter::new();
        rm.remove(col);
        // Re-apply against a sink that already has the nodes:
        dom.apply(&rm.take_batch(1)).unwrap();
        assert_eq!(dom.node_count(), 0);
        assert!(dom.children(ROOT).is_empty());
    }
}
