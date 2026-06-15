//! Integration tests for the `rsx!` proc-macro.
//!
//! The contract: a JSX `rsx!` tree lowers to **exactly** the calls a hand-written `Ui`
//! tree makes, in the same order — there is no second code path. So the tests build a
//! tree with `rsx!`, mount it into a real host [`Dom`], and assert its shape, text,
//! styling (resolved through the stylesheet), listeners, and reactive behaviour.

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
        <div class="root">
            <span class="title">"Canopy"</span>
            <div class="row">
                <button class="pill">"docs"</button>
                <button class="pill link">"github"</button>
            </div>
        </div>
    );
    ui.mount_root(root);
    let dom = mount(&ui);

    // The root is a container under ROOT, with its class resolved to inline styles.
    assert_eq!(dom.children(ROOT).len(), 1);
    assert_eq!(dom.children(ROOT)[0], root);
    assert_eq!(dom.style(root, BG), Some("#1e1e2e"));

    // Title text + color.
    let title = dom.children(root)[0];
    assert_eq!(dom.text_of(title), Some("Canopy"));
    assert_eq!(dom.style(title, FG), Some("#cdd6f4"));

    // The footer row holds two buttons; each button's text is its label child.
    let row = dom.children(root)[1];
    let buttons = dom.children(row);
    assert_eq!(buttons.len(), 2);
    assert_eq!(dom.text_of(dom.children(buttons[0])[0]), Some("docs"));
    // The second pill carries both classes: `.pill` background AND `.link` color.
    assert_eq!(dom.style(buttons[1], BG), Some("#313244"));
    assert_eq!(dom.style(buttons[1], FG), Some("#89b4fa"));
}

#[test]
fn on_click_and_reactive_text_child() {
    let ui = Ui::with_css(CSS);
    let count = ui.signal(0i32);
    let root = rsx!(ui =>
        <div class="root">
            <button class="btn"
                on:click={ let c = count.clone(); move |_| c.update(|n| *n += 1) }>
                { let c = count.clone(); move || {
                    let mut s = String::from("count is ");
                    s.push_str(&c.get().to_string());
                    s
                } }
            </button>
        </div>
    );
    ui.mount_root(root);
    let mut dom = mount(&ui);

    // The button is the container's only child; its bound label reads "count is 0".
    let button = dom.children(root)[0];
    let label = dom.children(button)[0];
    assert_eq!(dom.text_of(label), Some("count is 0"));

    // Firing the click (via the real dispatch path) increments the signal and the bound
    // label re-renders one targeted SetText.
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
fn component_splice_and_static_text_leaf() {
    // A component is just a function that builds a subtree and returns its root.
    fn badge(ui: &Ui) -> NodeId {
        rsx!(ui => <div class="row"><span>"v0"</span></div>)
    }

    let ui = Ui::with_css(CSS);
    let root = rsx!(ui =>
        <div class="root">
            { badge(&ui) }
            "plain leaf"
        </div>
    );
    ui.mount_root(root);
    let dom = mount(&ui);

    // The spliced component is mounted first, then the bare-string leaf.
    let kids = dom.children(root);
    assert_eq!(kids.len(), 2);
    assert_eq!(dom.text_of(dom.children(kids[0])[0]), Some("v0"));
    assert_eq!(dom.text_of(kids[1]), Some("plain leaf"));
}

#[test]
fn self_closing_input_and_el_escape_hatch() {
    use canopy_protocol::ElementTag;
    const CUSTOM: ElementTag = ElementTag::new(99);

    let ui = Ui::new();
    let root = rsx!(ui =>
        <el tag={CUSTOM}>
            <input value="seed"/>
            <span>"inside"</span>
        </el>
    );
    ui.mount_root(root);
    let dom = mount(&ui);

    let kids = dom.children(root);
    // The input's text child shows its seeded value; the span shows its text.
    assert_eq!(dom.text_of(dom.children(kids[0])[0]), Some("seed"));
    assert_eq!(dom.text_of(kids[1]), Some("inside"));
}
