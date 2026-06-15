//! Build a small Canopy UI, bridge it into an AccessKit tree, and print the result.
//!
//! Run with: `cargo +nightly run -p canopy-a11y --example dump_tree`
//!
//! This is the end-to-end smoke test described in the crate's task: a column with a
//! text label and a button (BUTTON element + text child + click listener) is assembled
//! via [`canopy_core::Emitter`], decoded into a [`canopy_dom::Dom`], and turned into an
//! [`accesskit::TreeUpdate`]. We print every node's id, role, and name/value so a human
//! can see the screen-reader view of the UI.

use canopy_a11y::{accesskit_id, build_tree, ROOT_ID};
use canopy_core::Emitter;
use canopy_dom::{Dom, ROOT};
use canopy_protocol::HandlerId;
use canopy_traits::OpSink;
use canopy_view::{BUTTON, CLICK, COLUMN};

fn main() {
    // column > [ label("Welcome"), button("Click me") ]
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
    dom.apply(&e.take_batch(0)).expect("apply op batch");

    let update = build_tree(&dom);

    println!("Canopy DOM nodes (excluding ROOT): {}", dom.node_count());
    println!(
        "AccessKit nodes (incl. root window): {}",
        update.nodes.len()
    );
    println!(
        "tree root = {:?}, focus = {:?}\n",
        update.tree.as_ref().unwrap().root,
        update.focus
    );

    for (id, node) in &update.nodes {
        let tag = if *id == ROOT_ID {
            " (ROOT)".to_string()
        } else if *id == accesskit_id(col) {
            " (column)".to_string()
        } else if *id == accesskit_id(btn) {
            " (button)".to_string()
        } else if *id == accesskit_id(label) {
            " (label)".to_string()
        } else {
            String::new()
        };
        let name = node.label().or_else(|| node.value()).unwrap_or("");
        println!(
            "  {:?}{:<10} role={:?}  name/value={:?}  children={:?}",
            id,
            tag,
            node.role(),
            name,
            node.children(),
        );
    }
}
