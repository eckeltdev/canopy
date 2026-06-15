//! Canopy accessibility bridge: turn a retained [`canopy_dom::Dom`] into an
//! [AccessKit](https://accesskit.dev) accessibility tree.
//!
//! Most Rust GUI stacks ship without a real accessibility story; Canopy treats it
//! as a first-class concern. This crate walks the host-side node arena that
//! [`canopy_dom`] builds from the op-stream and produces an [`accesskit::TreeUpdate`]
//! — the same value an AccessKit platform adapter consumes to expose the UI to a
//! screen reader on macOS (VoiceOver), Windows (Narrator/UIA), and Linux (Orca/AT-SPI).
//!
//! It is a `std` leaf crate: it reads from the `no_std` [`canopy_dom`] tree but lives
//! on the host side next to the renderer and window, so it can depend on AccessKit's
//! owned `String`/`Vec` node model without compromising the guest core.
//!
//! # Mapping
//!
//! The Canopy tree is intentionally minimal (elements + text nodes + listeners), so
//! the bridge infers semantic roles from structure rather than from a rich tag set:
//!
//! | Canopy node                                            | AccessKit [`Role`]      | name source            |
//! |--------------------------------------------------------|-------------------------|------------------------|
//! | text node                                              | [`Role::Label`]         | its string (as `value`)|
//! | element with a [`canopy_view::CLICK`] listener         | [`Role::Button`]        | descendant text        |
//! | [`canopy_view::INPUT`] element                         | [`Role::TextInput`]     | child text (as `value`)|
//! | [`canopy_view::COLUMN`] / [`canopy_view::ROW`] / other  | [`Role::GenericContainer`] | —                  |
//! | the implicit host [`canopy_dom::ROOT`]                  | [`Role::Window`]        | —                      |
//!
//! Parent/child relationships are preserved exactly: every element's AccessKit node
//! lists its Canopy children as AccessKit children, and the [`canopy_dom::ROOT`] becomes
//! the tree root. AccessKit [`NodeId`]s are derived from Canopy [`canopy_protocol::NodeId`]
//! raw values, with the root mapped to [`ROOT_ID`] (Canopy reserves raw `0` for `ROOT`
//! and mints real handles from `1`, so the spaces never collide).

use accesskit::{Node, NodeId, Role, Tree, TreeUpdate};
use canopy_dom::{Dom, ROOT};
use canopy_protocol::EventKind;
use canopy_view::{BUTTON, CLICK, COLUMN, INPUT, ROW};

/// The AccessKit [`NodeId`] of the synthesized tree root, corresponding to the implicit
/// host [`canopy_dom::ROOT`]. Canopy reserves raw node `0` for `ROOT` and allocates real
/// handles starting at `1`, so this id never aliases a real node.
pub const ROOT_ID: NodeId = NodeId(0);

/// Name of the UI toolkit reported to assistive technologies.
const TOOLKIT_NAME: &str = "Canopy";

/// Map a Canopy [`canopy_protocol::NodeId`] to its AccessKit [`NodeId`].
///
/// The mapping is the identity on the raw integer, so the trees stay in lock-step and a
/// later incremental update can address the same nodes. The host root maps to [`ROOT_ID`].
#[inline]
pub fn accesskit_id(node: canopy_protocol::NodeId) -> NodeId {
    NodeId(node.raw())
}

/// Build a complete AccessKit [`TreeUpdate`] for `dom`.
///
/// The returned update carries every reachable node (the synthesized root plus the whole
/// subtree under [`canopy_dom::ROOT`]), a [`Tree`] rooted at [`ROOT_ID`], and focus set to
/// the root (no element-level focus is inferred here). It is suitable as the initial,
/// full-tree update an AccessKit platform adapter expects on activation.
pub fn build_tree(dom: &Dom) -> TreeUpdate {
    let mut nodes: Vec<(NodeId, Node)> = Vec::new();

    // The synthesized root: a Window whose children are the top-level Canopy nodes.
    let mut root = Node::new(Role::Window);
    root.set_children(
        dom.children(ROOT)
            .iter()
            .map(|c| accesskit_id(*c))
            .collect::<Vec<_>>(),
    );
    nodes.push((ROOT_ID, root));

    // Walk every top-level subtree depth-first, emitting one AccessKit node per Canopy node.
    for child in dom.children(ROOT) {
        push_subtree(dom, *child, &mut nodes);
    }

    let mut tree = Tree::new(ROOT_ID);
    tree.toolkit_name = Some(TOOLKIT_NAME.into());
    tree.app_name = Some(TOOLKIT_NAME.into());

    TreeUpdate {
        nodes,
        tree: Some(tree),
        focus: ROOT_ID,
    }
}

/// Emit the AccessKit node for `node`, then recurse into its children, appending each to
/// `out` in depth-first order.
fn push_subtree(dom: &Dom, node: canopy_protocol::NodeId, out: &mut Vec<(NodeId, Node)>) {
    if let Some(ak) = node_for(dom, node) {
        out.push((accesskit_id(node), ak));
    }
    for child in dom.children(node) {
        push_subtree(dom, *child, out);
    }
}

/// Build the AccessKit [`Node`] for a single Canopy node, or [`None`] if `node` is not in
/// `dom`.
///
/// This is the per-node mapping in isolation (no recursion): roles are inferred from the
/// node's structure and the Canopy children are recorded as AccessKit children so a caller
/// assembling a partial update gets correct parenting. See the crate docs for the full table.
pub fn node_for(dom: &Dom, node: canopy_protocol::NodeId) -> Option<Node> {
    let n = dom.node(node)?;

    // A text node (no element tag) becomes a Label carrying its string as the value;
    // AccessKit models the text content of a Label via `value`, not `label`.
    if n.tag.is_none() {
        let mut ak = Node::new(Role::Label);
        if let Some(text) = n.text.as_deref() {
            ak.set_value(text);
        }
        return Some(ak);
    }

    let has_click = n.listeners.iter().any(|(event, _)| *event == CLICK);
    let is_button = n.tag == Some(BUTTON) || has_click;
    let is_input = n.tag == Some(INPUT);

    let role = if is_input {
        Role::TextInput
    } else if is_button {
        Role::Button
    } else if n.tag == Some(COLUMN) || n.tag == Some(ROW) {
        Role::GenericContainer
    } else {
        // An unrecognized element tag is still a grouping container in the a11y tree.
        Role::GenericContainer
    };

    let mut ak = Node::new(role);

    // Record children for parenting regardless of role.
    ak.set_children(
        n.children
            .iter()
            .map(|c| accesskit_id(*c))
            .collect::<Vec<_>>(),
    );

    match role {
        // A button is named by the text it contains (its accessible label).
        Role::Button => {
            if let Some(name) = descendant_text(dom, node) {
                ak.set_label(name);
            }
        }
        // A text input's value is the text it currently holds.
        Role::TextInput => {
            if let Some(value) = descendant_text(dom, node) {
                ak.set_value(value);
            }
        }
        _ => {}
    }

    Some(ak)
}

/// Concatenate the text of every text-node descendant of `node`, in document order.
///
/// Used to name a button or value a text input from the text nodes nested inside it.
/// Returns [`None`] if there is no descendant text at all.
fn descendant_text(dom: &Dom, node: canopy_protocol::NodeId) -> Option<String> {
    let mut acc = String::new();
    collect_text(dom, node, &mut acc);
    if acc.is_empty() {
        None
    } else {
        Some(acc)
    }
}

/// Depth-first accumulate descendant text into `acc` (excluding `node` itself if it is an
/// element; text nodes contribute their string).
fn collect_text(dom: &Dom, node: canopy_protocol::NodeId, acc: &mut String) {
    if let Some(n) = dom.node(node) {
        if let Some(text) = n.text.as_deref() {
            acc.push_str(text);
        }
        for child in &n.children {
            collect_text(dom, *child, acc);
        }
    }
}

/// Convenience predicate: does `node` carry a click listener (i.e. is it interactive)?
///
/// Exposed for hosts that want to mirror Canopy's notion of "is a button" when wiring
/// AccessKit [`accesskit::Action`] handlers back to event dispatch.
pub fn has_click_listener(dom: &Dom, node: canopy_protocol::NodeId) -> bool {
    dom.node(node)
        .map(|n| {
            n.listeners
                .iter()
                .any(|(e, _): &(EventKind, _)| *e == CLICK)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_core::Emitter;
    use canopy_dom::ROOT;
    use canopy_protocol::HandlerId;
    use canopy_traits::OpSink;

    /// Build a Dom: a column containing a text label and a button (BUTTON element with a
    /// text child and a click listener).
    fn build_demo_dom() -> (
        Dom,
        canopy_protocol::NodeId,
        canopy_protocol::NodeId,
        canopy_protocol::NodeId,
    ) {
        let mut e = Emitter::new();

        let col = e.create_element(COLUMN);
        e.append(ROOT, col);

        let label = e.create_text("Welcome");
        e.append(col, label);

        let btn = e.create_element(BUTTON);
        let btn_text = e.create_text("Click me");
        e.append(btn, btn_text);
        e.append(col, btn);
        e.add_listener(btn, CLICK, HandlerId::new(0));

        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();

        (dom, col, label, btn)
    }

    #[test]
    fn root_is_a_window_parenting_the_top_level_column() {
        let (dom, col, _, _) = build_demo_dom();
        let update = build_tree(&dom);

        let (_, root) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == ROOT_ID)
            .expect("root node present");
        assert_eq!(root.role(), Role::Window);
        assert_eq!(root.children(), &[accesskit_id(col)]);
        assert_eq!(update.tree.as_ref().unwrap().root, ROOT_ID);
        assert_eq!(update.focus, ROOT_ID);
    }

    #[test]
    fn button_node_is_named_by_its_text() {
        let (dom, _, _, btn) = build_demo_dom();
        let update = build_tree(&dom);

        let (_, button) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == accesskit_id(btn))
            .expect("button node present");
        assert_eq!(button.role(), Role::Button);
        assert_eq!(button.label(), Some("Click me"));
    }

    #[test]
    fn text_label_becomes_a_label_with_its_string_as_value() {
        let (dom, col, label, _) = build_demo_dom();
        let update = build_tree(&dom);

        let (_, label_node) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == accesskit_id(label))
            .expect("label node present");
        assert_eq!(label_node.role(), Role::Label);
        assert_eq!(label_node.value(), Some("Welcome"));

        // Parenting is preserved: the column lists both the label and the button.
        let (_, column) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == accesskit_id(col))
            .expect("column node present");
        assert_eq!(column.role(), Role::GenericContainer);
        assert!(column.children().contains(&accesskit_id(label)));
    }

    #[test]
    fn input_element_becomes_a_text_input_with_its_text_as_value() {
        let mut e = Emitter::new();
        let input = e.create_element(INPUT);
        let txt = e.create_text("hello");
        e.append(input, txt);
        e.append(ROOT, input);
        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();

        let update = build_tree(&dom);
        let (_, input_node) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == accesskit_id(input))
            .expect("input node present");
        assert_eq!(input_node.role(), Role::TextInput);
        assert_eq!(input_node.value(), Some("hello"));
    }

    #[test]
    fn node_count_covers_root_plus_every_dom_node() {
        let (dom, _, _, _) = build_demo_dom();
        let update = build_tree(&dom);
        // root window + column + label + button + button's text child = 5.
        assert_eq!(update.nodes.len(), dom.node_count() + 1);
        assert_eq!(update.nodes.len(), 5);
    }
}
