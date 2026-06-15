//! Integration tests for the `rsx!` proc-macro.
//!
//! The contract: an `rsx!` tree lowers to **exactly** the calls a hand-written `Ui`
//! tree makes, in the same order — there is no second code path. So the tests build a
//! tree with `rsx!`, mount it into a real host [`Dom`], and assert the tree's shape,
//! text, styling (resolved through the stylesheet), listeners, and reactive behaviour
//! are what a hand-written equivalent would produce.

use canopy_dom::{Dom, ROOT};
use canopy_paint::{BG, FG};
use canopy_traits::OpSink;
use canopy_ui::prelude::*;

const CSS: &str = "
    .root  { background: #1e1e2e; padding: 16px }
    .title { color: #cdd6f4 }
    .btn   { background: #313244; color: #cdd6f4 }
    .btn:hover { background: #585b70 }
    .pill  { background: #313244 }
    .link  { color: #89b4fa }
";

/// Build `ui` into a fresh `Dom` and return it.
fn mount(ui: &Ui) -> Dom {
    let mut dom = Dom::new();
    dom.apply(&ui.take_batch(0)).expect("mount batch applies");
    dom
}

#[test]
fn structure_text_and_classes() {
    let ui = Ui::with_css(CSS);
    let root = rsx!(ui =>
        Column class="root" {
            Label class="title" { "Canopy" }
            Row class="row" {
                Button class="pill" { "docs" }
                Button class="pill link" { "github" }
            }
        }
    );
    ui.mount_root(root);
    let dom = mount(&ui);

    // The root is a column under ROOT, with its class resolved to inline styles.
    assert_eq!(dom.children(ROOT).len(), 1);
    assert_eq!(dom.children(ROOT)[0], root);
    assert_eq!(dom.style(root, BG), Some("#1e1e2e"));

    // Title label text + color.
    let title = dom.children(root)[0];
    assert_eq!(dom.text_of(title), Some("Canopy"));
    assert_eq!(dom.style(title, FG), Some("#cdd6f4"));

    // The footer row holds two buttons; each button's text is its label child.
    let row = dom.children(root)[1];
    let buttons = dom.children(row);
    assert_eq!(buttons.len(), 2);
    let docs_label = dom.children(buttons[0])[0];
    assert_eq!(dom.text_of(docs_label), Some("docs"));
    // The second pill carries both classes: `.pill` background AND `.link` color.
    assert_eq!(dom.style(buttons[1], BG), Some("#313244"));
    assert_eq!(dom.style(buttons[1], FG), Some("#89b4fa"));
}

#[test]
fn on_click_and_reactive_bind_text() {
    let ui = Ui::with_css(CSS);
    let count = ui.signal(0i32);
    let root = rsx!(ui =>
        Column class="root" {
            Button class="btn"
                on_click({ let c = count.clone(); move |_| c.update(|n| *n += 1) })
                bind_text({ let c = count.clone(); move || {
                    let mut s = String::from("count is ");
                    s.push_str(&c.get().to_string());
                    s
                } })
        }
    );
    ui.mount_root(root);
    let mut dom = mount(&ui);

    // The button is the column's only child; its bound label child reads "count is 0".
    let button = dom.children(root)[0];
    let label = dom.children(button)[0];
    assert_eq!(dom.text_of(label), Some("count is 0"));

    // It carries a click listener; firing it (via the real dispatch path) increments
    // the signal and the bound label re-renders one targeted SetText.
    let handler = dom
        .node(button)
        .unwrap()
        .listeners
        .first()
        .map(|(_, h)| *h)
        .expect("button has a click listener");
    ui.dispatch(handler, EventPayload::None);
    dom.apply(&ui.take_batch(1)).expect("update batch applies");
    assert_eq!(dom.text_of(label), Some("count is 1"));
}

#[test]
fn component_splice_and_static_text_child() {
    // A component is just a function that builds a subtree and returns its root.
    fn badge(ui: &Ui) -> NodeId {
        rsx!(ui => Row class="row" { Label { "v0" } })
    }

    let ui = Ui::with_css(CSS);
    let root = rsx!(ui =>
        Column class="root" {
            { badge(&ui) }
            "plain leaf"
        }
    );
    ui.mount_root(root);
    let dom = mount(&ui);

    // The spliced component is mounted first, then the bare-string leaf.
    let kids = dom.children(root);
    assert_eq!(kids.len(), 2);
    // The component's nested label.
    let badge_label = dom.children(kids[0])[0];
    assert_eq!(dom.text_of(badge_label), Some("v0"));
    // The bare string became a text leaf.
    assert_eq!(dom.text_of(kids[1]), Some("plain leaf"));
}

#[test]
fn el_escape_hatch_builds_an_arbitrary_tag() {
    use canopy_protocol::ElementTag;
    const CUSTOM: ElementTag = ElementTag::new(99);

    let ui = Ui::new();
    let root = rsx!(ui => El(CUSTOM) { Label { "inside" } });
    ui.mount_root(root);
    let dom = mount(&ui);

    let inner = dom.children(root)[0];
    assert_eq!(dom.text_of(inner), Some("inside"));
}
