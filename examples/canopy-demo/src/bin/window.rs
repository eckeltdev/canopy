//! The windowed Canopy demo (macOS / Windows / Linux).
//!
//! Opens a winit window, presents the software-rendered frame via softbuffer, and
//! routes left-clicks back into the reactive app through hit-testing. The UI itself
//! is built by [`canopy_demo::build`]; this file is only the platform glue, which is
//! exactly the boundary Canopy is designed around — a real `Platform`/`Renderer`
//! backend swaps in here without touching the UI.
//!
//! Run from `examples/canopy-demo`: `cargo run`

use std::num::NonZeroU32;
use std::rc::Rc;

use canopy_demo::{build, click_handler, run_plugin, Demo, VIEW_H, VIEW_W};
use canopy_dom::Dom;
use canopy_input::Key;
use canopy_layout_taffy::build_scene;
use canopy_protocol::EventPayload;
use canopy_render_soft::SoftwareRenderer;
use canopy_traits::{Color, OpSink, Point, Rect, Renderer, Size};
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
    /// The untrusted wasm plugin, run once at startup; its `dom()` is composited
    /// into a panel each frame. `None` if the sandbox couldn't load it.
    plugin: Option<PluginHost>,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
}

impl DemoApp {
    fn new() -> Self {
        let demo = build();
        let mut dom = Dom::new();
        // Mount the initial tree.
        dom.apply(&demo.app.take_batch(0)).expect("initial batch");
        Self {
            demo,
            dom,
            seq: 1,
            cursor: PhysicalPosition::new(0.0, 0.0),
            plugin: run_plugin(),
            window: None,
            surface: None,
        }
    }

    /// Type a key into the focused text field, then re-apply and redraw.
    fn on_key(&mut self, key: Key) {
        self.demo.app.type_into(self.demo.input, key);
        let batch = self.demo.app.take_batch(self.seq);
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

        // Render the current tree at the window's physical size.
        let mut renderer = SoftwareRenderer::new(w, h, CLEAR);
        renderer
            .render(&build_scene(&self.dom, viewport))
            .expect("render");

        // Composite the untrusted plugin into a bordered panel on the right.
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
        let buf = renderer.buffer_mut();
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
        let rgba = renderer.buffer().data();
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
            self.demo.app.dispatch(handler, EventPayload::None);
            let batch = self.demo.app.take_batch(self.seq);
            self.seq += 1;
            let _ = self.dom.apply(&batch);
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
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
            WindowEvent::CursorMoved { position, .. } => self.cursor = position,
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
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = DemoApp::new();
    event_loop.run_app(&mut app).expect("run app");
}
