//! Canopy **welcome** app — UI logic only (no windowing).
//!
//! This is the flagship "create-canopy-app" landing: Canopy's answer to the
//! Vite/React starter screen. [`build`] assembles a centered, rounded welcome
//! screen on the Canopy crates — a logo built from rounded-rect "leaves", a heading
//! and tagline, a card with a reactive **counter button**, and a row of footer
//! pills — all authored as a real **CSS stylesheet** ([`canopy_style_css`]) with
//! class rules (including `:hover` and `border-radius`), laid out by the real
//! **Taffy** flexbox engine ([`canopy_layout_taffy`]) and rasterized with real
//! antialiased glyphs ([`canopy_render_text`]).
//!
//! Nothing here references `winit`; windowing lives entirely in the windowed binary
//! (`src/bin/window.rs`). The headless renderer (`src/bin/render.rs`) drives the
//! exact same [`build`] to write a PPM.
//!
//! # Why [`Welcome`] returns what it does
//!
//! The host binaries need more than the [`App`] to drive the screen interactively,
//! so [`build`] hands those pieces back:
//!
//! - `count` — the counter [`Signal`], so the headless shot can start it nonzero and
//!   the window can read it.
//! - `css` + `hoverables` — the parsed [`Stylesheet`] and the list of hoverable
//!   `(NodeId, classes)`, so the window can re-resolve a node's style (with `:hover`)
//!   when the cursor crosses it (see [`hover_target`] and the windowed binary).
//! - `styled` — **every** styled node paired with its classes. This is the
//!   registry the **hot-reload** loop replays: when `styles.css` changes on disk, the
//!   window re-parses it and re-applies *all* of these nodes' classes through
//!   [`Stylesheet::apply_state`], so the edited stylesheet restyles the live tree.

use canopy_dom::{Dom, ROOT};
use canopy_layout_taffy::{hit_test, layout};
use canopy_protocol::{HandlerId, NodeId};
use canopy_signals::Signal;
use canopy_style_css::Stylesheet;
use canopy_traits::{Point, Size};
use canopy_view::{App, BUTTON, CLICK, COLUMN, ROW};

/// Logical viewport width — wide enough to frame the centered content column with
/// margin on either side (the canvas class is `760px` wide; the content is `560px`,
/// so the symmetric canvas padding centers it horizontally).
pub const VIEW_W: f32 = 760.0;
/// Logical viewport height — sized so the canvas's symmetric padding leaves the logo,
/// heading, card and footer vertically balanced (the stack runs ~88px → ~510px, so a
/// ~590px viewport gives a comparable top and bottom margin).
pub const VIEW_H: f32 = 592.0;

/// The stylesheet shipped next to the source as a **real file** (`styles.css`). The
/// app reads it at runtime so editing + saving it hot-reloads the live window.
///
/// Locating it via `CARGO_MANIFEST_DIR` (baked in at compile time) means both the
/// headless `render` binary and the windowed binary find the same on-disk file
/// regardless of the working directory `cargo run` was invoked from.
pub const STYLES_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/styles.css");

/// Read and parse the on-disk [`STYLES_PATH`] stylesheet.
///
/// This is the single place the app turns the real `styles.css` file into a parsed
/// [`Stylesheet`]; the hot-reload loop calls it again on each save. Falls back to an
/// empty stylesheet if the file can't be read (e.g. deleted mid-session) so the app
/// keeps running with the styles already on the live tree rather than crashing.
#[must_use]
pub fn load_stylesheet() -> Stylesheet {
    match std::fs::read_to_string(STYLES_PATH) {
        Ok(src) => canopy_style_css::parse(&src),
        Err(_) => Stylesheet::new(),
    }
}

// --- Class lists, kept `'static` so the host can retain them in `hoverables`/`styled`
//     and replay them through `apply_state` on hover/reload without re-allocating. ---

/// The counter button's classes.
const BTN_CLASSES: &[&str] = &["btn"];
/// The "docs" footer pill's classes.
const PILL_CLASSES: &[&str] = &["pill"];
/// The "github" footer pill's classes (adds the blue link color on top of `.pill`).
const PILL_LINK_CLASSES: &[&str] = &["pill", "pill-link"];

/// The built welcome screen: the reactive [`App`] plus everything a host needs.
pub struct Welcome {
    /// The reactive app (produces op batches, receives dispatched events).
    pub app: App,
    /// The counter value, bound to the button label. Exposed so the headless shot can
    /// start it nonzero and the window can read it.
    pub count: Signal<i32>,
    /// The counter button node, so the headless shot can confirm it exists and a host
    /// could focus it.
    pub button: NodeId,
    /// The parsed stylesheet, retained so the windowed host can re-resolve a node's
    /// classes (with `:hover`) on cursor enter/leave, and so a fresh parse on reload
    /// can be swapped in.
    pub css: Stylesheet,
    /// Every node that reacts to `:hover`, paired with the `'static` classes to
    /// re-resolve it with. The window hit-tests the cursor, finds the matching entry,
    /// and flips that node's hover state (see [`hover_target`]). Holds the counter
    /// button and the two footer pills.
    pub hoverables: Vec<(NodeId, &'static [&'static str])>,
    /// **Every** styled node paired with its classes — the hot-reload registry. On a
    /// `styles.css` save the window re-parses the sheet and replays each of these
    /// through [`Stylesheet::apply_state`], so the edited file restyles the live tree.
    pub styled: Vec<(NodeId, &'static [&'static str])>,
}

/// A small builder that records every `(node, classes)` it styles, so the same list
/// drives the initial paint *and* the hot-reload replay from one source of truth.
///
/// Without this it would be far too easy for a node to be styled at build time but
/// forgotten on reload (it would simply stop updating when `styles.css` changes).
/// Routing every `style` call through here guarantees the reload registry is exactly
/// the set of styled nodes — no node can be styled without also being reloadable.
///
/// It deliberately does **not** borrow the [`App`]/[`Stylesheet`] (it takes them per
/// call) so it can accumulate the registry and then be consumed for [`Welcome::styled`]
/// while `app`/`css` move into the returned [`Welcome`].
struct Styler {
    /// Accumulates `(node, classes)` in the order styled; becomes [`Welcome::styled`].
    styled: Vec<(NodeId, &'static [&'static str])>,
}

impl Styler {
    fn new() -> Self {
        Self { styled: Vec::new() }
    }

    /// Apply `classes` to `node` now (base, un-hovered) and record it for reload.
    fn style(
        &mut self,
        app: &App,
        css: &Stylesheet,
        node: NodeId,
        classes: &'static [&'static str],
    ) {
        css.apply(app, node, classes);
        self.styled.push((node, classes));
    }
}

/// Assemble the welcome screen and return the live [`Welcome`].
///
/// The tree is deterministic: it always creates the same nodes in the same order, so
/// a host can rebuild an identical batch and have handles line up — which is exactly
/// what makes the hot-reload re-apply land on the right nodes.
#[must_use]
pub fn build() -> Welcome {
    let app = App::new();
    let rt = app.runtime();
    let css = load_stylesheet();
    let mut styler = Styler::new();
    let mut hoverables: Vec<(NodeId, &'static [&'static str])> = Vec::new();

    // The dark canvas fills the viewport; the content column sits inside it.
    let canvas = app.el(COLUMN);
    styler.style(&app, &css, canvas, &["canvas"]);
    app.mount(ROOT, canvas);

    let content = app.el(COLUMN);
    styler.style(&app, &css, content, &["content"]);
    app.mount(canvas, content);

    // --- The Canopy logo: rounded-rect leaves over a short trunk. ---
    let logo = build_logo(&app, &css, &mut styler);
    app.mount(content, logo);

    // --- Heading + tagline. ---
    let title = app.label("Canopy");
    styler.style(&app, &css, title, &["title"]);
    app.mount(content, title);

    let tagline = app.label("web-like native UI — no JavaScript runtime");
    styler.style(&app, &css, tagline, &["tagline"]);
    app.mount(content, tagline);

    // --- The counter card. ---
    let card = app.el(COLUMN);
    styler.style(&app, &css, card, &["card"]);
    app.mount(content, card);

    // The counter button: a signal-bound label that increments on click. We build the
    // button by hand (element + text child) rather than via [`App::button`] so we
    // capture the label's [`NodeId`] and can bind it to the counter — the binding emits
    // exactly one `SetText` per click, the fine-grained reactive hot path.
    let count = rt.signal(0i32);
    let button = app.el(BUTTON);
    styler.style(&app, &css, button, BTN_CLASSES);
    hoverables.push((button, BTN_CLASSES));
    {
        let count = count.clone();
        app.on_click(button, move |_| count.update(|n| *n += 1));
    }
    let button_label = app.label("");
    app.mount(button, button_label);
    {
        let count = count.clone();
        app.bind_text(button_label, move || format!("count is {}", count.get()));
    }
    app.mount(card, button);

    let hint = app.label("Edit styles.css and save to hot-reload");
    styler.style(&app, &css, hint, &["hint"]);
    app.mount(card, hint);

    // --- Footer pills/links. ---
    let footer = app.el(ROW);
    styler.style(&app, &css, footer, &["footer"]);
    app.mount(content, footer);

    let docs = app.button("docs");
    styler.style(&app, &css, docs, PILL_CLASSES);
    hoverables.push((docs, PILL_CLASSES));
    app.mount(footer, docs);

    let github = app.button("github");
    styler.style(&app, &css, github, PILL_LINK_CLASSES);
    hoverables.push((github, PILL_LINK_CLASSES));
    app.mount(footer, github);

    Welcome {
        app,
        count,
        button,
        css,
        hoverables,
        styled: styler.styled,
    }
}

/// Build the logo subtree — two rows of rounded "leaves" (the canopy) over a short
/// trunk — styling every piece through `styler`, and return its root node.
///
/// The leaves are pure rounded-rect element backgrounds (no text, no image files):
/// `.leaf` gives the rounded geometry and a `.leaf-*` class paints it green/teal/blue.
fn build_logo(app: &App, css: &Stylesheet, styler: &mut Styler) -> NodeId {
    let logo = app.el(COLUMN);
    styler.style(app, css, logo, &["logo"]);

    // Top row of the canopy: green, teal, green.
    let top = app.el(ROW);
    styler.style(app, css, top, &["leafrow"]);
    app.mount(logo, top);
    for class in [
        &["leaf", "leaf-green"][..],
        &["leaf", "leaf-teal"][..],
        &["leaf", "leaf-green"][..],
    ] {
        let leaf = app.el(COLUMN);
        // These class slices are local `'static` literals; record them for reload too.
        styler.style(app, css, leaf, leaf_classes(class));
        app.mount(top, leaf);
    }

    // Bottom row of the canopy: teal, blue (offset, two leaves).
    let bottom = app.el(ROW);
    styler.style(app, css, bottom, &["leafrow"]);
    app.mount(logo, bottom);
    for class in [&["leaf", "leaf-teal"][..], &["leaf", "leaf-blue"][..]] {
        let leaf = app.el(COLUMN);
        styler.style(app, css, leaf, leaf_classes(class));
        app.mount(bottom, leaf);
    }

    // The trunk: one short rounded rect under the canopy.
    let trunkrow = app.el(ROW);
    styler.style(app, css, trunkrow, &["trunkrow"]);
    app.mount(logo, trunkrow);
    let trunk = app.el(COLUMN);
    styler.style(app, css, trunk, &["trunk"]);
    app.mount(trunkrow, trunk);

    logo
}

/// Map a leaf's `["leaf", "leaf-<tint>"]` slice to the `'static` class list the styler
/// stores. The leaf tints are a closed set (green/teal/blue), so this returns a fixed
/// `'static` slice per tint (the styler retains `&'static` classes for reload), with
/// green as the default.
fn leaf_classes(class: &[&str]) -> &'static [&'static str] {
    match class {
        [_, "leaf-teal"] => &["leaf", "leaf-teal"],
        [_, "leaf-blue"] => &["leaf", "leaf-blue"],
        _ => &["leaf", "leaf-green"],
    }
}

/// Resolve a pointer position to the click handler that should fire, using the real
/// Taffy layout for `viewport`, hit-testing the topmost node, and walking up to the
/// nearest ancestor with a `click` listener. Returns `None` if nothing is hit.
///
/// Mirrors `canopy-demo`'s `click_handler`: a button's text label is a *child* of the
/// button element, so the raw hit is usually one level below the node that carries the
/// listener — hence the walk up the parent chain.
#[must_use]
pub fn click_handler(dom: &Dom, viewport: Size, point: Point) -> Option<HandlerId> {
    let (_scene, lay) = layout(dom, viewport);
    let mut node = hit_test(&lay, point)?;
    loop {
        let n = dom.node(node)?;
        if let Some((_, handler)) = n.listeners.iter().find(|(ev, _)| *ev == CLICK) {
            return Some(*handler);
        }
        node = n.parent?;
    }
}

/// Resolve `point` to the [`Welcome::hoverables`] entry under the cursor, if any.
///
/// Lays out `dom` at `viewport`, hit-tests the topmost node, then walks up the parent
/// chain to the nearest ancestor that appears in `hoverables` (a button's text label
/// is a child of the button element, so the raw hit is usually one level below the
/// hoverable node). Returns the matching `(NodeId, classes)` so the host can flip
/// exactly that node's `:hover` state. `None` when nothing hoverable is under the
/// cursor (e.g. the cursor is over the canvas background).
#[must_use]
pub fn hover_target(
    dom: &Dom,
    viewport: Size,
    point: Point,
    hoverables: &[(NodeId, &'static [&'static str])],
) -> Option<(NodeId, &'static [&'static str])> {
    let (_scene, lay) = layout(dom, viewport);
    let mut node = hit_test(&lay, point)?;
    loop {
        if let Some(entry) = hoverables.iter().find(|(id, _)| *id == node) {
            return Some(*entry);
        }
        node = dom.node(node)?.parent?;
    }
}

/// Re-apply a freshly parsed stylesheet to every styled node, emitting the inline-style
/// ops that the host then applies to the live [`Dom`] — the heart of the hot-reload.
///
/// This is the build-side half of the reload loop. Given the newly parsed `css` and the
/// [`Welcome::styled`] registry, it replays each node's classes through
/// [`Stylesheet::apply_state`], so every property the edited stylesheet changed becomes a
/// targeted `SetInlineStyle` against a handle the tree already owns. The node currently
/// under the cursor (`hovered`) is re-resolved *with* its `:hover` rules so a live hover
/// survives the reload instead of flickering back to its base style.
///
/// It does **not** touch the [`Dom`]; it only emits ops on `app`. The host takes the
/// resulting batch and applies it (via [`canopy_hotreload::reapply`]) so a malformed
/// reload can be rejected at the capability boundary without this function needing to
/// know about the host's tree. Returns the number of nodes re-styled (handy for logging).
pub fn reapply_styles(
    app: &App,
    css: &Stylesheet,
    styled: &[(NodeId, &'static [&'static str])],
    hovered: Option<NodeId>,
) -> usize {
    for (node, classes) in styled {
        css.apply_state(app, *node, classes, hovered == Some(*node));
    }
    styled.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_paint::{BG, RADIUS};
    use canopy_traits::OpSink;

    /// Mount a freshly built [`Welcome`] into a [`Dom`] and return both.
    fn mount() -> (Welcome, Dom) {
        let w = build();
        let mut dom = Dom::new();
        dom.apply(&w.app.take_batch(0))
            .expect("mount batch applies");
        (w, dom)
    }

    /// The tree builds, mounts, and contains the expected nodes: the counter button
    /// with a "count is N" label, the two footer pills, and the rounded card.
    #[test]
    fn welcome_tree_builds_with_expected_nodes() {
        let (w, dom) = mount();

        // The button exists and carries a click listener.
        let button = dom.node(w.button).expect("button node exists");
        assert!(
            button.listeners.iter().any(|(ev, _)| *ev == CLICK),
            "counter button has a click listener"
        );

        // Its text child reads "count is 0" at build time (counter starts at 0).
        let label = *button
            .children
            .first()
            .expect("button has a text child label");
        assert_eq!(dom.text_of(label), Some("count is 0"));

        // The hint and tagline copy are present somewhere in the tree.
        let texts: Vec<String> = (0..)
            .map(NodeId::new)
            .take_while(|id| id.raw() < 200)
            .filter_map(|id| dom.text_of(id).map(str::to_string))
            .collect();
        assert!(
            texts
                .iter()
                .any(|t| t == "Edit styles.css and save to hot-reload"),
            "hint line is present"
        );
        assert!(
            texts
                .iter()
                .any(|t| t == "web-like native UI — no JavaScript runtime"),
            "tagline is present"
        );
        assert!(texts.iter().any(|t| t == "docs"), "docs pill is present");
        assert!(
            texts.iter().any(|t| t == "github"),
            "github pill is present"
        );

        // The two footer pills and the counter button are the three hoverables.
        assert_eq!(w.hoverables.len(), 3, "button + docs pill + github pill");

        // Every styled node is recorded for reload, and the card carries a rounded
        // background, proving `border-radius` made it onto a real node.
        assert!(!w.styled.is_empty(), "styled registry is populated");
        let card = w
            .styled
            .iter()
            .find(|(id, classes)| classes.contains(&"card") && dom.contains(*id))
            .map(|(id, _)| *id)
            .expect("card node is styled");
        assert_eq!(dom.style(card, BG), Some("#313244"));
        assert_eq!(dom.style(card, RADIUS), Some("12"));
    }

    /// The counter button's bound label re-renders "count is N" when the signal changes.
    #[test]
    fn counter_label_tracks_the_signal() {
        let (w, mut dom) = mount();
        let label = *dom
            .node(w.button)
            .unwrap()
            .children
            .first()
            .expect("button label");

        w.count.set(3);
        w.app.runtime().flush();
        dom.apply(&w.app.take_batch(1))
            .expect("counter batch applies");
        assert_eq!(dom.text_of(label), Some("count is 3"));
    }

    /// Hot-reload round-trip: build with the on-disk stylesheet, then parse a *second*
    /// stylesheet string with a different `.btn` background, re-apply it via
    /// [`reapply_styles`], push the batch onto the live tree with
    /// [`canopy_hotreload::reapply`], and prove the button node's background changed in
    /// the [`Dom`]. This is the exact path the windowed host runs on a `styles.css` save.
    #[test]
    fn hot_reload_round_trip_restyles_the_button() {
        let (w, mut dom) = mount();

        // Sanity: the button starts with the on-disk `.btn` background.
        assert_eq!(dom.style(w.button, BG), Some("#45475a"));

        // A *new* stylesheet a designer might save: same classes, different `.btn` color
        // and a chunkier radius. We keep it minimal — only the rules that change matter,
        // because `apply_state` re-emits the full resolved set for each styled node.
        const EDITED: &str = "
            .btn  { background: #f9e2af; color: #1e1e2e; padding: 14px; border-radius: 16px }
            .pill { background: #313244; color: #9399b2; padding: 12px; border-radius: 8px }
        ";
        let edited = canopy_style_css::parse(EDITED);

        // Build-side: re-apply every styled node against the edited sheet (nothing
        // hovered), then take the resulting op batch.
        let n = reapply_styles(&w.app, &edited, &w.styled, None);
        assert_eq!(n, w.styled.len(), "every styled node was re-applied");
        let batch = w.app.take_batch(1);

        // Host-side: push the reload batch onto the *existing* tree (the hot-reload glue).
        canopy_hotreload::reapply(&mut dom, &batch).expect("reload batch applies");

        // The button's background and radius now reflect the edited stylesheet, on the
        // same node handle — no remount.
        assert_eq!(dom.style(w.button, BG), Some("#f9e2af"));
        assert_eq!(dom.style(w.button, RADIUS), Some("16"));
    }

    /// A reload that re-resolves the hovered node *with* its `:hover` rules keeps the
    /// hover lighten instead of snapping the button back to its base color.
    #[test]
    fn hot_reload_preserves_a_live_hover() {
        let (w, mut dom) = mount();

        // Re-apply the on-disk sheet with the button marked hovered.
        reapply_styles(&w.app, &w.css, &w.styled, Some(w.button));
        let batch = w.app.take_batch(1);
        canopy_hotreload::reapply(&mut dom, &batch).expect("hover reload applies");

        // The button shows the `.btn:hover` color, not the base `.btn` color.
        assert_eq!(dom.style(w.button, BG), Some("#585b70"));
    }
}
