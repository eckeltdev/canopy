//! The windowed Canopy **landing** (macOS / Windows / Linux).
//!
//! Opens a winit window, presents the rendered frame via softbuffer, and runs the
//! animation: a `canopy_anim::Timeline` is `tick`ed each frame, its tweens write the
//! `opacity`/`translate-y`/`width` signals the UI is bound to, and the frame redraws.
//! Because the ambient dot pulses loop forever the timeline never goes idle, so we pace
//! redraws with `ControlFlow::WaitUntil` (~60 fps) rather than busy-spinning.
//!
//! It also **hot-reloads `styles.css`**: edit the stylesheet, save, and the live window
//! restyles (the watcher pattern from the welcome example).
//!
//! Run from `examples/canopy-landing`: `cargo run`.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use canopy_landing::{build, load_styles, Landing, VIEW_H, VIEW_W};
use canopy_text_parley::TextEngine;
use canopy_traits::{Color, OpSink, Size};
use canopy_ui::prelude::Dom;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// The near-black canvas color, matching `.stage` so there is no seam.
const CLEAR: Color = Color {
    r: 0x06,
    g: 0x06,
    b: 0x08,
    a: 255,
};

/// ~60 fps frame budget for the animation loop.
const FRAME: Duration = Duration::from_millis(16);

struct LandingApp {
    landing: Landing,
    dom: Dom,
    seq: u32,
    /// Timestamp of the last animation tick (to measure `dt`).
    last_frame: Option<Instant>,
    /// Held across redraws so the bundled font loads once.
    text: TextEngine,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
}

impl LandingApp {
    fn new() -> Self {
        let landing = build();
        let mut dom = Dom::new();
        dom.apply(&landing.ui.take_batch(0)).expect("initial batch");
        Self {
            landing,
            dom,
            seq: 1,
            last_frame: None,
            text: TextEngine::new(),
            window: None,
            surface: None,
        }
    }

    fn apply_and_redraw(&mut self) {
        let batch = self.landing.ui.take_batch(self.seq);
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
            *dst = ((src[0] as u32) << 16) | ((src[1] as u32) << 8) | (src[2] as u32);
        }
        window.pre_present_notify();
        frame.present().expect("present");
    }
}

impl ApplicationHandler for LandingApp {
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
        // Start the animation clock.
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + FRAME));
    }

    /// The animation loop: advance the timeline at ~60 fps and redraw. The ambient dots
    /// loop forever, so this keeps pacing frames via `WaitUntil` (sleeping between, not
    /// busy-spinning).
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        match self.last_frame {
            Some(last) if now.duration_since(last) >= FRAME => {
                self.last_frame = Some(now);
                self.landing.timeline.tick(now.duration_since(last).as_secs_f32());
                self.landing.ui.runtime().flush();
                self.apply_and_redraw();
            }
            None => self.last_frame = Some(now),
            _ => {}
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(now + FRAME));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            // Press any key / click to hot-reload styles.css from disk.
            WindowEvent::KeyboardInput { .. } | WindowEvent::MouseInput { .. } => {
                self.landing.ui.reload_css(&load_styles(), None);
                self.apply_and_redraw();
            }
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("event loop");
    let mut app = LandingApp::new();
    event_loop.run_app(&mut app).expect("run app");
}
