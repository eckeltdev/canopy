//! End-to-end proof of Canopy's signal-based reactivity, with no renderer:
//! an event handler writes a signal, which emits one targeted op, which a real
//! host `Dom` applies — and the host tree reflects the new value.

use canopy_dom::{Dom, ROOT};
use canopy_protocol::{ElementTag, EventKind, EventPayload};
use canopy_traits::OpSink;
use canopy_view::App;

const COLUMN: ElementTag = ElementTag::new(1);
const CLICK: EventKind = EventKind::new(1);

#[test]
fn click_drives_a_signal_that_updates_the_host_dom() {
    let app = App::new();

    // Build the view: a column containing a bound label.
    let col = app.element(COLUMN);
    app.append(ROOT, col);
    let label = app.text("");
    app.append(col, label);

    // State + binding: the label tracks `count`.
    let count = app.runtime().signal(0i32);
    {
        let count = count.clone();
        app.bind_text(label, move || format!("Count: {}", count.get()));
    }

    // A click on the column increments `count`.
    let on_click = {
        let count = count.clone();
        app.on(col, CLICK, move |_payload| count.update(|n| *n += 1))
    };

    // Mount: apply the initial batch to a fresh host tree.
    let mut dom = Dom::new();
    dom.apply(&app.take_batch(0)).unwrap();
    assert_eq!(dom.children(ROOT), &[col]);
    assert_eq!(dom.children(col), &[label]);
    assert_eq!(dom.text_of(label), Some("Count: 0"));

    // Host delivers a click -> handler writes the signal -> flush emits one op.
    app.dispatch(on_click, EventPayload::None);
    let batch = app.take_batch(1);
    dom.apply(&batch).unwrap();
    assert_eq!(dom.text_of(label), Some("Count: 1"));

    // The update is a single targeted SetText (+ its interned string), not a
    // re-mount: exactly one node still exists with no new elements created.
    let ops = canopy_protocol::decode_all(&batch).unwrap();
    let set_texts = ops
        .iter()
        .filter(|o| matches!(o, canopy_protocol::Op::SetText { .. }))
        .count();
    assert_eq!(set_texts, 1);
    assert!(!ops
        .iter()
        .any(|o| matches!(o, canopy_protocol::Op::CreateElement { .. })));

    // And again, to show it is steady-state, not a one-shot.
    app.dispatch(on_click, EventPayload::None);
    dom.apply(&app.take_batch(2)).unwrap();
    assert_eq!(dom.text_of(label), Some("Count: 2"));
    assert_eq!(dom.node_count(), 2); // column + label, nothing leaked
}
