//! Canopy GPU renderer: a [`wgpu`]-backed [`canopy_traits::Renderer`] that
//! rasterizes a [`DisplayList`] into an offscreen texture and reads it back to
//! RGBA8.
//!
//! This is the **Tier-0** path — the same `Renderer` seam the software backend
//! ([`canopy-render-soft`](https://docs.rs/canopy-render-soft)) implements, but
//! running on the GPU (Metal on macOS, Vulkan/DX12/GL elsewhere). It renders
//! headlessly: no window or surface is needed, so the whole thing is
//! unit-testable on a CI box with an adapter. The windowed integration that swaps
//! the offscreen target for a swapchain is a thin follow-up.
//!
//! ## How it draws
//! Everything reduces to **colored quads** in pixel space, drawn by one instanced
//! pipeline (see `quad.wgsl`):
//! - [`DisplayItem::Rect`] becomes a single instance.
//! - [`DisplayItem::Text`] is expanded on the CPU into one instance per "ink"
//!   pixel of the baked 8x8 font ([`canopy_text_baked`]), scaled by
//!   `max(1, size / 8)` — exactly matching the software renderer's blit, so the
//!   two backends produce the same glyphs.
//! - [`DisplayItem::Glyphs`] (the shaped, capable-tier path) is not rasterized
//!   here yet, matching the software backend.
//!
//! Keeping text as quads means the crate needs no font rasterizer or Parley
//! dependency: it stays self-contained.
//!
//! ## Color
//! Canopy colors are straight-alpha sRGB bytes. The offscreen target is
//! `Rgba8UnormSrgb`, so the shader receives linearized colors and the readback is
//! sRGB again — i.e. byte-for-byte round-trips for opaque fills. Alpha blending is
//! straight `src.a` over the existing target.

use bytemuck::{Pod, Zeroable};
use canopy_text_baked::{glyph, CELL_H, CELL_W};
use canopy_traits::{Color, DisplayItem, DisplayList, HostError, Point, Rect, Renderer, Size};
use wgpu::util::DeviceExt;

/// One instanced quad: a pixel-space rectangle with a straight-alpha RGBA color.
///
/// `#[repr(C)]` + `Pod` so it can be uploaded straight into a vertex buffer.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct QuadInstance {
    /// Top-left origin in pixels.
    origin: [f32; 2],
    /// Size in pixels.
    size: [f32; 2],
    /// Straight-alpha RGBA in `[0, 1]`.
    color: [f32; 4],
}

/// The viewport uniform: surface size in pixels, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Viewport {
    size: [f32; 2],
    _pad: [f32; 2],
}

impl QuadInstance {
    /// A solid rectangle. Negative origins / sizes are tolerated — the GPU clips
    /// to the viewport, mirroring the software renderer's saturating clip.
    fn rect(rect: Rect, color: Color) -> Self {
        Self {
            origin: [rect.origin.x, rect.origin.y],
            size: [rect.size.w, rect.size.h],
            color: color_to_linear_rgba(color),
        }
    }
}

/// Straight-alpha sRGB bytes -> straight-alpha **linear** float RGBA.
///
/// The target is `Rgba8UnormSrgb`: wgpu linearizes on the way in and re-encodes
/// sRGB on the way out, so a value written here and read back is the original
/// byte (for opaque pixels). We linearize the color components but leave alpha
/// linear, matching the sRGB convention (alpha is never gamma-encoded).
fn color_to_linear_rgba(c: Color) -> [f32; 4] {
    [
        srgb_to_linear(c.r),
        srgb_to_linear(c.g),
        srgb_to_linear(c.b),
        c.a as f32 / 255.0,
    ]
}

/// sRGB byte -> linear float, the standard IEC 61966-2-1 transfer.
fn srgb_to_linear(byte: u8) -> f32 {
    let s = byte as f32 / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// Lower a [`DisplayList`] into a flat list of colored-quad instances in pixel
/// space, back-to-front. [`DisplayItem::Text`] is expanded into one quad per ink
/// pixel of the baked font; [`DisplayItem::Glyphs`] is skipped (not yet
/// rasterized on any tier).
fn lower(scene: &DisplayList) -> Vec<QuadInstance> {
    let mut quads = Vec::new();
    for item in &scene.items {
        match item {
            DisplayItem::Rect { rect, color } => {
                quads.push(QuadInstance::rect(*rect, *color));
            }
            DisplayItem::Text {
                origin,
                text,
                color,
                size,
            } => push_text_quads(&mut quads, *origin, text, *color, *size),
            DisplayItem::Glyphs { .. } => {}
        }
    }
    quads
}

/// Expand a baked-font text run into one quad per ink pixel.
///
/// Mirrors `canopy_render_soft::Buffer::blit_text`: `scale = max(1, size / 8)`,
/// one cell advance of `8 * scale` per character, and each set bit (0x80 =
/// leftmost) drawn as a `scale`x`scale` block.
fn push_text_quads(
    out: &mut Vec<QuadInstance>,
    origin: Point,
    text: &str,
    color: Color,
    size: f32,
) {
    let scale = ((size / CELL_H as f32) as i32).max(1) as f32;
    let advance = CELL_W as f32 * scale;
    for (col, ch) in text.chars().enumerate() {
        let bitmap = glyph(ch);
        let cell_x = origin.x + col as f32 * advance;
        for (row, bits) in bitmap.iter().enumerate() {
            for bit in 0..CELL_W as usize {
                if bits & (0x80 >> bit) != 0 {
                    out.push(QuadInstance {
                        origin: [cell_x + bit as f32 * scale, origin.y + row as f32 * scale],
                        size: [scale, scale],
                        color: color_to_linear_rgba(color),
                    });
                }
            }
        }
    }
}

/// The texture format of the offscreen render target and the readback. sRGB so
/// straight-alpha byte colors round-trip.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// A GPU device + pipeline that renders colored quads into an offscreen texture.
///
/// Construct once with [`GpuRenderer::new`]; it owns the `wgpu` device/queue and
/// the render pipeline. The offscreen texture is (re)allocated to match the
/// requested size on [`render`](Renderer::render) / [`resize`](Renderer::resize).
pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    viewport_layout: wgpu::BindGroupLayout,
    width: u32,
    height: u32,
    clear: Color,
    /// The most recent frame's pixels (RGBA8, row-major), produced by `render`.
    last_frame: Vec<u8>,
}

/// Initialize a `wgpu` instance/adapter/device, blocking on the async init.
///
/// Returns `None` if no GPU adapter is available (e.g. a headless CI box with no
/// software fallback), so callers can fall back to the software renderer instead
/// of panicking.
fn init_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    pollster::block_on(async {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("canopy-render-vello device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                trace: wgpu::Trace::Off,
            })
            .await
            .ok()?;
        Some((device, queue))
    })
}

impl GpuRenderer {
    /// Create a renderer for a `width`x`height` offscreen target with a `clear`
    /// background.
    ///
    /// Returns `None` if no GPU adapter could be acquired. On macOS this requests
    /// the Metal backend; the device is headless (no surface).
    pub fn new(width: u32, height: u32, clear: Color) -> Option<Self> {
        let (device, queue) = init_device()?;
        Some(Self::from_device(device, queue, width, height, clear))
    }

    /// Build the pipeline on an already-initialized device/queue.
    fn from_device(
        device: wgpu::Device,
        queue: wgpu::Queue,
        width: u32,
        height: u32,
        clear: Color,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canopy quad shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("quad.wgsl").into()),
        });

        let viewport_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("canopy viewport bind group layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("canopy pipeline layout"),
            bind_group_layouts: &[Some(&viewport_layout)],
            immediate_size: 0,
        });

        // One instance buffer; each QuadInstance is origin(vec2) + size(vec2) +
        // color(vec4), stepped per-instance.
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<QuadInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: 8,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("canopy quad pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[instance_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TARGET_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            device,
            queue,
            pipeline,
            viewport_layout,
            width: width.max(1),
            height: height.max(1),
            clear,
            last_frame: Vec::new(),
        }
    }

    /// Render `scene` into a fresh offscreen texture and copy it back to RGBA8
    /// (row-major). Always allocates the target at the current size, so it is safe
    /// to call after a `resize`.
    fn render_frame(&self, scene: &DisplayList) -> Vec<u8> {
        let quads = lower(scene);
        draw_quads(
            &self.device,
            &self.queue,
            &self.pipeline,
            &self.viewport_layout,
            self.width,
            self.height,
            self.clear,
            &quads,
        )
    }
}

/// The whole offscreen draw: allocate target + readback buffers, encode one
/// render pass, copy the texture to the readback buffer, map it, and unswizzle
/// the row-padded copy into a tight RGBA8 `Vec`.
///
/// Free function (not a method) so the test can exercise it directly, and so the
/// borrow of device/queue/pipeline stays explicit.
#[allow(clippy::too_many_arguments)]
fn draw_quads(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::RenderPipeline,
    viewport_layout: &wgpu::BindGroupLayout,
    width: u32,
    height: u32,
    clear: Color,
    quads: &[QuadInstance],
) -> Vec<u8> {
    let width = width.max(1);
    let height = height.max(1);

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("canopy offscreen target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    // Viewport uniform.
    let viewport = Viewport {
        size: [width as f32, height as f32],
        _pad: [0.0, 0.0],
    };
    let viewport_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("canopy viewport uniform"),
        contents: bytemuck::bytes_of(&viewport),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("canopy viewport bind group"),
        layout: viewport_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: viewport_buf.as_entire_binding(),
        }],
    });

    // Instance buffer (may be empty — then we just clear). An empty slice cannot
    // be uploaded, so an empty list gets one zero placeholder we then never draw,
    // keeping the buffer valid.
    let placeholder = [QuadInstance::zeroed()];
    let instance_bytes: &[u8] = if quads.is_empty() {
        bytemuck::cast_slice(&placeholder)
    } else {
        bytemuck::cast_slice(quads)
    };
    let instance_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("canopy instance buffer"),
        contents: instance_bytes,
        usage: wgpu::BufferUsages::VERTEX,
    });

    // Readback buffer. wgpu requires each row's byte stride to be a multiple of
    // 256, so we pad and strip the padding after mapping.
    let unpadded_bpr = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bpr = unpadded_bpr.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("canopy readback buffer"),
        size: (padded_bpr * height) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("canopy"),
    });
    {
        let clear_linear = color_to_linear_rgba(clear);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("canopy quad pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: clear_linear[0] as f64,
                        g: clear_linear[1] as f64,
                        b: clear_linear[2] as f64,
                        a: clear_linear[3] as f64,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if !quads.is_empty() {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_vertex_buffer(0, instance_buf.slice(..));
            // 6 vertices (two triangles) per instance.
            pass.draw(0..6, 0..quads.len() as u32);
        }
    }

    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );

    queue.submit(std::iter::once(encoder.finish()));

    // Map the readback buffer and block until it is ready.
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    // `poll(Wait)` drives the queue until the map callback fires.
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv()
        .expect("map_async callback dropped")
        .expect("buffer map failed");

    let mapped = slice.get_mapped_range();
    let mut out = vec![0u8; (unpadded_bpr * height) as usize];
    for row in 0..height as usize {
        let src = row * padded_bpr as usize;
        let dst = row * unpadded_bpr as usize;
        out[dst..dst + unpadded_bpr as usize]
            .copy_from_slice(&mapped[src..src + unpadded_bpr as usize]);
    }
    drop(mapped);
    readback.unmap();
    out
}

/// One-shot offscreen render: rasterize `scene` at `size` over a `clear`
/// background and return the RGBA8 pixels (row-major, 4 bytes per pixel).
///
/// This spins up a fresh `wgpu` device, so it is convenient for tests and tools
/// but heavier than reusing a [`GpuRenderer`]. **Panics** if no GPU adapter is
/// available — use [`try_render_to_rgba`] to handle that case gracefully.
pub fn render_to_rgba(scene: &DisplayList, size: Size, clear: Color) -> Vec<u8> {
    try_render_to_rgba(scene, size, clear).expect("no GPU adapter available")
}

/// Fallible one-shot offscreen render. Returns `None` if no GPU adapter could be
/// acquired; otherwise the RGBA8 pixels (row-major).
pub fn try_render_to_rgba(scene: &DisplayList, size: Size, clear: Color) -> Option<Vec<u8>> {
    let r = GpuRenderer::new(size.w as u32, size.h as u32, clear)?;
    Some(r.render_frame(scene))
}

impl Renderer for GpuRenderer {
    fn resize(&mut self, size: Size) {
        self.width = (size.w as u32).max(1);
        self.height = (size.h as u32).max(1);
    }

    /// Render `scene` offscreen and stash the result for [`present`](Renderer::present)
    /// / [`last_frame`](GpuRenderer::last_frame). Never fails once the device is up.
    fn render(&mut self, scene: &DisplayList) -> Result<(), HostError> {
        self.last_frame = self.render_frame(scene);
        Ok(())
    }

    /// No-op on the headless path: the frame already lives in CPU memory after
    /// `render` (this is the readback). A windowed integration overrides this with
    /// a swapchain present.
    fn present(&mut self) -> Result<(), HostError> {
        Ok(())
    }
}

impl GpuRenderer {
    /// The most recently rendered frame as RGBA8 (row-major). Empty until the
    /// first [`render`](Renderer::render).
    pub fn last_frame(&self) -> &[u8] {
        &self.last_frame
    }

    /// Convenience: read one pixel of the last frame as `[r, g, b, a]`. Returns
    /// `None` if out of bounds or before the first render.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        self.last_frame
            .get(i..i + 4)
            .map(|p| [p[0], p[1], p[2], p[3]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_traits::Point;

    /// Build a one-red-rect-on-black scene at the given size.
    fn red_rect_scene(w: f32, h: f32) -> DisplayList {
        DisplayList {
            items: vec![DisplayItem::Rect {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size { w, h },
                },
                color: Color {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                },
            }],
        }
    }

    #[test]
    fn lowering_expands_text_to_ink_quads() {
        // 'A' at scale 1 (size 8) has a known number of ink bits; lowering must
        // emit exactly that many quads and skip the surrounding background.
        let ink_bits: usize = glyph('A').iter().map(|b| b.count_ones() as usize).sum();
        let scene = DisplayList {
            items: vec![DisplayItem::Text {
                origin: Point { x: 3.0, y: 5.0 },
                text: "A".into(),
                color: Color {
                    r: 1,
                    g: 2,
                    b: 3,
                    a: 255,
                },
                size: 8.0,
            }],
        };
        let quads = lower(&scene);
        assert_eq!(quads.len(), ink_bits, "one quad per ink pixel");
        // Every quad is a 1x1 block (scale 1) at integer offsets from the origin.
        assert!(quads.iter().all(|q| q.size == [1.0, 1.0]));
    }

    #[test]
    fn srgb_encode_round_trips_primaries() {
        // The sRGB transfer must map 0/255 to themselves and stay monotonic.
        assert_eq!(srgb_to_linear(0), 0.0);
        assert!((srgb_to_linear(255) - 1.0).abs() < 1e-6);
        assert!(srgb_to_linear(128) > srgb_to_linear(64));
    }

    /// THE GPU TEST: render a red rect on black on the real adapter and assert the
    /// center pixel is red. Skips (passes) only if no adapter exists at all.
    #[test]
    fn red_rect_center_is_red_on_gpu() {
        let size = Size { w: 64.0, h: 48.0 };
        let clear = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let Some(px) = try_render_to_rgba(&red_rect_scene(size.w, size.h), size, clear) else {
            eprintln!("no GPU adapter; skipping GPU assertion");
            return;
        };

        let w = size.w as usize;
        let h = size.h as usize;
        assert_eq!(px.len(), w * h * 4);

        // Center pixel.
        let (cx, cy) = (w / 2, h / 2);
        let i = (cy * w + cx) * 4;
        let center = [px[i], px[i + 1], px[i + 2], px[i + 3]];
        assert_eq!(center, [255, 0, 0, 255], "center pixel must be opaque red");

        // The rect covers the whole surface, so a corner is red too.
        assert_eq!(&px[0..4], &[255, 0, 0, 255], "top-left also red");
    }

    /// A partial rect: only the covered region is red; the rest stays the clear
    /// color. Proves coordinates and the y-flip are correct, not just a full clear.
    #[test]
    fn partial_rect_respects_bounds_on_gpu() {
        let size = Size { w: 40.0, h: 40.0 };
        let clear = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let scene = DisplayList {
            items: vec![DisplayItem::Rect {
                // Top-left 10x10 block.
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size { w: 10.0, h: 10.0 },
                },
                color: Color {
                    r: 0,
                    g: 255,
                    b: 0,
                    a: 255,
                },
            }],
        };
        let Some(px) = try_render_to_rgba(&scene, size, clear) else {
            eprintln!("no GPU adapter; skipping GPU assertion");
            return;
        };
        let w = size.w as usize;
        let at = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        // Inside the block (near the top-left) is green.
        assert_eq!(at(2, 2), [0, 255, 0, 255], "inside block is green");
        // Outside the block is the black clear.
        assert_eq!(at(30, 30), [0, 0, 0, 255], "outside block is clear");
    }

    /// Text renders ink pixels matching the baked font through the full GPU path.
    #[test]
    fn text_ink_paints_on_gpu() {
        let size = Size { w: 32.0, h: 16.0 };
        let bg = Color {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 255,
        };
        let ink = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        let scene = DisplayList {
            items: vec![DisplayItem::Text {
                origin: Point { x: 0.0, y: 0.0 },
                text: "A".into(),
                color: ink,
                size: 8.0,
            }],
        };
        let Some(px) = try_render_to_rgba(&scene, size, bg) else {
            eprintln!("no GPU adapter; skipping GPU assertion");
            return;
        };
        let w = size.w as usize;
        let at = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        // 'A' row 0 = 0x38 -> columns 2,3,4 ink; column 0 clear (background).
        assert_eq!(at(2, 0), [255, 255, 255, 255], "ink pixel of 'A' apex");
        assert_eq!(
            at(0, 0),
            [0x10, 0x20, 0x30, 255],
            "off-glyph keeps background"
        );
    }

    /// `GpuRenderer` used through the trait stashes the frame for readback.
    #[test]
    fn renderer_trait_roundtrip_on_gpu() {
        let clear = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let Some(mut r) = GpuRenderer::new(20, 20, clear) else {
            eprintln!("no GPU adapter; skipping GPU assertion");
            return;
        };
        let scene = red_rect_scene(20.0, 20.0);
        r.render(&scene).unwrap();
        r.present().unwrap();
        assert_eq!(r.last_frame().len(), 20 * 20 * 4);
        assert_eq!(r.pixel(10, 10), Some([255, 0, 0, 255]));
        assert_eq!(r.pixel(99, 99), None, "out of bounds");
    }
}
