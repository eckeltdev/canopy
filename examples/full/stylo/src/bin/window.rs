//! The windowed full-tier (**Stylo**) demo (macOS / Windows / Linux).
//!
//! Opens a winit window and presents the Stylo-styled UI via softbuffer. The headline
//! feature is **live CSS hot-reload**: edit `styles.css`, press any key (or click) in the
//! window, and the whole tree is re-cascaded through Servo's Stylo and redrawn — so you
//! can watch real inheritance / specificity / descendant-combinator behavior update live.
//!
//! Unlike the landing examples this scene is static (no animation timeline), so it only
//! redraws on demand: the first frame, a resize (it reflows to the new width), and each
//! hot-reload. There is no busy render loop.
//!
//! Run from `examples/full/stylo`: `cargo run`.

use std::num::NonZeroU32;
use std::rc::Rc;

use canopy_full_stylo::{build, load_styles, Scene, VIEW_H, VIEW_W};
use canopy_traits::Size;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

struct StyloApp {
    /// The built scene (Stylo engine + item tree). Rebuilt on each CSS hot-reload.
    scene: Scene,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
}

impl StyloApp {
    fn new() -> Self {
        Self {
            scene: build(&load_styles()),
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

        // Re-lay-out + repaint the (already-cascaded) scene at the current window size.
        let buf = self.scene.render(w, h);

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

impl ApplicationHandler for StyloApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Canopy (Stylo)")
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
            // Press any key / click to hot-reload styles.css: rebuild the scene with the
            // edited CSS (re-running the full Stylo cascade) and redraw.
            WindowEvent::KeyboardInput { .. } | WindowEvent::MouseInput { .. } => {
                self.scene = build(&load_styles());
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
    let mut app = StyloApp::new();
    event_loop.run_app(&mut app).expect("run app");
}
