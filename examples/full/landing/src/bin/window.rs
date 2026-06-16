//! The windowed Canopy landing, **GPU-rasterized** (macOS / Windows / Linux) — the full
//! path.
//!
//! This is the GPU twin of `canopy-lite-landing`'s window: same winit window, same
//! `canopy_anim::Timeline` ticked each frame at ~60 fps via `ControlFlow::WaitUntil`,
//! same hot-reload-on-keypress, same shared UI ([`canopy_landing_ui`]). The *only*
//! difference is the per-frame pipeline:
//!
//! - The CPU example calls `canopy_render_text::render_dom_with`, which lays out and
//!   software-rasterizes in one shot.
//! - Here we [`build_scene`](canopy_layout_taffy::build_scene) for the current viewport
//!   and rasterize it on the **GPU** with [`canopy_render_vello::GpuRenderer`] (Metal on
//!   this Mac), then read the frame back and blit it into the softbuffer surface.
//!
//! This is **GPU rasterization + a CPU present-blit**: render-vello renders into an
//! offscreen texture and reads it back to RGBA8 ([`GpuRenderer::last_frame`]), which we
//! copy into softbuffer to present. That readback is the path render-vello supports
//! today; a zero-copy `wgpu`-swapchain present (render straight into the window's
//! surface, no readback, no softbuffer) would be the next optimization, but render-vello
//! does not expose a swapchain target yet, so we use the readback path.
//!
//! The `GpuRenderer` is created once at the window size and **reused** across frames via
//! the [`Renderer`](canopy_traits::Renderer) trait (`render` + `last_frame`); because it
//! is sized at construction we recreate it on resize.
//!
//! Run from `examples/full/landing`: `cargo run`.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use canopy_landing_ui::{build, load_styles, Landing, VIEW_H, VIEW_W};
use canopy_layout_taffy::build_scene;
use canopy_render_vello::GpuRenderer;
use canopy_traits::{Color, OpSink, Renderer, Size};
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
    /// The GPU renderer, created at the window size and reused frame-to-frame. It is
    /// sized at construction, so a resize replaces it (see `gpu_for`). `None` until the
    /// window exists or if no GPU adapter could be acquired.
    gpu: Option<GpuRenderer>,
    /// The pixel size `gpu` was built for, so a resize can detect the change and
    /// recreate it (the renderer is sized at construction and exposes no size getter).
    gpu_size: (u32, u32),
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
            gpu: None,
            gpu_size: (0, 0),
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

    /// Borrow a `GpuRenderer` sized for `viewport`, creating it on first use and
    /// recreating it whenever the size changes (the renderer is sized at construction).
    /// Returns `None` if no GPU adapter is available — the window then presents an empty
    /// (cleared) surface rather than crashing.
    fn gpu_for(&mut self, viewport: Size) -> Option<&mut GpuRenderer> {
        let size = (viewport.w as u32, viewport.h as u32);
        if self.gpu.is_none() || self.gpu_size != size {
            self.gpu = GpuRenderer::new(size.0, size.1, CLEAR);
            self.gpu_size = size;
        }
        self.gpu.as_mut()
    }

    fn redraw(&mut self) {
        let viewport = self.viewport();
        let (w, h) = (viewport.w as usize, viewport.h as usize);

        // Lay the current DOM out into a display list, then rasterize it on the GPU
        // (reusing one renderer; recreated on a size change inside `gpu_for`).
        let scene = build_scene(&self.dom, viewport);
        let rgba: Vec<u8> = match self.gpu_for(viewport) {
            Some(gpu) => {
                // The headless `Renderer`: render offscreen, then read the frame back.
                gpu.render(&scene).expect("gpu render");
                gpu.last_frame().to_vec()
            }
            // No adapter: present a cleared surface so the window still opens.
            None => vec![0u8; w * h * 4],
        };

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
        // Blit the GPU readback (RGBA8) into softbuffer's `0x00RRGGBB` words. If the
        // readback is shorter than the surface (a stale size mid-resize), the zip
        // stops early and the remainder keeps its previous content for one frame.
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
            .with_title("Canopy (GPU)")
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
                self.landing
                    .timeline
                    .tick(now.duration_since(last).as_secs_f32());
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
            // The `.stage` is `100% x 100%`, so a resize just relayouts at the new
            // viewport — redraw at the new size (the animation loop redraws too, but
            // this makes the resize feel immediate). `gpu_for` rebuilds the GPU
            // renderer at the new size on the next redraw.
            WindowEvent::Resized(_) => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
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
