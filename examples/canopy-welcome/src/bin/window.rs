//! The windowed Canopy **welcome** app (macOS / Windows / Linux).
//!
//! Opens a winit window, presents the rendered frame via softbuffer, routes pointer
//! input into the reactive app, and — the signature feature — **hot-reloads
//! `styles.css`**: edit the stylesheet, save, and the live window restyles without a
//! restart. The UI itself is built by [`canopy_welcome::build`]; this file is only the
//! platform glue, which is exactly the boundary Canopy is designed around.
//!
//! ## What this host drives
//!
//! - **Sharp text.** The frame is rasterized by [`canopy_render_text`] with real
//!   antialiased cosmic-text glyphs. A single [`TextEngine`] is held across redraws
//!   (via [`canopy_render_text::render_dom_with`]) so the font and its glyph caches are
//!   loaded once, not per frame.
//! - **Click.** A left click hit-tests the cursor against the live Taffy layout
//!   ([`canopy_welcome::click_handler`]) and dispatches the counter button's handler, so
//!   the button increments and its bound "count is N" label re-renders.
//! - **`:hover`.** Each `CursorMoved` finds the hoverable under the cursor
//!   ([`canopy_welcome::hover_target`]); when it changes, the old node is re-resolved
//!   without hover and the new one with hover ([`Stylesheet::apply_state`]), so buttons
//!   and pills lighten under the pointer.
//!
//! ## Hot reload — the loop (`canopy-hotreload`)
//!
//! A [`canopy_hotreload::Watcher`] watches `styles.css`. Its callback runs on the
//! watcher's own debounce thread, so it cannot touch the [`Dom`] directly; instead it
//! sends a [`ReloadEvent`] through a channel and asks winit to wake the event loop via
//! an [`EventLoopProxy`]. Back on the **main thread**, on each wake we:
//!
//! 1. read + re-parse the edited `styles.css` ([`canopy_welcome::load_stylesheet`]),
//! 2. re-apply **every** styled node's classes against the new sheet
//!    ([`canopy_welcome::reapply_styles`], preserving any live hover),
//! 3. take the resulting op batch and push it onto the live tree
//!    ([`canopy_hotreload::reapply`]) — a malformed reload is rejected at the
//!    capability boundary and the old tree is kept,
//! 4. request a redraw.
//!
//! Doing the `Dom` mutation on the main thread (not the watcher thread) keeps the tree
//! single-owner and lock-free; the watcher only ever sends a wake + the event.
//!
//! Run from `examples/canopy-welcome`: `cargo run`.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};

use canopy_dom::Dom;
use canopy_hotreload::{reapply, ReloadEvent, Watcher};
use canopy_protocol::{EventPayload, NodeId};
use canopy_text_parley::TextEngine;
use canopy_traits::{Color, OpSink, Point, Size};
use canopy_welcome::{
    build, click_handler, hover_target, load_stylesheet, reapply_styles, Welcome, STYLES_PATH,
    VIEW_H, VIEW_W,
};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

/// The dark-canvas clear color (Catppuccin base). The `.canvas` element paints it too;
/// clearing to it keeps any sub-pixel gaps on-palette.
const CLEAR: Color = Color {
    r: 0x1e,
    g: 0x1e,
    b: 0x2e,
    a: 255,
};

/// The custom winit user-event: "a `styles.css` reload is pending; drain the channel."
///
/// The watcher thread sends the [`ReloadEvent`] through a [`mpsc`] channel and then
/// wakes the loop with this marker. We don't ship the event *in* the user-event because
/// `EventLoopProxy::send_event` needs a `'static` payload and we'd rather keep the
/// reload data flowing through the channel we already own. The marker just says "go
/// look".
#[derive(Debug)]
struct ReloadPending;

struct WelcomeApp {
    welcome: Welcome,
    dom: Dom,
    seq: u32,
    cursor: PhysicalPosition<f64>,
    /// The hoverable currently under the cursor, so we only re-resolve styles when the
    /// hovered node actually changes. Also consulted on reload so a live hover survives.
    hovered: Option<NodeId>,
    /// Reused across redraws so the bundled font is shaped/rasterized once and its glyph
    /// caches persist (see [`canopy_render_text::render_dom_with`]).
    text: TextEngine,
    /// Receives one [`ReloadEvent`] per debounced `styles.css` save from the watcher
    /// thread; drained on the main thread when the loop is woken by [`ReloadPending`].
    reloads: Receiver<ReloadEvent>,
    /// Kept alive for the life of the app so the OS watch stays armed; dropping it stops
    /// hot reload. The `_` name documents that it is a guard, not otherwise read.
    _watcher: Watcher,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
}

impl WelcomeApp {
    /// Build the app and arm the `styles.css` watcher.
    ///
    /// The watcher callback runs off-thread: it forwards each debounced [`ReloadEvent`]
    /// down `reload_tx` and wakes the winit loop through `proxy`, so the actual reload
    /// work happens back on the main thread in [`Self::drain_reloads`].
    fn new(proxy: EventLoopProxy<ReloadPending>) -> Self {
        let welcome = build();
        let mut dom = Dom::new();
        // Mount the initial tree.
        dom.apply(&welcome.app.take_batch(0))
            .expect("initial batch");

        let (reload_tx, reloads) = mpsc::channel::<ReloadEvent>();
        let watcher = Watcher::new(STYLES_PATH, move |event| {
            // Off the main thread: just forward the event and wake the loop. If either
            // send fails the app is shutting down, so dropping the reload is correct.
            if reload_tx.send(event).is_ok() {
                let _ = proxy.send_event(ReloadPending);
            }
        })
        .expect("watch styles.css");

        Self {
            welcome,
            dom,
            seq: 1,
            cursor: PhysicalPosition::new(0.0, 0.0),
            hovered: None,
            text: TextEngine::new(),
            reloads,
            _watcher: watcher,
            window: None,
            surface: None,
        }
    }

    /// Apply a freshly-taken op batch to the retained [`Dom`] and request a redraw.
    fn apply_and_redraw(&mut self) {
        let batch = self.welcome.app.take_batch(self.seq);
        self.seq += 1;
        let _ = self.dom.apply(&batch);
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn viewport(&self) -> Size {
        match &self.window {
            Some(w) => {
                let s = w.inner_size();
                Size {
                    w: s.width.max(1) as f32,
                    h: s.height.max(1) as f32,
                }
            }
            None => Size {
                w: VIEW_W,
                h: VIEW_H,
            },
        }
    }

    fn redraw(&mut self) {
        let viewport = self.viewport();
        let (w, h) = (viewport.w as usize, viewport.h as usize);

        // Lay out + rasterize the tree with real antialiased glyphs, reusing the held
        // TextEngine so shaping/rasterization caches persist across frames.
        let buf = canopy_render_text::render_dom_with(&self.dom, viewport, CLEAR, &mut self.text);

        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return;
        };
        surface
            .resize(
                NonZeroU32::new(w as u32).unwrap(),
                NonZeroU32::new(h as u32).unwrap(),
            )
            .expect("resize surface");
        let mut frame = surface.buffer_mut().expect("surface buffer");
        let rgba = buf.data();
        for (dst, src) in frame.iter_mut().zip(rgba.chunks_exact(4)) {
            // softbuffer wants 0RGB packed in a u32.
            *dst = ((src[0] as u32) << 16) | ((src[1] as u32) << 8) | (src[2] as u32);
        }
        window.pre_present_notify();
        frame.present().expect("present");
    }

    fn on_click(&mut self) {
        let point = Point {
            x: self.cursor.x as f32,
            y: self.cursor.y as f32,
        };
        if let Some(handler) = click_handler(&self.dom, self.viewport(), point) {
            self.welcome.app.dispatch(handler, EventPayload::None);
            self.apply_and_redraw();
        }
    }

    /// Update `:hover` when the cursor moves: find the hoverable under the cursor and,
    /// if it changed, re-resolve the old node's style without hover and the new node's
    /// style with hover, then apply + redraw.
    fn on_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        self.cursor = position;
        let point = Point {
            x: position.x as f32,
            y: position.y as f32,
        };
        let target = hover_target(&self.dom, self.viewport(), point, &self.welcome.hoverables);
        let new_hovered = target.map(|(id, _)| id);
        if new_hovered == self.hovered {
            return; // Same node (or still nothing): no style change needed.
        }

        // Un-hover the node we left (re-resolve its base style)...
        if let Some(old) = self.hovered {
            if let Some((_, classes)) = self.welcome.hoverables.iter().find(|(id, _)| *id == old) {
                self.welcome
                    .css
                    .apply_state(&self.welcome.app, old, classes, false);
            }
        }
        // ...and hover the node we entered.
        if let Some((id, classes)) = target {
            self.welcome
                .css
                .apply_state(&self.welcome.app, id, classes, true);
        }
        self.hovered = new_hovered;
        self.apply_and_redraw();
    }

    /// Drain every pending `styles.css` reload and restyle the live tree.
    ///
    /// Called on the main thread when the loop is woken by [`ReloadPending`]. For each
    /// debounced save we re-read + re-parse the file, swap the parsed sheet into
    /// [`Welcome::css`] (so subsequent `:hover` resolves use the new rules), re-apply
    /// every styled node against it (preserving the live hover), take the op batch, and
    /// push it onto the existing tree via [`reapply`]. A rejected batch leaves the old
    /// tree intact and is logged.
    fn drain_reloads(&mut self) {
        let mut changed = false;
        while let Ok(event) = self.reloads.try_recv() {
            // Re-read + re-parse the edited stylesheet from disk.
            let fresh = load_stylesheet();

            // Re-apply every styled node against the new sheet, keeping the hovered node
            // lit so a live hover doesn't flicker back to base on save.
            reapply_styles(
                &self.welcome.app,
                &fresh,
                &self.welcome.styled,
                self.hovered,
            );

            // Swap the parsed sheet in so future hover resolves use the edited rules.
            self.welcome.css = fresh;

            // Push the restyle batch onto the live tree. Keep the old tree on error.
            let batch = self.welcome.app.take_batch(self.seq);
            self.seq += 1;
            match reapply(&mut self.dom, &batch) {
                Ok(()) => {
                    println!(
                        "hot reload: restyled {} nodes from {} (changed: {:?})",
                        self.welcome.styled.len(),
                        STYLES_PATH,
                        event.path()
                    );
                    changed = true;
                }
                Err(e) => eprintln!("hot reload: batch rejected, keeping old styles: {e}"),
            }
        }
        if changed {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }
}

impl ApplicationHandler<ReloadPending> for WelcomeApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Canopy")
            .with_inner_size(LogicalSize::new(VIEW_W, VIEW_H));
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface =
            softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.window = Some(window);
        self.surface = Some(surface);
    }

    /// The watcher woke us: a `styles.css` save is pending. Drain + restyle.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ReloadPending) {
        self.drain_reloads();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::CursorMoved { position, .. } => self.on_cursor_moved(position),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.on_click(),
            _ => {}
        }
    }
}

fn main() {
    // A `with_user_event` loop so the watcher thread can wake us via an `EventLoopProxy`
    // when `styles.css` changes (the proxy is the only thread-safe handle into winit).
    let event_loop = EventLoop::<ReloadPending>::with_user_event()
        .build()
        .expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = WelcomeApp::new(proxy);
    event_loop.run_app(&mut app).expect("run app");
}
