//! The windowed Canopy demo (macOS / Windows / Linux).
//!
//! Opens a winit window, presents the rendered frame via softbuffer, and routes
//! pointer + keyboard input back into the reactive app. The UI itself is built by
//! [`canopy_demo::build`]; this file is only the platform glue, which is exactly the
//! boundary Canopy is designed around — a real `Platform`/`Renderer` backend swaps in
//! here without touching the UI.
//!
//! ## What changed
//! - **Sharp text.** The base frame is rasterized by [`canopy_render_text`] with real
//!   antialiased cosmic-text glyphs instead of the baked 8x8 software font. A single
//!   [`TextEngine`] is held across redraws (via [`canopy_render_text::render_dom_with`])
//!   so the font and its glyph caches are loaded once, not per frame.
//! - **`:hover`.** Each `CursorMoved` hit-tests the cursor against the live Taffy
//!   layout ([`canopy_demo::hover_target`]); when the hovered button changes, the old
//!   node's style is re-resolved with `hovered = false` and the new one with
//!   `hovered = true` ([`Stylesheet::apply_state`]), emitting inline-style ops that we
//!   apply and redraw — so buttons lighten under the pointer.
//!
//! ## GPU path (optional, `--gpu` + the `gpu` feature)
//! With the `gpu` feature built in and `--gpu` on the command line, the base scene is
//! rasterized on the GPU via [`canopy_render_vello::try_render_to_rgba`] and those
//! RGBA bytes are blitted to softbuffer. **This is a demonstration of the quad
//! pipeline, not the sharp-text path:** `canopy-render-vello` expands text into the
//! *baked 8x8 font* (one quad per ink pixel) and does not rasterize the cosmic-text
//! glyphs, and it renders only the app scene — the untrusted-plugin panel is *not*
//! composited on the GPU path. The CPU sharp-text path is always the default and is
//! the only complete one; `--gpu` falls back to it automatically if no GPU adapter is
//! available. See the crate's Cargo `gpu` feature comment.
//!
//! Run from `examples/canopy-demo`: `cargo run` (CPU sharp text), or
//! `cargo run --features gpu -- --gpu` (GPU quad demo).

use std::num::NonZeroU32;
use std::rc::Rc;

use canopy_demo::{build, click_handler, hover_target, run_plugin, Demo, VIEW_H, VIEW_W};
use canopy_dom::Dom;
use canopy_input::Key;
use canopy_protocol::{EventPayload, NodeId};
use canopy_render_soft::Buffer;
use canopy_text_parley::TextEngine;
use canopy_traits::{Color, OpSink, Point, Rect, Size};
use canopy_transport_wasmtime::PluginHost;

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key as WinitKey, NamedKey};
use winit::window::{Window, WindowId};

/// Translate a winit key press into a Canopy text-edit [`Key`].
fn translate_key(event: &KeyEvent) -> Option<Key> {
    match &event.logical_key {
        WinitKey::Named(NamedKey::Backspace) => Some(Key::Backspace),
        WinitKey::Named(NamedKey::Enter) => Some(Key::Enter),
        WinitKey::Named(NamedKey::Space) => Some(Key::Char(' ')),
        _ => event
            .text
            .as_ref()
            .and_then(|s| s.chars().next())
            .filter(|c| !c.is_control())
            .map(Key::Char),
    }
}

const CLEAR: Color = Color {
    r: 0x1e,
    g: 0x1e,
    b: 0x2e,
    a: 255,
};

struct DemoApp {
    demo: Demo,
    dom: Dom,
    seq: u32,
    cursor: PhysicalPosition<f64>,
    /// The button currently under the cursor (a [`Demo::hoverables`] node), so we only
    /// re-resolve styles when the hovered node actually changes.
    hovered: Option<NodeId>,
    /// Reused across redraws so the bundled font is shaped/rasterized once and its
    /// glyph caches persist (see [`canopy_render_text::render_dom_with`]).
    text: TextEngine,
    /// Whether the optional GPU render path is active (requested via `--gpu` and not
    /// yet disproven by a missing adapter). Only exists when the `gpu` feature is built;
    /// the CPU sharp-text path needs no such state.
    #[cfg(feature = "gpu")]
    gpu: bool,
    /// The untrusted wasm plugin, run once at startup; its `dom()` is composited into a
    /// panel each frame. `None` if the sandbox couldn't load it.
    plugin: Option<PluginHost>,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
}

impl DemoApp {
    /// Build the app. `gpu` requests the optional GPU path; it is ignored (and the
    /// parameter unused) unless the `gpu` feature is compiled in.
    fn new(#[cfg_attr(not(feature = "gpu"), allow(unused_variables))] gpu: bool) -> Self {
        let demo = build();
        let mut dom = Dom::new();
        // Mount the initial tree.
        dom.apply(&demo.app.take_batch(0)).expect("initial batch");
        Self {
            demo,
            dom,
            seq: 1,
            cursor: PhysicalPosition::new(0.0, 0.0),
            hovered: None,
            text: TextEngine::new(),
            #[cfg(feature = "gpu")]
            gpu,
            plugin: run_plugin(),
            window: None,
            surface: None,
        }
    }

    /// Apply a freshly-taken op batch to the retained [`Dom`] and request a redraw.
    fn apply_and_redraw(&mut self) {
        let batch = self.demo.app.take_batch(self.seq);
        self.seq += 1;
        let _ = self.dom.apply(&batch);
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Type a key into the focused text field, then re-apply and redraw.
    fn on_key(&mut self, key: Key) {
        self.demo.app.type_into(self.demo.input, key);
        self.apply_and_redraw();
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

    /// Composite the untrusted plugin into a bordered panel on the right of `buf`.
    ///
    /// Identical to the previous software path: a pink label, a bordered frame, and
    /// the plugin's own DOM painted inside via [`canopy_plugin_panel::render_panel`].
    /// Runs on the CPU [`Buffer`] surface that [`canopy_render_text`] returns.
    fn composite_plugin_panel(&self, buf: &mut Buffer, viewport: Size) {
        let label = Color {
            r: 0xf3,
            g: 0x8b,
            b: 0xa8,
            a: 255,
        };
        let border = Color {
            r: 0x45,
            g: 0x47,
            b: 0x5a,
            a: 255,
        };
        let plugin_bg = Color {
            r: 0x18,
            g: 0x18,
            b: 0x25,
            a: 255,
        };
        let px = (viewport.w - 312.0).max(0.0);
        buf.blit_text(Point { x: px, y: 24.0 }, "untrusted plugin:", label, 16.0);
        let frame = Rect {
            origin: Point { x: px, y: 48.0 },
            size: Size { w: 300.0, h: 104.0 },
        };
        let inner = Rect {
            origin: Point {
                x: px + 2.0,
                y: 50.0,
            },
            size: Size { w: 296.0, h: 100.0 },
        };
        buf.fill_rect(frame, border);
        buf.fill_rect(inner, plugin_bg);
        if let Some(host) = &self.plugin {
            canopy_plugin_panel::render_panel(buf, inner, host.dom());
        }
    }

    fn redraw(&mut self) {
        let viewport = self.viewport();
        let (w, h) = (viewport.w as usize, viewport.h as usize);

        // The optional GPU path renders the app scene with the quad pipeline and blits
        // it directly. It is best-effort and incomplete (baked font, no plugin panel),
        // so it lives behind the feature+flag and silently falls back to the CPU path
        // if the adapter or the readback is unavailable.
        #[cfg(feature = "gpu")]
        if self.gpu && self.present_gpu(viewport) {
            return;
        }

        // CPU sharp-text path (the default, always complete): lay out + rasterize the
        // tree with real antialiased glyphs, reusing the held TextEngine, then
        // composite the untrusted-plugin panel onto the same buffer.
        let mut buf =
            canopy_render_text::render_dom_with(&self.dom, viewport, CLEAR, &mut self.text);
        self.composite_plugin_panel(&mut buf, viewport);

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

    /// GPU render path: rasterize the app scene on the GPU and blit the RGBA frame to
    /// softbuffer. Returns `false` (so the caller falls back to the CPU path) if no GPU
    /// adapter is available or the window/surface isn't ready.
    ///
    /// Note this renders only the app scene — the baked-font glyphs and *no* plugin
    /// panel — which is why it is gated and never the default. See the module docs.
    #[cfg(feature = "gpu")]
    fn present_gpu(&mut self, viewport: Size) -> bool {
        use canopy_layout_taffy::build_scene;

        let (w, h) = (viewport.w as usize, viewport.h as usize);
        let scene = build_scene(&self.dom, viewport);
        let Some(rgba) = canopy_render_vello::try_render_to_rgba(&scene, viewport, CLEAR) else {
            // No adapter: let the caller fall back to the CPU path. Drop the flag so we
            // don't re-probe the (absent) GPU every frame.
            self.gpu = false;
            return false;
        };
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return false;
        };
        surface
            .resize(
                NonZeroU32::new(w as u32).unwrap(),
                NonZeroU32::new(h as u32).unwrap(),
            )
            .expect("resize surface");
        let mut frame = surface.buffer_mut().expect("surface buffer");
        for (dst, src) in frame.iter_mut().zip(rgba.chunks_exact(4)) {
            *dst = ((src[0] as u32) << 16) | ((src[1] as u32) << 8) | (src[2] as u32);
        }
        window.pre_present_notify();
        frame.present().expect("present");
        true
    }

    fn on_click(&mut self) {
        let point = Point {
            x: self.cursor.x as f32,
            y: self.cursor.y as f32,
        };
        if let Some(handler) = click_handler(&self.dom, self.viewport(), point) {
            self.demo.app.dispatch(handler, EventPayload::None);
            self.apply_and_redraw();
        }
    }

    /// Update `:hover` when the cursor moves: find the hoverable node under the cursor
    /// and, if it changed, re-resolve the old node's style without hover and the new
    /// node's style with hover, then apply + redraw.
    fn on_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        self.cursor = position;
        let point = Point {
            x: position.x as f32,
            y: position.y as f32,
        };
        let target = hover_target(&self.dom, self.viewport(), point, &self.demo.hoverables);
        let new_hovered = target.map(|(id, _)| id);
        if new_hovered == self.hovered {
            return; // Same node (or still nothing): no style change needed.
        }

        // Un-hover the node we left (re-resolve its base style)...
        if let Some(old) = self.hovered {
            if let Some((_, classes)) = self.demo.hoverables.iter().find(|(id, _)| *id == old) {
                self.demo
                    .css
                    .apply_state(&self.demo.app, old, classes, false);
            }
        }
        // ...and hover the node we entered.
        if let Some((id, classes)) = target {
            self.demo.css.apply_state(&self.demo.app, id, classes, true);
        }
        self.hovered = new_hovered;
        self.apply_and_redraw();
    }
}

impl ApplicationHandler for DemoApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Canopy demo")
            .with_inner_size(LogicalSize::new(VIEW_W, VIEW_H));
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface =
            softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.window = Some(window);
        self.surface = Some(surface);
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
            WindowEvent::KeyboardInput {
                event: ref key_event,
                ..
            } if key_event.state == ElementState::Pressed => {
                if let Some(key) = translate_key(key_event) {
                    self.on_key(key);
                }
            }
            _ => {}
        }
    }
}

fn main() {
    // The GPU path is opt-in: it requires both the `gpu` build feature and `--gpu` on
    // the command line. Without the feature the flag is accepted but inert (the CPU
    // sharp-text path always runs), so the same invocation works either way.
    let want_gpu = std::env::args().any(|a| a == "--gpu");
    if want_gpu && cfg!(not(feature = "gpu")) {
        eprintln!(
            "--gpu was passed but the demo was built without the `gpu` feature; \
             rendering on the CPU sharp-text path. Rebuild with `--features gpu`."
        );
    }
    let gpu = want_gpu && cfg!(feature = "gpu");

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = DemoApp::new(gpu);
    event_loop.run_app(&mut app).expect("run app");
}
