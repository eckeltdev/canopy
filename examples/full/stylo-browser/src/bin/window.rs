//! The windowed full-tier (**Stylo**) browser (macOS / Windows / Linux).
//!
//! Opens a winit window and presents an *arbitrary* HTML/CSS page — parsed,
//! cascaded, laid out, and painted entirely by the real Servo-Stylo engine — via
//! softbuffer. The headline feature is **live HTML hot-reload**: edit `page.html`
//! (markup AND embedded CSS together), press any key (or click) in the window, and
//! the whole document is re-parsed, re-cascaded through Stylo, re-laid-out with
//! Taffy, and redrawn.
//!
//! Like its `stylo` sibling this scene is static (no animation timeline), so it only
//! redraws on demand: the first frame, a resize (it reflows to the new width), and
//! each hot-reload. There is no busy render loop.
//!
//! Run from `examples/full/stylo-browser`: `cargo +nightly run`.

use std::num::NonZeroU32;
use std::rc::Rc;

use canopy_full_stylo_browser::{build, load_html, render_to_buffer, VIEW_H, VIEW_W};
use canopy_style_stylo::StyloEngine;
use canopy_traits::{Point, Size};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

struct BrowserApp {
    /// The Stylo engine for the current `page.html`. Rebuilt on each hot-reload.
    engine: StyloEngine,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    /// Arena slab of the element the pointer is currently over (the `:hover`
    /// target), so a `CursorMoved` only restyles when the hovered element changes.
    hover: Option<usize>,
}

impl BrowserApp {
    fn new() -> Self {
        Self {
            engine: build(&load_html()),
            window: None,
            surface: None,
            hover: None,
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

        // Re-lay-out + repaint the page (the engine resolves styles + runs Taffy)
        // at the current window size.
        let buf = render_to_buffer(&mut self.engine, w, h);

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
            *dst = ((src[0] as u32) << 16) | ((src[1] as u32) << 8) | (src[2] as u32);
        }
        window.pre_present_notify();
        frame.present().expect("present");
    }
}

impl ApplicationHandler for BrowserApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Canopy (Stylo HTML)")
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
            // Pointer movement drives `:hover`: map the cursor to a canopy Point,
            // hit-test the deepest element under it, and — when that element
            // changes — move the HOVER state to it (which forces a restyle) and
            // redraw, so a `:hover` rule visibly repaints live.
            WindowEvent::CursorMoved { position, .. } => {
                let viewport = self.viewport();
                let point = Point {
                    x: position.x as f32,
                    y: position.y as f32,
                };
                let hit = self.engine.hit_test(point, viewport);
                if hit != self.hover {
                    self.hover = hit;
                    self.engine.set_hover(hit);
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            // Press any key / click to hot-reload page.html: re-parse the whole
            // document (markup + CSS), re-run the full Stylo cascade, and redraw.
            WindowEvent::KeyboardInput { .. } | WindowEvent::MouseInput { .. } => {
                self.engine = build(&load_html());
                // The rebuilt engine starts with no hover state.
                self.hover = None;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("event loop");
    let mut app = BrowserApp::new();
    event_loop.run_app(&mut app).expect("run app");
}
