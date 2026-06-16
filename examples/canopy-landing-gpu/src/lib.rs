//! Canopy **landing (GPU)** — a stunning one-page (no-scroll) welcome screen,
//! rasterized on the **GPU**.
//!
//! This is the GPU twin of [`canopy-landing`]: the UI tree and the animation
//! timeline below are identical to the CPU example — same dark, minimal page (design
//! inspiration: x.ai), same staggered fade + slide-up entrance, same ambient pulsing
//! dot row. The only difference lives in the two binaries: instead of the CPU
//! "sharp-text" software path ([`canopy-render-text`]), the window rasterizes each
//! frame with [`canopy-render-vello`], the `wgpu`-backed `Renderer` (Metal on this
//! Mac). See `src/bin/render.rs` (headless still -> PPM) and `src/bin/window.rs`
//! (the animated window, GPU-rasterized then blitted to softbuffer).
//!
//! It exists to show off what Canopy can do now that the core has opacity +
//! translate + alpha compositing, flex alignment, and a real animation timeline:
//!
//! - **Layout** is honest flexbox: the stage `justify-content: space-between` pins the
//!   nav to the top and the footer to the bottom with the hero centered between them,
//!   and `align-items: center` centers every row — no spacer hacks.
//! - **Entrance** is a staggered choreography: the nav, then the wordmark, tagline,
//!   subline, accent rule, and footer each **fade in while sliding up**, one beat after
//!   the last. Fade = an animated `opacity`; slide = an animated `translate-y`; both
//!   composite correctly over the black canvas because the renderer alpha-blends.
//! - **Ambient** motion: a row of dots **pulse** in a flowing wave — each an
//!   opacity `PingPong` tween with a staggered delay — so the page feels alive after
//!   it settles.
//!
//! Every animation is a `canopy_anim` tween whose `Signal<f32>` is bound to a style via
//! [`Ui::bind_style`]; the windowed host just `tick`s the timeline each frame. The
//! headless renderer [`settle`](Landing::settle)s the one-shot entrance for a still.

use canopy_anim::{Easing, Repeat, Timeline, Tween};
use canopy_paint::{OPACITY, TRANSLATE_Y, WIDTH};
use canopy_ui::prelude::*;

/// Logical window size — a wide 16:10-ish landing canvas.
pub const VIEW_W: f32 = 960.0;
/// Logical window height.
pub const VIEW_H: f32 = 600.0;

/// The editable stylesheet, read at runtime so editing + saving hot-reloads the window.
pub const STYLES_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/styles.css");

/// Read `styles.css` from disk (empty string on failure, so the app still runs).
#[must_use]
pub fn load_styles() -> String {
    std::fs::read_to_string(STYLES_PATH).unwrap_or_default()
}

/// The built landing: the `Ui` context plus the animation `Timeline` the host ticks.
pub struct Landing {
    /// Authoring + host context (batches, hover, hit-testing).
    pub ui: Ui,
    /// The animation clock. The windowed host advances it each frame; it never goes
    /// fully idle because the ambient dots loop forever.
    pub timeline: Timeline,
}

impl Landing {
    /// Advance the one-shot entrance tweens to completion and flush — so a static
    /// (headless) render shows the settled page. The looping dot tweens never finish; a
    /// large tick simply lands them at a stable phase, which is fine for a still.
    pub fn settle(&mut self) {
        self.timeline.tick(5.0);
        self.ui.runtime().flush();
    }
}

/// Assemble the landing and wire its animations.
#[must_use]
pub fn build() -> Landing {
    let ui = Ui::with_css(&load_styles());
    let mut tl = Timeline::default();
    let rt = ui.runtime();

    let stage = el(&ui, &["stage"]);
    ui.mount_root(stage);

    // --- Top nav: brand left, links right (the whole row fades/slides in first). ---
    let nav = el(&ui, &["nav"]);
    ui.mount(stage, nav);
    ui.mount(nav, text(&ui, &["brand"], "Canopy"));
    let links = el(&ui, &["navlinks"]);
    ui.mount(nav, links);
    ui.mount(links, text(&ui, &["navlink"], "Docs"));
    ui.mount(links, text(&ui, &["navlink"], "GitHub"));
    ui.mount(links, text(&ui, &["navlink"], "Spec"));
    enter(&ui, &mut tl, &rt, nav, 0.0);

    // --- Centered hero: wordmark, tagline, subline, accent rule, ambient dots. ---
    let hero = el(&ui, &["hero"]);
    ui.mount(stage, hero);

    let wordmark = text(&ui, &["wordmark"], "Canopy");
    ui.mount(hero, wordmark);
    enter(&ui, &mut tl, &rt, wordmark, 0.16);

    let tagline = text(
        &ui,
        &["tagline"],
        "A native UI runtime \u{2014} no JavaScript.",
    );
    ui.mount(hero, tagline);
    enter(&ui, &mut tl, &rt, tagline, 0.30);

    let subline = text(
        &ui,
        &["subline"],
        "Web-like  \u{00b7}  capability-safe  \u{00b7}  embeddable to bare metal",
    );
    ui.mount(hero, subline);
    enter(&ui, &mut tl, &rt, subline, 0.42);

    // The accent rule draws itself in (width 0 -> full) while fading up.
    let rule = el(&ui, &["rule"]);
    ui.mount(hero, rule);
    enter(&ui, &mut tl, &rt, rule, 0.56);
    {
        let w = Tween::new(0.0, 300.0, 0.7)
            .delay(0.56)
            .easing(Easing::EaseOutCubic)
            .start(&mut tl, &rt);
        ui.bind_style(rule, WIDTH, move || (w.get() as i32).to_string());
    }

    // The ambient dot row: each dot pulses opacity in a staggered ping-pong wave.
    let dotrow = el(&ui, &["dotrow"]);
    ui.mount(hero, dotrow);
    for i in 0..5 {
        let dot = el(&ui, &["dot"]);
        ui.mount(dotrow, dot);
        let pulse = Tween::new(0.22, 1.0, 0.85)
            .delay(0.6 + i as f32 * 0.14)
            .easing(Easing::EaseInOutQuad)
            .repeat(Repeat::PingPong)
            .start(&mut tl, &rt);
        ui.bind_style(dot, OPACITY, move || format!("{:.3}", pulse.get()));
    }

    // --- Footer: a bright CTA pill + muted status, fading in last. ---
    let footer = el(&ui, &["footer"]);
    ui.mount(stage, footer);
    // The CTA is a padded pill (a div, since padding applies to elements, not text
    // leaves) wrapping its dark label.
    let cta = el(&ui, &["cta"]);
    ui.mount(cta, text(&ui, &["cta-text"], "canopy new"));
    ui.mount(footer, cta);
    ui.mount(
        footer,
        text(
            &ui,
            &["status"],
            "v0.0  \u{00b7}  27 crates  \u{00b7}  no JS runtime",
        ),
    );
    enter(&ui, &mut tl, &rt, footer, 0.74);

    Landing { ui, timeline: tl }
}

/// Create a styled element. The class sets its direction/size/color; the element tag is
/// irrelevant to layout, so one `column()` + class covers rows and columns alike.
fn el(ui: &Ui, class: &'static [&'static str]) -> NodeId {
    let n = ui.column();
    ui.class(n, class);
    n
}

/// Create a styled text leaf.
fn text(ui: &Ui, class: &'static [&'static str], s: &str) -> NodeId {
    let n = ui.label(s);
    ui.class(n, class);
    n
}

/// Give `node` a **fade + slide-up** entrance starting after `delay` seconds: opacity
/// `0 -> 1` and translate-y `16 -> 0`, both decelerating. Binding on a container fades
/// and slides its whole subtree (opacity and translate accumulate down the tree).
fn enter(ui: &Ui, tl: &mut Timeline, rt: &Runtime, node: NodeId, delay: f32) {
    let op = Tween::new(0.0, 1.0, 0.55)
        .delay(delay)
        .easing(Easing::EaseOutCubic)
        .start(tl, rt);
    ui.bind_style(node, OPACITY, move || format!("{:.3}", op.get()));

    let ty = Tween::new(16.0, 0.0, 0.6)
        .delay(delay)
        .easing(Easing::EaseOutCubic)
        .start(tl, rt);
    ui.bind_style(node, TRANSLATE_Y, move || format!("{:.2}", ty.get()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_traits::OpSink;
    use canopy_ui::prelude::Dom;

    #[test]
    fn landing_builds_with_hero_copy() {
        let mut landing = build();
        landing.settle();
        let mut dom = Dom::new();
        dom.apply(&landing.ui.take_batch(0)).expect("mount batch");

        let texts: Vec<String> = (0..)
            .map(NodeId::new)
            .take_while(|id| id.raw() < 300)
            .filter_map(|id| dom.text_of(id).map(str::to_string))
            .collect();
        for needle in ["Canopy", "A native UI runtime", "canopy new"] {
            assert!(
                texts.iter().any(|t| t.contains(needle)),
                "{needle:?} present on the page"
            );
        }
    }
}
