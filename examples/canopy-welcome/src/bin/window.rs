//! The windowed Canopy **welcome** app (macOS / Windows / Linux).
//!
//! Opens a winit window, presents the rendered frame via softbuffer, routes pointer
//! input into the reactive app, and — the signature feature — **hot-reloads
//! `styles.css`**: edit the stylesheet, save, and the live window restyles without a
//! restart. The UI is built by [`canopy_welcome::build`]; this file is only platform
//! glue, and almost all of its app logic is a one-liner against the [`Ui`] context:
//!
//! - **Click** → [`Ui::click_handler`] + [`Ui::dispatch`] (the counter increments and
//!   its bound "count is N" label re-renders).
//! - **`:hover`** → [`Ui::hover_target`] + [`Ui::set_hover`] (buttons/pills lighten
//!   under the pointer).
//! - **Hot reload** → [`Ui::reload_css`] (re-resolve every styled node against the
//!   edited sheet).
//!
//! Text is rasterized by [`canopy_render_text`] with real antialiased cosmic-text
//! glyphs; a single [`TextEngine`] is held across redraws so the font loads once.
//!
//! ## Hot reload — the loop (`canopy-hotreload`)
//!
//! A [`canopy_hotreload::Watcher`] watches `styles.css`. Its callback runs on the
//! watcher's debounce thread, so it cannot touch the [`Dom`] directly; it sends a
//! [`ReloadEvent`] through a channel and wakes the winit loop via an [`EventLoopProxy`].
//! Back on the **main thread**, each wake re-reads the file, calls [`Ui::reload_css`]
//! (which re-resolves every styled node, preserving a live hover), takes the op batch,
//! and pushes it onto the tree via [`canopy_hotreload::reapply`] — a malformed reload is
//! rejected at the capability boundary. Doing the `Dom` mutation on the main thread
//! keeps the tree single-owner and lock-free.
//!
//! Run from `examples/canopy-welcome`: `cargo run`.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};

use canopy_hotreload::{reapply, ReloadEvent, Watcher};
use canopy_text_parley::TextEngine;
use canopy_traits::{Color, OpSink, Point, Size};
use canopy_ui::prelude::{Dom, EventPayload, NodeId};
use canopy_welcome::{build, load_styles, Welcome, STYLES_PATH, VIEW_H, VIEW_W};

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
/// wakes the loop with this marker (an [`EventLoopProxy`] payload must be `'static`, and
/// the reload data already flows through the channel we own). The marker just says "go
/// look".
#[derive(Debug)]
struct ReloadPending;

struct WelcomeApp {
    welcome: Welcome,
    dom: Dom,
    seq: u32,
    cursor: PhysicalPosition<f64>,
    /// The hoverable currently under the cursor, so we only restyle when it changes (and
    /// so a reload can keep a live hover lit).
    hovered: Option<NodeId>,
    /// Reused across redraws so the bundled font is loaded/rasterized once.
    text: TextEngine,
    /// One [`ReloadEvent`] per debounced `styles.css` save from the watcher thread.
    reloads: Receiver<ReloadEvent>,
    /// Kept alive so the OS watch stays armed; dropping it stops hot reload.
    _watcher: Watcher,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
}

impl WelcomeApp {
    /// Build the app and arm the `styles.css` watcher.
    fn new(proxy: EventLoopProxy<ReloadPending>) -> Self {
        let welcome = build();
        let mut dom = Dom::new();
        dom.apply(&welcome.ui.take_batch(0)).expect("initial batch");

        let (reload_tx, reloads) = mpsc::channel::<ReloadEvent>();
        let watcher = Watcher::new(STYLES_PATH, move |event| {
            // Off the main thread: forward the event and wake the loop. A failed send
            // means the app is shutting down, so dropping the reload is correct.
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

    /// Drain everything the [`Ui`] emitted into the retained [`Dom`] and request a
    /// redraw.
    fn apply_and_redraw(&mut self) {
        let batch = self.welcome.ui.take_batch(self.seq);
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

        // Lay out + rasterize with real antialiased glyphs, reusing the held TextEngine.
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
        if let Some(handler) = self
            .welcome
            .ui
            .click_handler(&self.dom, self.viewport(), point)
        {
            self.welcome.ui.dispatch(handler, EventPayload::None);
            self.apply_and_redraw();
        }
    }

    /// Update `:hover` when the cursor moves: find the hoverable under the cursor and, if
    /// it changed, un-hover the old node and hover the new one, then apply + redraw.
    fn on_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        self.cursor = position;
        let point = Point {
            x: position.x as f32,
            y: position.y as f32,
        };
        let target = self
            .welcome
            .ui
            .hover_target(&self.dom, self.viewport(), point);
        if target == self.hovered {
            return; // Same node (or still nothing): no style change needed.
        }
        if let Some(old) = self.hovered {
            self.welcome.ui.set_hover(old, false);
        }
        if let Some(new) = target {
            self.welcome.ui.set_hover(new, true);
        }
        self.hovered = target;
        self.apply_and_redraw();
    }

    /// Drain every pending `styles.css` reload and restyle the live tree. Runs on the
    /// main thread when the loop is woken by [`ReloadPending`].
    fn drain_reloads(&mut self) {
        let mut changed = false;
        while let Ok(event) = self.reloads.try_recv() {
            // Re-read the edited stylesheet and re-resolve every styled node against it,
            // keeping a live hover lit so it doesn't flicker back to base on save.
            let n = self.welcome.ui.reload_css(&load_styles(), self.hovered);

            // Push the restyle batch onto the live tree; keep the old tree on error.
            let batch = self.welcome.ui.take_batch(self.seq);
            self.seq += 1;
            match reapply(&mut self.dom, &batch) {
                Ok(()) => {
                    println!(
                        "hot reload: restyled {n} nodes from {STYLES_PATH} (changed: {:?})",
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
