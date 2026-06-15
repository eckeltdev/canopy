//! Canopy **welcome** app — UI logic only (no windowing).
//!
//! This is the flagship "create-canopy-app" landing: Canopy's answer to the Vite/React
//! starter screen — a logo built from rounded-rect "leaves", a heading and tagline, a
//! card with a reactive **counter button**, and a row of footer pills.
//!
//! It is authored with [`canopy_ui`]: the whole screen is one [`rsx!`] expression, and
//! everything a host needs — styling, the hover registry, hot-reload — is bundled in
//! the returned [`Ui`]. Compare the body of [`build`] to the tree it produces: that is
//! the entire point of the DX layer. Styling is a real **CSS stylesheet**
//! (`styles.css`, with `:hover` + `border-radius`), layout is real **Taffy**, and text
//! is real antialiased glyphs — `canopy-ui` just removes the plumbing.
//!
//! Nothing here references `winit`; windowing lives in `src/bin/window.rs`, and the
//! headless renderer (`src/bin/render.rs`) drives the same [`build`] to write a PPM.

use canopy_ui::prelude::*;

/// Logical viewport width — wide enough to frame the centered content column (the
/// `.canvas` class is `760px`; its symmetric padding centers the `560px` content).
pub const VIEW_W: f32 = 760.0;
/// Logical viewport height — sized so the canvas padding balances the logo, heading,
/// card and footer top-to-bottom.
pub const VIEW_H: f32 = 592.0;

/// The stylesheet shipped next to the source as a **real file** (`styles.css`), located
/// via `CARGO_MANIFEST_DIR` so both binaries find it regardless of the working
/// directory. The app reads it at runtime, so editing + saving it hot-reloads the live
/// window.
pub const STYLES_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/styles.css");

/// Read `styles.css` from disk (empty string if it can't be read, so the app keeps
/// running with whatever styles are already on the tree).
#[must_use]
pub fn load_styles() -> String {
    std::fs::read_to_string(STYLES_PATH).unwrap_or_default()
}

/// The built welcome screen: the [`Ui`] context (app + stylesheet + styled/hover
/// registries + hit-testing) plus the counter signal the headless shot starts nonzero.
pub struct Welcome {
    /// The authoring + host context. Drives batches, hover, clicks, and hot-reload.
    pub ui: Ui,
    /// The counter value, bound to the button label.
    pub count: Signal<i32>,
}

/// Assemble the welcome screen. The entire tree is one `rsx!` expression; the logo is a
/// reusable component spliced in with `{ logo(&ui) }`.
#[must_use]
pub fn build() -> Welcome {
    let ui = Ui::with_css(&load_styles());
    let count = ui.signal(0i32);

    let root = rsx!(ui =>
        <div class="canvas">
            <div class="content">
                { logo(&ui) }
                <span class="title">"Canopy"</span>
                <span class="tagline">"web-like native UI — no JavaScript runtime"</span>
                <div class="card">
                    <button class="btn"
                        on:click={ let c = count.clone(); move |_| c.update(|n| *n += 1) }>
                        { let c = count.clone(); move || format!("count is {}", c.get()) }
                    </button>
                    <span class="hint">"Edit styles.css and save to hot-reload"</span>
                </div>
                <div class="footer">
                    <button class="pill">"docs"</button>
                    <button class="pill pill-link">"github"</button>
                </div>
            </div>
        </div>
    );
    ui.mount_root(root);

    Welcome { ui, count }
}

/// The Canopy logo: two rows of rounded "leaves" (the canopy) over a short trunk, each
/// piece a pure rounded-rect element background (no text, no image files). A component
/// is just a function that builds a subtree on the shared [`Ui`] and returns its root.
/// (A `<div>` is a flex container whose row/column direction is set by its CSS class —
/// `.leafrow`/`.trunkrow` carry `direction: row` — exactly like real flexbox.)
fn logo(ui: &Ui) -> NodeId {
    rsx!(ui =>
        <div class="logo">
            <div class="leafrow">
                <div class="leaf leaf-green"/>
                <div class="leaf leaf-teal"/>
                <div class="leaf leaf-green"/>
            </div>
            <div class="leafrow">
                <div class="leaf leaf-teal"/>
                <div class="leaf leaf-blue"/>
            </div>
            <div class="trunkrow">
                <div class="trunk"/>
            </div>
        </div>
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_paint::{BG, RADIUS};
    use canopy_traits::OpSink;
    use canopy_ui::prelude::Dom;

    /// Mount a freshly built [`Welcome`] into a [`Dom`].
    fn mount() -> (Welcome, Dom) {
        let w = build();
        let mut dom = Dom::new();
        dom.apply(&w.ui.take_batch(0)).expect("mount batch applies");
        (w, dom)
    }

    /// The single node carrying a click listener — the counter button.
    fn find_button(dom: &Dom) -> NodeId {
        (0..)
            .map(NodeId::new)
            .take_while(|id| id.raw() < 200)
            .find(|&id| dom.node(id).is_some_and(|n| !n.listeners.is_empty()))
            .expect("a node with a listener (the counter button)")
    }

    #[test]
    fn welcome_tree_builds_with_expected_nodes() {
        let (w, dom) = mount();

        // The counter button's bound label reads "count is 0" at build time.
        let button = find_button(&dom);
        let label = dom.children(button)[0];
        assert_eq!(dom.text_of(label), Some("count is 0"));

        // Copy across the screen is present.
        let texts: Vec<String> = (0..)
            .map(NodeId::new)
            .take_while(|id| id.raw() < 200)
            .filter_map(|id| dom.text_of(id).map(str::to_string))
            .collect();
        for needle in [
            "web-like native UI — no JavaScript runtime",
            "Edit styles.css and save to hot-reload",
            "docs",
            "github",
        ] {
            assert!(texts.iter().any(|t| t == needle), "{needle:?} is present");
        }

        // The button + two footer pills are the three hoverables (derived from the
        // stylesheet's `:hover` rules, not hand-listed).
        assert_eq!(w.ui.hoverables().len(), 3, "button + docs + github");

        // The card carries a rounded background, proving `border-radius` reached a node.
        let card =
            w.ui.styled()
                .into_iter()
                .find(|(id, classes)| classes.contains(&"card") && dom.contains(*id))
                .map(|(id, _)| id)
                .expect("card node is styled");
        assert_eq!(dom.style(card, BG), Some("#313244"));
        assert_eq!(dom.style(card, RADIUS), Some("12"));
    }

    #[test]
    fn counter_label_tracks_the_signal() {
        let (w, mut dom) = mount();
        let button = find_button(&dom);
        let label = dom.children(button)[0];

        w.count.set(3);
        w.ui.runtime().flush();
        dom.apply(&w.ui.take_batch(1))
            .expect("counter batch applies");
        assert_eq!(dom.text_of(label), Some("count is 3"));
    }

    #[test]
    fn hot_reload_restyles_the_button() {
        let (w, mut dom) = mount();
        let button = find_button(&dom);
        assert_eq!(dom.style(button, BG), Some("#45475a"));

        // A new stylesheet a designer might save: a different `.btn` color + radius.
        const EDITED: &str = "
            .btn  { background: #f9e2af; color: #1e1e2e; padding: 14px; border-radius: 16px }
        ";
        let n = w.ui.reload_css(EDITED, None);
        assert!(n > 0, "every styled node re-applied");
        dom.apply(&w.ui.take_batch(1))
            .expect("reload batch applies");

        // The button's background and radius now reflect the edited stylesheet, on the
        // same node handle — no remount.
        assert_eq!(dom.style(button, BG), Some("#f9e2af"));
        assert_eq!(dom.style(button, RADIUS), Some("16"));
    }

    #[test]
    fn hover_lightens_the_button() {
        let (w, mut dom) = mount();
        let button = find_button(&dom);
        assert_eq!(dom.style(button, BG), Some("#45475a"));

        w.ui.set_hover(button, true);
        dom.apply(&w.ui.take_batch(1)).expect("hover batch applies");
        assert_eq!(dom.style(button, BG), Some("#585b70"), "hover lightens");
    }
}
