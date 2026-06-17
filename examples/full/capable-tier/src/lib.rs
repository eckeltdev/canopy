//! **The capable tier, running live.**
//!
//! [`main.rs`](../main.rs) proves the capable (Servo-**Stylo**) cascade *statically* —
//! it writes PPMs and prints a color contrast. This module proves it **interactively**:
//! it holds a persistent [`StyloEngine`] plus the authored capable [`Dom`] plus a
//! [`SoftwareRenderer`] and drives them from pointer input, so a `:hover` rule visibly
//! repaints as the cursor moves over the row that carries it.
//!
//! The crux is [`CapableApp`]:
//!
//! - [`CapableApp::paint`] re-syncs the engine from the (possibly mutated) Dom, builds
//!   the [`DisplayList`], and renders one frame into the software buffer — exactly the
//!   Dom → Stylo → `DisplayList` → renderer loop the host's [`StyloSceneBuilder`] runs,
//!   just hoisted into a struct that also keeps the input state.
//! - [`CapableApp::on_pointer_move`] uses [`StyloEngine::hit_test`] to find the deepest
//!   element under the cursor and [`StyloEngine::set_hover`] to move the `:hover` state
//!   there, returning whether the hovered element changed so the caller only repaints on
//!   a real transition (the same gate the Stylo browser window uses).
//!
//! The windowed driver lives in [`src/bin/window.rs`](../bin/window.rs) (behind the
//! `window` feature); the headless interactivity test at the bottom of this file drives
//! the same logic with no window, so the live-hover behaviour is verified offline.

use canopy_dom::Dom;
use canopy_protocol::NodeId;
use canopy_render_soft::{Buffer, SoftwareRenderer};
use canopy_style_stylo::StyloEngine;
use canopy_traits::{
    Color, ComputedStyle, DisplayList, OpSink, Point, Renderer, Size, StyleEngine,
};
use canopy_ui::prelude::{Classes, Ui};

/// Logical window width — the tree lays out within this and reflows on resize.
pub const VIEW_W: usize = 380;
/// Logical window height.
pub const VIEW_H: usize = 300;

/// The page color behind the `.app` panel (the renderer clears to this each frame).
pub const CLEAR: Color = Color {
    r: 0x0c,
    g: 0x0d,
    b: 0x10,
    a: 255,
};

/// The capable author CSS, with a real **`:hover`** rule. `.row:hover` is the headline
/// of *this* binary: a selector the lite class subset cannot represent (it has no
/// pseudo-classes), so the row's background only changes under the Stylo cascade, live,
/// as the pointer enters it. The `.card .title` descendant rule (gold) is kept from the
/// static demo so the running tree shows the same combinator-driven cascade.
pub const CAPABLE_CSS: &str = "\
.app   { background: #14161c; padding: 18px; color: #e8eaf0 }
.title { color: #e8eaf0 }
.card  { background: #1c2030; padding: 14px }
.card .title { color: #f5b301 }
.row   { background: #1c2030; color: #aeb4c2; padding: 10px }
.row:hover { background: #2d3346; color: #ffffff }
.muted { color: #5b6172 }
";

/// The handles the interactive app needs to refer back to specific nodes after
/// authoring — chiefly [`row`](Authored::row), the node carrying the `:hover` rule that
/// the headless test drives the pointer onto.
pub struct Authored {
    /// The root `.app` panel.
    pub root: NodeId,
    /// The `.row` node whose background flips on `:hover` — the live-restyle target.
    pub row: NodeId,
}

/// Author the capable tree **once** into `ui` (identity-carrying [`Ui::capable`] mode, so
/// the Dom records each node's tag / class for the host engine to cascade), returning the
/// handles the interactive driver refers back to.
///
/// The shape mirrors [`main.rs`](../main.rs)'s `build_app`: `app › [heading, card ›
/// [title, row], footer]`. The difference is intent — here the `.row` is the live
/// `:hover` target, so its handle is surfaced.
pub fn author(ui: &Ui) -> Authored {
    let app = container(ui, &["app"]);
    let heading = label_band(ui, &["title"], "Canopy  —  capable tier");
    let card = container(ui, &["card"]);
    let title = label_band(ui, &["title"], "Settings");
    let row = label_band(ui, &["row"], "Theme:  dark   (hover me)");
    let footer = label_band(ui, &["muted"], "live :hover via the Stylo cascade");

    ui.mount(card, title);
    ui.mount(card, row);
    ui.mount(app, heading);
    ui.mount(app, card);
    ui.mount(app, footer);
    ui.mount_root(app);

    Authored { root: app, row }
}

/// A `div` container carrying `classes` (no text).
fn container(ui: &Ui, classes: Classes) -> NodeId {
    let el = ui.column();
    ui.tag(el, "div");
    ui.class(el, classes);
    el
}

/// A `div` carrying `classes` with a single text child — a label band.
fn label_band(ui: &Ui, classes: Classes, text: &'static str) -> NodeId {
    let el = ui.column();
    ui.tag(el, "div");
    ui.class(el, classes);
    ui.mount(el, ui.label(text));
    el
}

/// A live capable-tier host: a persistent [`StyloEngine`] cascading the authored
/// [`Dom`], a [`SoftwareRenderer`] it paints into, and the current `:hover` slab so a
/// pointer move only restyles on a real transition.
///
/// This is the windowed counterpart of [`main.rs`](../main.rs)'s `StyloSceneBuilder` /
/// `Host`: the engine is held across frames and re-synced per [`paint`](CapableApp::paint)
/// (so live Dom mutations re-cascade without rebuilding the `Stylist`), and pointer input
/// flips `:hover` state through [`on_pointer_move`](CapableApp::on_pointer_move).
pub struct CapableApp {
    /// The authored retained tree the engine cascades. Owned so a future interaction
    /// could mutate it and re-paint; today it is authored once and read each frame.
    dom: Dom,
    /// The persistent Stylo engine (CSS parsed + `Stylist` built once; re-synced per
    /// paint). Holding it across frames is the live-host path.
    engine: StyloEngine,
    /// The CPU rasterizer the display list is painted into.
    renderer: SoftwareRenderer,
    /// Handles back into the tree (the `:hover` target row).
    authored: Authored,
    /// The arena slab of the element the pointer is currently over (Stylo's `hit_test`
    /// returns slab ids, which `set_hover` consumes), so a `CursorMoved` only restyles
    /// when the hovered element actually changes.
    hover: Option<usize>,
}

impl CapableApp {
    /// Build the live app: author the capable tree, apply its op-batch into a fresh
    /// [`Dom`], and stand up the persistent [`StyloEngine`] + [`SoftwareRenderer`]. No
    /// frame is painted yet — call [`paint`](CapableApp::paint) for the first frame.
    #[must_use]
    pub fn new() -> Self {
        let ui = Ui::capable(CAPABLE_CSS);
        let authored = author(&ui);
        let mut dom = Dom::new();
        dom.apply(&ui.take_batch(0)).expect("apply capable ops");
        let engine = StyloEngine::from_dom(&dom, ui.css_source());
        let renderer = SoftwareRenderer::new(VIEW_W, VIEW_H, CLEAR);
        Self {
            dom,
            engine,
            renderer,
            authored,
            hover: None,
        }
    }

    /// Re-sync the engine from the (possibly mutated) Dom, build the display list at
    /// `viewport`, and render one frame into the software buffer.
    ///
    /// This is the per-frame host loop — the same Dom → Stylo → [`DisplayList`] →
    /// renderer path `main.rs`'s `StyloSceneBuilder::build_scene` runs — so calling it
    /// again after [`on_pointer_move`](CapableApp::on_pointer_move) flips `:hover`
    /// repaints the restyled tree.
    pub fn paint(&mut self, viewport: Size) {
        // Re-sync the overlay from the retained tree (cheap; no `Stylist` rebuild), then
        // size the renderer to match the viewport so a resize reflows correctly.
        self.engine.sync_from_dom(&self.dom);
        // Preserve the current hover after a re-sync (sync clears element state): re-apply
        // it so a repaint that follows a Dom change keeps the hovered row highlighted.
        self.engine.set_hover(self.hover);
        self.engine.set_viewport(viewport);
        self.renderer.resize(viewport);
        let scene: DisplayList = self.engine.build_display_list(viewport);
        self.renderer.render(&scene).expect("render display list");
    }

    /// Map the pointer to the deepest element under it ([`StyloEngine::hit_test`]) and,
    /// **when that element changes**, move the `:hover` state there
    /// ([`StyloEngine::set_hover`], which forces a re-cascade). Returns `true` iff the
    /// hovered element changed, so the caller knows to repaint.
    ///
    /// The caller is expected to [`paint`](CapableApp::paint) on a `true` return: the
    /// `set_hover` here only invalidates the cascade; the next paint resolves it.
    pub fn on_pointer_move(&mut self, point: Point, viewport: Size) -> bool {
        let hit = self.engine.hit_test(point, viewport);
        if hit == self.hover {
            return false;
        }
        self.hover = hit;
        self.engine.set_hover(hit);
        true
    }

    /// The current frame buffer (RGBA8) — what the window blits, and what the headless
    /// test inspects pixel-by-pixel.
    #[must_use]
    pub fn buffer(&self) -> &Buffer {
        self.renderer.buffer()
    }

    /// The handles back into the authored tree (the `:hover` target row).
    #[must_use]
    pub fn authored(&self) -> &Authored {
        &self.authored
    }

    /// The flat [`ComputedStyle`] the Stylo cascade currently resolves for `node` (the
    /// public [`StyleEngine`] seam — maps the Dom handle to its overlay and answers from
    /// the cascade). Reflects the live `:hover` state, so a caller can read that the
    /// hovered row resolved its `:hover` background without rasterizing.
    pub fn resolve_style(&mut self, node: NodeId) -> Option<ComputedStyle> {
        self.engine.resolve(node, None).ok()
    }
}

impl Default for CapableApp {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Size {
        Size {
            w: VIEW_W as f32,
            h: VIEW_H as f32,
        }
    }

    /// The row's `:hover` background, the headline color the cascade applies live.
    const HOVER_BG: Color = Color {
        r: 0x2d,
        g: 0x33,
        b: 0x46,
        a: 255,
    };

    /// Count pixels of exactly `color` over the logical viewport.
    fn count_pixels(buf: &Buffer, color: [u8; 4]) -> usize {
        (0..VIEW_H)
            .flat_map(|y| (0..VIEW_W).map(move |x| (x, y)))
            .filter(|&(x, y)| buf.pixel(x, y) == color)
            .count()
    }

    /// Find a viewport point that lands the pointer on the `:hover` row, without
    /// hard-coding layout pixels: scan a vertical line down the panel's center and return
    /// the first point where moving the pointer there makes the Stylo cascade resolve the
    /// row's **`:hover`** background. Runs on a throwaway app so it leaves the app under
    /// test untouched. (Hit-test + the `:hover` cascade are exactly what the production
    /// `on_pointer_move` exercises, so this also confirms the row is hittable.)
    fn point_over_row(vp: Size) -> Point {
        let mut probe = CapableApp::new();
        let row = probe.authored().row;
        let x = vp.w / 2.0;
        let mut y = 1.0;
        while y < vp.h {
            let p = Point { x, y };
            probe.on_pointer_move(p, vp);
            if probe.resolve_style(row).map(|s| s.background) == Some(HOVER_BG) {
                return p;
            }
            y += 1.0;
        }
        panic!("no point on the panel's center line hovers the `.row`");
    }

    /// **The verifiable core of B6 — live `:hover`, no window.** Build the capable app,
    /// paint a baseline frame, then drive a pointer move onto the `.row` that carries the
    /// `:hover` rule and repaint. The hovered element must change (so the driver knows to
    /// repaint), and the rendered pixels must change: the row's hover background
    /// (`#2d3346`) must appear where it was absent before, proving the Stylo cascade
    /// re-ran the `:hover` rule and the renderer reflected it.
    #[test]
    fn pointer_hover_restyles_and_repaints_the_row() {
        let vp = viewport();
        let hover_bg = [HOVER_BG.r, HOVER_BG.g, HOVER_BG.b, HOVER_BG.a];

        let mut app = CapableApp::new();

        // Baseline frame: nothing hovered, so the row sits at its resting `.row`
        // background and none of the hover color is present.
        app.paint(vp);
        let before = app.buffer().data().to_vec();
        assert_eq!(
            count_pixels(app.buffer(), hover_bg),
            0,
            "no hover background before the pointer enters the row"
        );

        // Aim the pointer at the `:hover` row (located from its real cascade, so the test
        // does not hard-code layout pixels).
        let over_row = point_over_row(vp);

        // The pointer move must report a hover change (the caller's repaint gate)…
        let changed = app.on_pointer_move(over_row, vp);
        assert!(changed, "moving onto the row changes the hovered element");

        // …and repainting must visibly differ from the baseline.
        app.paint(vp);
        let after = app.buffer().data().to_vec();
        assert_ne!(
            before, after,
            "the hover restyle changed the rendered pixels"
        );

        // The cascade resolved the `:hover` rule on the row itself…
        assert_eq!(
            app.resolve_style(app.authored().row).map(|s| s.background),
            Some(HOVER_BG),
            "the `.row:hover` rule resolved the hover background"
        );
        // …and concretely the row's `:hover` background now paints a real region.
        let hovered_px = count_pixels(app.buffer(), hover_bg);
        assert!(
            hovered_px > 200,
            "the `.row:hover` background (#2d3346) painted a real region; got {hovered_px} px"
        );
    }

    /// Moving the pointer back off the row (here: to the origin, the `.app` padding gutter
    /// outside any inner box's hover) clears the hover and restores the resting frame —
    /// the round-trip of the live-restyle loop.
    #[test]
    fn leaving_the_row_clears_the_hover() {
        let vp = viewport();
        let hover_bg = [HOVER_BG.r, HOVER_BG.g, HOVER_BG.b, HOVER_BG.a];

        let mut app = CapableApp::new();
        app.paint(vp);
        let resting = app.buffer().data().to_vec();

        let over_row = point_over_row(vp);
        assert!(app.on_pointer_move(over_row, vp), "hover enters the row");
        app.paint(vp);
        assert!(
            count_pixels(app.buffer(), hover_bg) > 200,
            "row is hovered after entering it"
        );

        // Leave: top-left corner is the `.app` panel itself (the row's hover must clear).
        let left = app.on_pointer_move(Point { x: 1.0, y: 1.0 }, vp);
        assert!(left, "moving off the row changes the hovered element");
        app.paint(vp);
        assert_eq!(
            count_pixels(app.buffer(), hover_bg),
            0,
            "no hover background remains after leaving the row"
        );
        assert_eq!(
            resting,
            app.buffer().data(),
            "leaving the row restores the exact resting frame"
        );
    }

    /// The hovered-element gate: re-issuing the *same* pointer position over the row does
    /// not report a change (so the window does not needlessly repaint a static frame).
    #[test]
    fn re_entering_the_same_element_reports_no_change() {
        let vp = viewport();
        let mut app = CapableApp::new();
        app.paint(vp);

        let over_row = point_over_row(vp);
        assert!(
            app.on_pointer_move(over_row, vp),
            "first move onto row changes"
        );
        assert!(
            !app.on_pointer_move(over_row, vp),
            "staying on the same element reports no hover change"
        );
    }
}
