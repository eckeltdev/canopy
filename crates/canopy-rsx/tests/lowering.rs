//! Integration tests for the `rsx!` proc-macro.
//!
//! The contract these tests pin down is the whole point of the macro: an `rsx!` tree
//! must lower to **exactly** the op-stream a hand-written tree built with the same
//! `App` builders, in the same order, produces — there is no second code path. So most
//! tests build the same tree twice (once with `rsx!`, once by hand on a fresh `App`)
//! and assert the two op batches are **byte-for-byte equal**.
//!
//! This equality works because `App` mints node and interned-string ids monotonically
//! from zero: two `App`s driven with the identical sequence of builder calls hand out
//! the identical handles, so the encoded bytes coincide. The macro's job is to emit
//! that identical sequence; these tests fail the instant it reorders or drops a call.
//!
//! A final test additionally *applies* an `rsx!` batch to a real host `Dom` and drives
//! a reactive update end-to-end, to prove the lowered tree is not just byte-equal in a
//! vacuum but actually behaves.

use canopy_dom::{Dom, ROOT};
use canopy_protocol::{decode_all, EventPayload, NodeId, Op, PropId};
use canopy_rsx::rsx;
use canopy_traits::OpSink;
use canopy_view::{App, BUTTON, COLUMN, INPUT, ROW};

/// The reserved class-list property id the macro writes `class = ".."` under. Mirrors
/// the documented `canopy_rsx::CLASS_PROP_ID` (which a `proc-macro` crate cannot
/// export), so a host or test decodes the slot by matching `PropId::new(0)`.
const CLASS_PROP: PropId = PropId::new(0);
/// An arbitrary host property id for the explicit-`style(..)` test; any id the host
/// understands works, which is the point of leaving `style(PROP, ..)` open.
const BG: PropId = PropId::new(7);

/// Build the op batch a freshly-built tree emits, given a closure that authors it.
fn batch_of(build: impl FnOnce(&App) -> NodeId) -> Vec<u8> {
    let app = App::new();
    let root = build(&app);
    // Mount the root under the host root so the top-level insert is part of the batch
    // too — that way the comparison covers the whole emitted stream, not just the
    // subtree interior.
    app.mount(ROOT, root);
    app.take_batch(0)
}

#[test]
fn nesting_matches_hand_written_tree_byte_for_byte() {
    // column > [ label("a"), row > [ label("b"), button("c") ] ]
    let from_macro = batch_of(|app| {
        rsx!(app => Column {
            Label("a");
            Row {
                Label("b");
                Button("c");
            }
        })
    });

    // The same tree, authored by hand in the *exact* order the macro lowers to:
    // depth-first, and each child is created and then immediately mounted before the
    // next sibling is built. (Create col; create+mount label a; create row, then
    // create+mount its children, then mount row under col.)
    let by_hand = batch_of(|app| {
        let col = app.el(COLUMN);
        let a = app.label("a");
        app.mount(col, a);
        let row = app.el(ROW);
        let b = app.label("b");
        app.mount(row, b);
        let c = app.button("c");
        app.mount(row, c);
        app.mount(col, row);
        col
    });

    assert_eq!(
        from_macro, by_hand,
        "rsx! nesting must emit the identical op-stream to the hand-built tree"
    );

    // And sanity-check the shape via the decoded ops: 2 elements (column, row) + the
    // button element = 3 elements, and 3 text leaves ("a", "b", and the button label).
    let ops = decode_all(&from_macro).unwrap();
    assert_eq!(
        ops.iter()
            .filter(|o| matches!(o, Op::CreateElement { .. }))
            .count(),
        3
    );
    assert_eq!(
        ops.iter()
            .filter(|o| matches!(o, Op::CreateText { .. }))
            .count(),
        3
    );
}

#[test]
fn class_attribute_lowers_to_a_class_inline_style() {
    let from_macro = batch_of(|app| {
        rsx!(app => Column(class = "root") {
            Label("hi");
        })
    });

    let by_hand = batch_of(|app| {
        let col = app.el(COLUMN);
        app.style(col, CLASS_PROP, "root");
        let hi = app.label("hi");
        app.mount(col, hi);
        col
    });

    assert_eq!(
        from_macro, by_hand,
        "`class = \"root\"` must lower to one style(node, CLASS_PROP, \"root\") write"
    );

    // The class shows up as an inline-style op carrying the class name under the
    // reserved prop, on the root element.
    let ops = decode_all(&from_macro).unwrap();
    let root = match ops
        .iter()
        .find(|o| matches!(o, Op::CreateElement { .. }))
        .unwrap()
    {
        Op::CreateElement { node, .. } => *node,
        _ => unreachable!(),
    };
    assert!(ops.iter().any(|o| matches!(
        o,
        Op::SetInlineStyle { node, prop, .. } if *node == root && *prop == CLASS_PROP
    )));
}

#[test]
fn explicit_style_attribute_lowers_to_app_style() {
    let from_macro = batch_of(|app| rsx!(app => Column(style(BG, "#101010")) {}));

    let by_hand = batch_of(|app| {
        let col = app.el(COLUMN);
        app.style(col, BG, "#101010");
        col
    });

    assert_eq!(from_macro, by_hand);
}

#[test]
fn multiple_args_and_a_button_modifier_match_by_hand() {
    // A row with a class, holding a single button that carries an on_click handler.
    let from_macro = batch_of(|app| {
        rsx!(app => Row(class = "bar") {
            Button("ok") on_click(|_payload| {});
        })
    });

    let by_hand = batch_of(|app| {
        let row = app.el(ROW);
        app.style(row, CLASS_PROP, "bar");
        let btn = app.button("ok");
        app.on_click(btn, |_payload| {});
        app.mount(row, btn);
        row
    });

    assert_eq!(
        from_macro, by_hand,
        "an element with a class attribute and a child carrying on_click must match"
    );

    // The listener is registered on the button element for the CLICK event.
    let ops = decode_all(&from_macro).unwrap();
    assert!(ops.iter().any(|o| matches!(o, Op::AddListener { .. })));
    let buttons = ops
        .iter()
        .filter(|o| matches!(o, Op::CreateElement { tag, .. } if *tag == BUTTON))
        .count();
    assert_eq!(buttons, 1);
}

#[test]
fn bind_text_label_matches_hand_written_binding() {
    // A bound label inside a column; the binding's first run emits the initial SetText.
    let from_macro = batch_of(|app| {
        let count = app.runtime().signal(0i32);
        rsx!(app => Column {
            Label() bind_text(move || format!("Count: {}", count.get()));
        })
    });

    let by_hand = batch_of(|app| {
        let count = app.runtime().signal(0i32);
        let col = app.el(COLUMN);
        let label = app.label("");
        app.bind_text(label, move || format!("Count: {}", count.get()));
        app.mount(col, label);
        col
    });

    assert_eq!(
        from_macro, by_hand,
        "`Label() bind_text(..)` must lower to label(\"\") + bind_text(..)"
    );

    // The binding ran once at build time, emitting exactly one SetText for the label.
    let ops = decode_all(&from_macro).unwrap();
    assert_eq!(
        ops.iter()
            .filter(|o| matches!(o, Op::SetText { .. }))
            .count(),
        1,
        "the bound label emits its initial text on creation"
    );
}

#[test]
fn input_head_lowers_to_text_input() {
    let from_macro = batch_of(|app| rsx!(app => Input("seed")));

    let by_hand = batch_of(|app| app.text_input("seed"));

    assert_eq!(
        from_macro, by_hand,
        "`Input(\"seed\")` must lower to text_input(\"seed\")"
    );

    // It is an INPUT element (with its mirrored text child + initial SetText).
    let ops = decode_all(&from_macro).unwrap();
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::CreateElement { tag, .. } if *tag == INPUT)));
}

#[test]
fn rsx_tree_applies_to_a_host_dom_and_a_click_updates_it() {
    // The end-to-end proof: an rsx!-built tree, applied to a real host Dom, with a
    // click handler that writes a signal a bound label tracks.
    let app = App::new();
    let count = app.runtime().signal(0i32);

    let root = {
        let count_for_click = count.clone();
        let count_for_label = count.clone();
        rsx!(app => Column(class = "root") {
            Button("+") on_click(move |_| count_for_click.update(|n| *n += 1));
            Label() bind_text(move || format!("Count: {}", count_for_label.get()));
        })
    };
    app.mount(ROOT, root);

    // Mount the initial batch into a fresh host tree.
    let mut dom = Dom::new();
    dom.apply(&app.take_batch(0)).unwrap();

    // The column is the single top-level node, and it carries the class slot.
    assert_eq!(dom.children(ROOT), &[root]);
    assert_eq!(dom.style(root, CLASS_PROP), Some("root"));

    // Find the bound label (the column's second child) and confirm its initial text.
    let label = dom.children(root)[1];
    assert_eq!(dom.text_of(label), Some("Count: 0"));

    // Drive a click: the column's first child is the button; grab its listener id.
    let button = dom.children(root)[0];
    let handler = dom
        .node(button)
        .and_then(|n| n.listeners.first().copied())
        .map(|(_, h)| h)
        .expect("the button registered a click listener");

    // Dispatch the click, apply the resulting (single-op) batch, and observe the label.
    app.dispatch(handler, EventPayload::None);
    let batch = app.take_batch(1);
    dom.apply(&batch).unwrap();
    assert_eq!(dom.text_of(label), Some("Count: 1"));

    // The update was one targeted SetText, not a re-mount: no element was recreated.
    let ops = decode_all(&batch).unwrap();
    assert_eq!(
        ops.iter()
            .filter(|o| matches!(o, Op::SetText { .. }))
            .count(),
        1
    );
    assert!(!ops.iter().any(|o| matches!(o, Op::CreateElement { .. })));
}
