//! The **interactive** capable-tier (**Stylo**) host (macOS / Windows / Linux).
//!
//! Opens a winit window and presents the authored capable Canopy tree — cascaded by the
//! real Servo-Stylo engine, laid out with Taffy, painted on the CPU via softbuffer. The
//! headline is **live `:hover`**: move the cursor over the "Theme: dark" row and its
//! background flips (`.row:hover`), because the pointer drives [`CapableApp::on_pointer_move`]
//! → `hit_test` → `set_hover` → re-cascade → repaint. That is the capable host running
//! live, not a still — the same Dom → Stylo → `DisplayList` → renderer loop the headless
//! `main.rs` proves, now fed by input.
//!
//! Like the sibling Stylo window this scene is static (no animation timeline), so it only
//! redraws on demand: the first frame, a resize (it reflows to the new size), and each
//! hover transition. There is no busy render loop.
//!
//! Run from `examples/full/capable-tier`: `cargo +nightly run --features window --bin window`.

use std::num::NonZeroU32;
use std::rc::Rc;

use canopy_full_capable_tier::{CapableApp, VIEW_H, VIEW_W};
use canopy_traits::{Point, Size};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

struct CapableWindow {
    /// The persistent capable host: Stylo engine + authored Dom + software renderer +
    /// hover state. Painted on demand and fed pointer moves for live `:hover`.
    app: CapableApp,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
}

impl CapableWindow {
    fn new() -> Self {
        Self {
            app: CapableApp::new(),
            window: None,
            surface: None,
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
                w: VIEW_W as f32,
                h: VIEW_H as f32,
            },
        }
    }

    fn redraw(&mut self) {
        let viewport = self.viewport();
        let (w, h) = (viewport.w as usize, viewport.h as usize);

        // Re-cascade + repaint at the current window size (the engine resolves styles +
        // runs Taffy, then the software renderer rasterizes the display list).
        self.app.paint(viewport);

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
        let rgba = self.app.buffer().data();
        for (dst, src) in frame.iter_mut().zip(rgba.chunks_exact(4)) {
            *dst = ((src[0] as u32) << 16) | ((src[1] as u32) << 8) | (src[2] as u32);
        }
        window.pre_present_notify();
        frame.present().expect("present");
    }
}

impl ApplicationHandler for CapableWindow {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Canopy (capable tier — live :hover)")
            .with_inner_size(LogicalSize::new(VIEW_W as f64, VIEW_H as f64));
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface =
            softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.window = Some(window);
        self.surface = Some(surface);
        // Static scene: redraw on demand only (no animation loop).
        event_loop.set_control_flow(ControlFlow::Wait);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            // Reflow to the new size.
            WindowEvent::Resized(_) => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            // Pointer movement drives `:hover`: map the cursor to a canopy Point and ask
            // the app to hit-test + toggle hover. It returns whether the hovered element
            // changed; only then do we request a redraw, so a `:hover` rule repaints live
            // without a busy loop.
            WindowEvent::CursorMoved { position, .. } => {
                let viewport = self.viewport();
                let point = Point {
                    x: position.x as f32,
                    y: position.y as f32,
                };
                if self.app.on_pointer_move(point, viewport) {
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("event loop");
    let mut app = CapableWindow::new();
    event_loop.run_app(&mut app).expect("run app");
}
