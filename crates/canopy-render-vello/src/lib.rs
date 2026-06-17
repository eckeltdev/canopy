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
//! Two alpha-blended pipelines share one render pass, drawn in display-list
//! order so back-to-front compositing is correct:
//! - [`DisplayItem::Rect`] becomes a single **colored-quad** instance
//!   (`quad.wgsl`). A positive `radius` rounds the corners on the GPU via a
//!   rounded-rectangle signed-distance function evaluated per fragment, with a
//!   ~1px antialiased edge — matching (and slightly improving on, since the CPU
//!   tiers have hard edges) the software `fill_round_rect`. `radius == 0` keeps
//!   the exact square fill.
//! - [`DisplayItem::Text`] is rasterized to a **real antialiased coverage mask**
//!   by [`canopy_text_parley::TextEngine`], uploaded as an `R8Unorm` texture, and
//!   drawn as one **textured glyph quad** (`glyph.wgsl`) tinted with the ink
//!   color. This gives the GPU the same sharp, antialiased text the CPU
//!   "sharp-text" path produces — partial-coverage edge pixels and all — which
//!   the old baked 8×8 expansion could never do.
//! - [`DisplayItem::Glyphs`] (the pre-shaped path) is not rasterized here yet;
//!   the demos emit `Text`, which is what we render.
//!
//! ### Why coverage-textured quads (approach A) over per-pixel quads (B)
//! We rasterize each run **once** into a coverage texture and draw it with a
//! single quad, rather than emitting one alpha-blended quad per non-zero
//! coverage pixel. Approach A is the cleaner, far cheaper path: a 200-px line of
//! text is one draw call and one small texture instead of thousands of
//! per-frame instances, and the GPU's bilinear sampler does the compositing.
//! The cost is the extra texture/bind-group churn per run and a sampler in the
//! pipeline — both negligible next to per-pixel instancing. Glyph placement is
//! top-aligned to the run's `origin` (the coverage mask is the tight ink box;
//! see [`GlyphRun`]).
//!
//! ## Color
//! Canopy colors are straight-alpha sRGB bytes. The offscreen target is
//! `Rgba8UnormSrgb`, so the shader receives linearized colors and the readback is
//! sRGB again — i.e. byte-for-byte round-trips for opaque fills. Alpha blending is
//! straight `src.a` over the existing target, so the antialiased coverage edges
//! of glyphs composite smoothly onto whatever rect sits behind them.

use bytemuck::{Pod, Zeroable};
use canopy_text_parley::{Glyphs, TextEngine};
use canopy_traits::{Color, DisplayItem, DisplayList, HostError, Point, Rect, Renderer, Size};
use wgpu::util::DeviceExt;

/// One instanced quad: a pixel-space rectangle with a straight-alpha RGBA color
/// and an optional corner radius.
///
/// `#[repr(C)]` + `Pod` so it can be uploaded straight into a vertex buffer.
/// `radius` (plus the already-present `size`) is everything the fragment shader's
/// rounded-rect SDF needs; a trailing pad keeps the struct's size a multiple of
/// its 8-byte alignment (`vec2` fields), which `bytemuck` requires of a `Pod`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct QuadInstance {
    /// Top-left origin in pixels.
    origin: [f32; 2],
    /// Size in pixels.
    size: [f32; 2],
    /// Straight-alpha RGBA in `[0, 1]`.
    color: [f32; 4],
    /// Corner radius in pixels, **already clamped** to half the shorter side
    /// (see [`QuadInstance::rect`]). `0` draws a square.
    radius: f32,
    /// Padding so the struct stays a multiple of its 8-byte alignment. Unread by
    /// the shader (the vertex layout stops at `radius`).
    _pad: f32,
}

/// The viewport uniform: surface size in pixels, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Viewport {
    size: [f32; 2],
    _pad: [f32; 2],
}

/// The per-glyph-run uniform consumed by `glyph.wgsl`: where to place the
/// coverage mask and the ink color to tint it with.
///
/// Laid out to match the WGSL `GlyphRun` struct exactly (`origin`+`size`+`color`
/// = 8 floats = 32 bytes, already 16-byte aligned, so no trailing pad needed).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlyphRunUniform {
    /// Top-left origin of the coverage rect, in pixels.
    origin: [f32; 2],
    /// Size of the coverage rect, in pixels (= the mask's width/height).
    size: [f32; 2],
    /// Straight-alpha RGBA ink color in `[0, 1]`.
    color: [f32; 4],
}

impl QuadInstance {
    /// A solid rectangle with corner `radius` (in pixels; `0` = square). Negative
    /// origins / sizes are tolerated — the GPU clips to the viewport, mirroring
    /// the software renderer's saturating clip.
    ///
    /// `radius` is clamped to half the shorter side here, exactly as the CPU
    /// [`fill_round_rect`](https://docs.rs/canopy-render-soft) does, so an
    /// oversized radius produces a pill/stadium (or a circle for a square rect)
    /// instead of letting opposite corners' arcs overlap. We clamp on the CPU so
    /// the shader can trust the value and stay branch-light. A negative radius
    /// (shouldn't happen — the paint layer emits `>= 0`) clamps to `0`.
    fn rect(rect: Rect, color: Color, radius: f32) -> Self {
        let max_r = 0.5 * rect.size.w.min(rect.size.h);
        let r = radius.clamp(0.0, max_r.max(0.0));
        Self {
            origin: [rect.origin.x, rect.origin.y],
            size: [rect.size.w, rect.size.h],
            color: color_to_linear_rgba(color),
            radius: r,
            _pad: 0.0,
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

/// A rasterized text run ready to upload: its coverage mask plus where to place
/// it (pixel-space top-left) and the ink color to tint it with.
///
/// The mask's `width`/`height` are the **tight ink box** of the run (the
/// rasterizer trims leading/top blank space). We top-left-align that box to the
/// run's `origin`, the same place the baked path started drawing — so a Text run
/// lands inside its layout box rather than floating.
struct GlyphRun {
    /// Top-left placement in pixels.
    origin: Point,
    /// The 8-bit alpha-coverage mask (row-major, `width*height` bytes).
    mask: Glyphs,
    /// Ink color.
    color: Color,
}

/// One thing to draw, in display-list order. Keeping rects and glyph runs in a
/// single ordered list lets us paint them back-to-front in one render pass, so a
/// glyph run drawn after its background rect composites on top of it correctly.
enum DrawCmd {
    /// A solid colored quad (from [`DisplayItem::Rect`]).
    Rect(QuadInstance),
    /// A textured, antialiased glyph run (from [`DisplayItem::Text`]).
    Glyphs(GlyphRun),
}

/// The number of solid slabs a [`DisplayItem::Gradient`] is lowered into on the GPU.
///
/// We approximate a smooth gradient with this many adjacent constant-color quads,
/// each colored at its midpoint along the axis. It is a real, visible ramp (the CPU
/// tiers degrade to a single solid), and at 32 slabs across a typical box the banding
/// is below one slab per few pixels — smooth enough without a dedicated shader.
const GRADIENT_SLABS: u32 = 32;

/// Sample a [`GradientStops`] set at normalized position `t` in `[0, 1]`, linearly
/// interpolating between the two surrounding stops.
///
/// Stops are assumed sorted by `position` (the lowering emits them in axis order). A
/// `t` before the first stop clamps to the first color; after the last, to the last.
/// An empty set yields transparent black. This is the per-slab color the GPU gradient
/// lowering paints.
fn sample_gradient(stops: &canopy_traits::GradientStops, t: f32) -> Color {
    let s = stops.as_slice();
    match s.first() {
        None => Color::default(),
        Some(first) => {
            if t <= first.position {
                return first.color;
            }
            // Walk to the segment containing `t`.
            for pair in s.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if t <= b.position {
                    let span = (b.position - a.position).max(f32::EPSILON);
                    let f = ((t - a.position) / span).clamp(0.0, 1.0);
                    return lerp_color(a.color, b.color, f);
                }
            }
            // Past the last stop.
            s[s.len() - 1].color
        }
    }
}

/// Linearly interpolate two straight-alpha colors channel-wise at fraction `f`.
fn lerp_color(a: Color, b: Color, f: f32) -> Color {
    let mix = |x: u8, y: u8| -> u8 {
        (x as f32 + (y as f32 - x as f32) * f)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color {
        r: mix(a.r, b.r),
        g: mix(a.g, b.g),
        b: mix(a.b, b.b),
        a: mix(a.a, b.a),
    }
}

/// Lower a [`DisplayItem::Gradient`] into [`GRADIENT_SLABS`] adjacent solid quads
/// across `direction`, each colored at its slab midpoint — a real GPU ramp.
fn lower_gradient(
    cmds: &mut Vec<DrawCmd>,
    rect: Rect,
    stops: &canopy_traits::GradientStops,
    direction: canopy_traits::GradientDirection,
) {
    use canopy_traits::GradientDirection;
    let n = GRADIENT_SLABS;
    for i in 0..n {
        // Midpoint of slab `i` along the axis, in [0, 1].
        let t = (i as f32 + 0.5) / n as f32;
        let color = sample_gradient(stops, t);
        let slab = match direction {
            GradientDirection::Vertical => {
                let h = rect.size.h / n as f32;
                Rect {
                    origin: Point {
                        x: rect.origin.x,
                        y: rect.origin.y + i as f32 * h,
                    },
                    size: Size { w: rect.size.w, h },
                }
            }
            GradientDirection::Horizontal => {
                let w = rect.size.w / n as f32;
                Rect {
                    origin: Point {
                        x: rect.origin.x + i as f32 * w,
                        y: rect.origin.y,
                    },
                    size: Size { w, h: rect.size.h },
                }
            }
        };
        // Slabs are square-cornered fills; rounding (if any) belongs to the box's
        // own background/border, not the gradient ramp.
        cmds.push(DrawCmd::Rect(QuadInstance::rect(slab, color, 0.0)));
    }
}

/// Lower a [`DisplayItem::Border`] into four edge-band quads (top, bottom, left,
/// right) drawn through the rect pipeline.
///
/// The bands are stroked *inside* `rect`, `width` px thick; top/bottom span the full
/// width and left/right fill the gap between them, so no corner is double-painted. The
/// corner `radius` is carried onto the corner-touching bands via the SDF so the outer
/// corners round on the GPU (the CPU tiers keep square corners). A non-positive width
/// emits nothing.
fn lower_border(cmds: &mut Vec<DrawCmd>, rect: Rect, color: Color, width: f32, radius: f32) {
    if width <= 0.0 {
        return;
    }
    let w = width.min(0.5 * rect.size.w.min(rect.size.h));
    if w <= 0.0 {
        return;
    }
    let Rect { origin, size } = rect;
    // Top and bottom bands (full width). Carry the radius so the GPU SDF rounds the
    // outer corners of these bands; the inner straight edge stays crisp.
    let top = Rect {
        origin,
        size: Size { w: size.w, h: w },
    };
    let bottom = Rect {
        origin: Point {
            x: origin.x,
            y: origin.y + size.h - w,
        },
        size: Size { w: size.w, h: w },
    };
    let left = Rect {
        origin: Point {
            x: origin.x,
            y: origin.y + w,
        },
        size: Size {
            w,
            h: size.h - 2.0 * w,
        },
    };
    let right = Rect {
        origin: Point {
            x: origin.x + size.w - w,
            y: origin.y + w,
        },
        size: Size {
            w,
            h: size.h - 2.0 * w,
        },
    };
    // The thin bands are narrower than `2*radius`, so the SDF clamps the radius to
    // half each band's shorter side; passing the box radius keeps the rounding intent
    // without overflowing (the QuadInstance clamps it per band).
    cmds.push(DrawCmd::Rect(QuadInstance::rect(top, color, radius)));
    cmds.push(DrawCmd::Rect(QuadInstance::rect(bottom, color, radius)));
    cmds.push(DrawCmd::Rect(QuadInstance::rect(left, color, 0.0)));
    cmds.push(DrawCmd::Rect(QuadInstance::rect(right, color, 0.0)));
}

/// Lower a [`DisplayItem::Shadow`] into a single translucent, offset, rounded quad
/// behind the box — a real (if simplified) GPU drop shadow.
///
/// The shadow box is `rect` translated by `offset`; we round its corners generously
/// (radius ≈ `blur`) and scale the color's alpha down by a blur-dependent factor so a
/// larger blur reads as a softer, fainter shadow. This is a single-quad approximation
/// of a gaussian blur (no separable blur pass), but it is a genuine GPU draw — unlike
/// the CPU tiers, which drop the shadow entirely. A fully-transparent shadow color or
/// a zero-area box emits nothing.
fn lower_shadow(cmds: &mut Vec<DrawCmd>, rect: Rect, color: Color, blur: f32, offset: Point) {
    if color.a == 0 || rect.size.w <= 0.0 || rect.size.h <= 0.0 {
        return;
    }
    let shadow_rect = Rect {
        origin: Point {
            x: rect.origin.x + offset.x,
            y: rect.origin.y + offset.y,
        },
        size: rect.size,
    };
    // Soften: a bigger blur fades the shadow (alpha falls off as the energy spreads).
    // Clamp so even a large blur keeps a faint, visible shadow.
    let softness = 1.0 / (1.0 + (blur * 0.15).max(0.0));
    let a = (color.a as f32 * softness).round().clamp(0.0, 255.0) as u8;
    if a == 0 {
        return;
    }
    let soft_color = Color { a, ..color };
    // Round the corners by roughly the blur radius so the shadow has soft-looking
    // rounded edges (the SDF clamps an oversized radius to half the shorter side).
    let radius = blur.max(0.0);
    cmds.push(DrawCmd::Rect(QuadInstance::rect(
        shadow_rect,
        soft_color,
        radius,
    )));
}

/// Lower a [`DisplayList`] into an ordered list of draw commands.
///
/// [`DisplayItem::Rect`] becomes a colored quad; [`DisplayItem::Text`] is
/// rasterized to a real antialiased coverage mask via `engine` and becomes a
/// textured glyph run; [`DisplayItem::Border`], [`DisplayItem::Gradient`], and
/// [`DisplayItem::Shadow`] lower to real colored quads on the rect pipeline (an
/// edge-band frame, an interpolated slab ramp, and a softened offset quad
/// respectively); [`DisplayItem::Glyphs`] is skipped (the pre-shaped path is not
/// rasterized on any tier yet). Order is preserved for correct compositing.
fn lower(scene: &DisplayList, engine: &mut TextEngine) -> Vec<DrawCmd> {
    let mut cmds = Vec::new();
    for item in &scene.items {
        match item {
            // The `radius` rides into the quad instance and the fragment shader's
            // rounded-rect SDF rounds the corners on the GPU (clamped to half the
            // shorter side, antialiased ~1px edge) — closing the gap with the CPU
            // tiers' `fill_round_rect`. `radius == 0` draws a square as before.
            DisplayItem::Rect {
                rect,
                color,
                radius,
            } => {
                cmds.push(DrawCmd::Rect(QuadInstance::rect(*rect, *color, *radius)));
            }
            DisplayItem::Text {
                origin,
                text,
                color,
                size,
                box_w,
                align,
            } => {
                // Rasterize the run to an antialiased coverage mask once. An
                // empty/whitespace run yields a zero-ink mask, which draws as
                // nothing — fine to keep (a fully-transparent quad).
                let mask = engine.rasterize(text, *size, *color);
                // Center / right-align the run within its box using the run's OWN
                // real pixel width (`mask.width`, the tight ink box) — the honest
                // metric for these proportional glyphs, exactly as the CPU sharp-text
                // path does. Offset = (box_w - run_w) * align, clamped to >= 0 so a
                // too-narrow box never pushes ink left; `align == 0.0` => 0 (legacy).
                let offset = ((box_w - mask.width as f32) * align).max(0.0);
                let origin = Point {
                    x: origin.x + offset,
                    y: origin.y,
                };
                cmds.push(DrawCmd::Glyphs(GlyphRun {
                    origin,
                    mask,
                    color: *color,
                }));
            }
            // A drop shadow lowers to a single softened, offset, rounded quad behind
            // the box. Emitted in place, so a display list that puts the shadow before
            // its box paints it underneath.
            DisplayItem::Shadow {
                rect,
                color,
                blur,
                offset,
            } => {
                lower_shadow(&mut cmds, *rect, *color, *blur, *offset);
            }
            // A gradient lowers to a real interpolated slab ramp across its axis.
            DisplayItem::Gradient {
                rect,
                stops,
                direction,
            } => {
                lower_gradient(&mut cmds, *rect, stops, *direction);
            }
            // A border lowers to four edge-band quads (a stroked frame).
            DisplayItem::Border {
                rect,
                color,
                width,
                radius,
            } => {
                lower_border(&mut cmds, *rect, *color, *width, *radius);
            }
            DisplayItem::Glyphs { .. } => {}
            // `DisplayItem` is `#[non_exhaustive]`: a future primitive lands in this
            // forward-compat arm and is skipped (paint nothing rather than panic).
            _ => {}
        }
    }
    cmds
}

/// The texture format of the offscreen render target and the readback. sRGB so
/// straight-alpha byte colors round-trip.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// A GPU device + pipelines that render a display list into an offscreen texture.
///
/// Construct once with [`GpuRenderer::new`]; it owns the `wgpu` device/queue, the
/// colored-quad and textured-glyph render pipelines, and a [`TextEngine`] used to
/// rasterize Text runs. The offscreen texture is (re)allocated to match the
/// requested size on [`render`](Renderer::render) / [`resize`](Renderer::resize).
pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Solid colored-quad pipeline (rects).
    rect_pipeline: wgpu::RenderPipeline,
    /// Textured glyph-quad pipeline (antialiased text).
    glyph_pipeline: wgpu::RenderPipeline,
    viewport_layout: wgpu::BindGroupLayout,
    /// Layout for a glyph run's `(uniform, texture, sampler)` bind group.
    glyph_layout: wgpu::BindGroupLayout,
    /// Bilinear sampler for the coverage texture (shared across runs).
    sampler: wgpu::Sampler,
    /// Real-glyph rasterizer, reused frame-to-frame (it caches shaped glyphs).
    text_engine: TextEngine,
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

    /// Build the pipelines on an already-initialized device/queue.
    fn from_device(
        device: wgpu::Device,
        queue: wgpu::Queue,
        width: u32,
        height: u32,
        clear: Color,
    ) -> Self {
        // Shared viewport uniform layout (group 0), used by both pipelines.
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

        let rect_pipeline = build_rect_pipeline(&device, &viewport_layout);
        let (glyph_pipeline, glyph_layout) = build_glyph_pipeline(&device, &viewport_layout);

        // One bilinear, clamped sampler for every coverage texture. Linear
        // filtering smooths the mask under any scaling; clamp-to-edge keeps the
        // border from bleeding the wrap color into AA edges.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("canopy coverage sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        Self {
            device,
            queue,
            rect_pipeline,
            glyph_pipeline,
            viewport_layout,
            glyph_layout,
            sampler,
            text_engine: TextEngine::new(),
            width: width.max(1),
            height: height.max(1),
            clear,
            last_frame: Vec::new(),
        }
    }

    /// Render `scene` into a fresh offscreen texture and copy it back to RGBA8
    /// (row-major). Always allocates the target at the current size, so it is safe
    /// to call after a `resize`.
    fn render_frame(&mut self, scene: &DisplayList) -> Vec<u8> {
        let cmds = lower(scene, &mut self.text_engine);
        draw_frame(
            &self.device,
            &self.queue,
            &self.rect_pipeline,
            &self.glyph_pipeline,
            &self.viewport_layout,
            &self.glyph_layout,
            &self.sampler,
            self.width,
            self.height,
            self.clear,
            &cmds,
        )
    }
}

/// Build the colored-quad pipeline (rects): instanced, alpha-blended, sourcing
/// `quad.wgsl`. Kept beside the glyph pipeline so the two are built side by side.
///
/// The color target uses [`wgpu::BlendState::ALPHA_BLENDING`] — straight (non-
/// premultiplied) source-over — so a [`DisplayItem::Rect`] whose color alpha is
/// `< 255` blends over whatever is already in the target instead of overwriting it.
/// That is what lets a faded-in (reduced-opacity) rect composite over its background
/// on the GPU, matching the CPU `fill_rect` blend. An `alpha == 1.0` rect is fully
/// opaque, so the blend resolves to a plain overwrite (the original behavior).
fn build_rect_pipeline(
    device: &wgpu::Device,
    viewport_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("canopy quad shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("quad.wgsl").into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("canopy rect pipeline layout"),
        bind_group_layouts: &[Some(viewport_layout)],
        immediate_size: 0,
    });

    // One instance buffer; each QuadInstance is origin(vec2 @0) + size(vec2 @8) +
    // color(vec4 @16) + radius(f32 @32), stepped per-instance. The trailing `_pad`
    // (@36) is not surfaced as an attribute — the shader never reads it.
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
            wgpu::VertexAttribute {
                offset: 32,
                shader_location: 3,
                format: wgpu::VertexFormat::Float32,
            },
        ],
    };

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
    })
}

/// Build the textured glyph-quad pipeline (antialiased text), sourcing
/// `glyph.wgsl`. No vertex buffer: the quad corners come from `vertex_index` and
/// each run rebinds group 1 = `(GlyphRun uniform, coverage texture, sampler)`.
///
/// Returns the pipeline and the group-1 bind-group layout so runs can build
/// their bind groups.
fn build_glyph_pipeline(
    device: &wgpu::Device,
    viewport_layout: &wgpu::BindGroupLayout,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("canopy glyph shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("glyph.wgsl").into()),
    });

    // Group 1: per-run uniform (placement + ink), the coverage texture, and the
    // sampler. Uniform is visible to the vertex stage (placement) and fragment
    // stage (ink color); the texture/sampler are fragment-only.
    let glyph_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("canopy glyph bind group layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("canopy glyph pipeline layout"),
        bind_group_layouts: &[Some(viewport_layout), Some(&glyph_layout)],
        immediate_size: 0,
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("canopy glyph pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
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

    (pipeline, glyph_layout)
}

/// Upload a run's coverage mask as an `R8Unorm` texture and build its group-1
/// bind group `(uniform, texture view, sampler)`.
///
/// Returns `None` for a zero-area mask (nothing to draw). The mask bytes are
/// uploaded with the wgpu 256-byte row alignment requirement honored.
fn build_glyph_bind_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    run: &GlyphRun,
) -> Option<wgpu::BindGroup> {
    let w = run.mask.width;
    let h = run.mask.height;
    if w == 0 || h == 0 {
        return None;
    }

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("canopy coverage texture"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Single 8-bit channel: the alpha-coverage value, sampled as `.r`.
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    // `write_texture` handles row padding internally, so we can hand it the
    // tight `width`-byte rows directly (1 byte per pixel for R8Unorm).
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &run.mask.coverage,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let uniform = GlyphRunUniform {
        origin: [run.origin.x, run.origin.y],
        size: [w as f32, h as f32],
        color: color_to_linear_rgba(run.color),
    };
    let uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("canopy glyph run uniform"),
        contents: bytemuck::bytes_of(&uniform),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("canopy glyph bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    }))
}

/// The whole offscreen draw: allocate target + readback buffers, encode one
/// render pass that paints every [`DrawCmd`] back-to-front (rects via the
/// colored-quad pipeline, text via the textured glyph pipeline), copy the
/// texture to the readback buffer, map it, and unswizzle the row-padded copy
/// into a tight RGBA8 `Vec`.
///
/// Free function (not a method) so the test can exercise it directly, and so the
/// borrows of device/queue/pipelines stay explicit.
#[allow(clippy::too_many_arguments)]
fn draw_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rect_pipeline: &wgpu::RenderPipeline,
    glyph_pipeline: &wgpu::RenderPipeline,
    viewport_layout: &wgpu::BindGroupLayout,
    glyph_layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    width: u32,
    height: u32,
    clear: Color,
    cmds: &[DrawCmd],
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

    // Viewport uniform (group 0), shared by both pipelines.
    let viewport = Viewport {
        size: [width as f32, height as f32],
        _pad: [0.0, 0.0],
    };
    let viewport_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("canopy viewport uniform"),
        contents: bytemuck::bytes_of(&viewport),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let viewport_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("canopy viewport bind group"),
        layout: viewport_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: viewport_buf.as_entire_binding(),
        }],
    });

    // Build per-command GPU resources up front (these borrow the device, which
    // the render pass also needs immutably — fine, but the resources must
    // outlive the pass, so they live here).
    //
    // Rect instances are packed into one shared vertex buffer; each rect draws
    // `instance_base..instance_base+1` of it. Glyph runs each get a bind group.
    let mut rect_instances: Vec<QuadInstance> = Vec::new();
    enum Cmd {
        /// Draw instance `i` of the shared rect instance buffer.
        Rect(u32),
        /// Draw the glyph quad for this prepared bind group.
        Glyphs(wgpu::BindGroup),
    }
    let mut plan: Vec<Cmd> = Vec::new();
    for cmd in cmds {
        match cmd {
            DrawCmd::Rect(inst) => {
                let i = rect_instances.len() as u32;
                rect_instances.push(*inst);
                plan.push(Cmd::Rect(i));
            }
            DrawCmd::Glyphs(run) => {
                if let Some(bg) = build_glyph_bind_group(device, queue, glyph_layout, sampler, run)
                {
                    plan.push(Cmd::Glyphs(bg));
                }
                // A zero-area / all-whitespace run is dropped (nothing to draw).
            }
        }
    }

    // The rect instance buffer. An empty slice cannot be uploaded, so an empty
    // list gets one zero placeholder we then never reference.
    let placeholder = [QuadInstance::zeroed()];
    let instance_bytes: &[u8] = if rect_instances.is_empty() {
        bytemuck::cast_slice(&placeholder)
    } else {
        bytemuck::cast_slice(&rect_instances)
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
            label: Some("canopy frame pass"),
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

        // Group 0 (viewport) is the same for both pipelines.
        pass.set_bind_group(0, &viewport_group, &[]);
        pass.set_vertex_buffer(0, instance_buf.slice(..));

        // Track which pipeline is bound to avoid redundant `set_pipeline` calls.
        // Painting strictly in `plan` order preserves back-to-front compositing.
        let mut current_is_glyph: Option<bool> = None;
        for step in &plan {
            match step {
                Cmd::Rect(i) => {
                    if current_is_glyph != Some(false) {
                        pass.set_pipeline(rect_pipeline);
                        current_is_glyph = Some(false);
                    }
                    // 6 vertices (two triangles), this single instance.
                    pass.draw(0..6, *i..*i + 1);
                }
                Cmd::Glyphs(bg) => {
                    if current_is_glyph != Some(true) {
                        pass.set_pipeline(glyph_pipeline);
                        current_is_glyph = Some(true);
                    }
                    pass.set_bind_group(1, bg, &[]);
                    // 6 vertices (two triangles); the glyph pipeline ignores
                    // instancing (quad corners come from `vertex_index`).
                    pass.draw(0..6, 0..1);
                }
            }
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
/// This spins up a fresh `wgpu` device and `TextEngine`, so it is convenient for
/// tests and tools but heavier than reusing a [`GpuRenderer`]. **Panics** if no
/// GPU adapter is available — use [`try_render_to_rgba`] to handle that case
/// gracefully.
pub fn render_to_rgba(scene: &DisplayList, size: Size, clear: Color) -> Vec<u8> {
    try_render_to_rgba(scene, size, clear).expect("no GPU adapter available")
}

/// Fallible one-shot offscreen render. Returns `None` if no GPU adapter could be
/// acquired; otherwise the RGBA8 pixels (row-major).
pub fn try_render_to_rgba(scene: &DisplayList, size: Size, clear: Color) -> Option<Vec<u8>> {
    let mut r = GpuRenderer::new(size.w as u32, size.h as u32, clear)?;
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
                radius: 0.0,
            }],
        }
    }

    #[test]
    fn lowering_rasterizes_text_to_a_glyph_run() {
        // A Text run lowers to exactly one antialiased glyph run (not N opaque
        // ink quads): the coverage mask carries partial-coverage edges the baked
        // path could never produce.
        let mut engine = TextEngine::new();
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
                size: 16.0,
                // Left-aligned (align 0.0): box_w is irrelevant, no offset.
                box_w: 0.0,
                align: 0.0,
            }],
        };
        let cmds = lower(&scene, &mut engine);
        assert_eq!(cmds.len(), 1, "one Text item -> one draw command");
        let DrawCmd::Glyphs(run) = &cmds[0] else {
            panic!("Text must lower to a glyph run");
        };
        assert!(
            run.mask.width > 0 && run.mask.height > 0,
            "the run has a sized coverage mask"
        );
        assert!(run.mask.ink_pixels() > 0, "the run has some ink");
        let partial = run
            .mask
            .coverage
            .iter()
            .filter(|&&c| c > 0 && c < 255)
            .count();
        assert!(
            partial > 0,
            "antialiased mask must have partial-coverage pixels, got {partial}"
        );
    }

    #[test]
    fn lowering_keeps_back_to_front_order() {
        // A rect then a text run must lower to a Rect command before a Glyphs
        // command, so the glyph composites on top of its background.
        let mut engine = TextEngine::new();
        let scene = DisplayList {
            items: vec![
                DisplayItem::Rect {
                    rect: Rect {
                        origin: Point { x: 0.0, y: 0.0 },
                        size: Size { w: 8.0, h: 8.0 },
                    },
                    color: Color {
                        r: 9,
                        g: 9,
                        b: 9,
                        a: 255,
                    },
                    radius: 0.0,
                },
                DisplayItem::Text {
                    origin: Point { x: 0.0, y: 0.0 },
                    text: "x".into(),
                    color: Color {
                        r: 1,
                        g: 1,
                        b: 1,
                        a: 255,
                    },
                    size: 16.0,
                    // Left-aligned (align 0.0): box_w is irrelevant, no offset.
                    box_w: 0.0,
                    align: 0.0,
                },
            ],
        };
        let cmds = lower(&scene, &mut engine);
        assert_eq!(cmds.len(), 2);
        assert!(matches!(cmds[0], DrawCmd::Rect(_)), "rect first");
        assert!(matches!(cmds[1], DrawCmd::Glyphs(_)), "glyphs second");
    }

    /// `sample_gradient` interpolates between the surrounding stops and clamps at the
    /// ends — the per-slab color the GPU gradient ramp paints.
    #[test]
    fn gradient_sampling_interpolates_and_clamps() {
        use canopy_traits::{GradientStop, GradientStops};
        let black = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let white = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        let stops = GradientStops::from_slice(&[
            GradientStop {
                color: black,
                position: 0.0,
            },
            GradientStop {
                color: white,
                position: 1.0,
            },
        ]);
        // Endpoints clamp to the stop colors.
        assert_eq!(sample_gradient(&stops, 0.0), black);
        assert_eq!(sample_gradient(&stops, 1.0), white);
        // Before/after the range clamps too.
        assert_eq!(sample_gradient(&stops, -1.0), black);
        assert_eq!(sample_gradient(&stops, 2.0), white);
        // The midpoint is the channel-wise average (~128).
        let mid = sample_gradient(&stops, 0.5);
        assert!(
            mid.r > 120 && mid.r < 136,
            "midpoint is ~half-gray, got {}",
            mid.r
        );
        assert_eq!(mid.r, mid.g);
        assert_eq!(mid.g, mid.b);
        // An empty set degrades to transparent black, never panicking.
        assert_eq!(
            sample_gradient(&GradientStops::default(), 0.5),
            Color::default()
        );
    }

    /// A `DisplayItem::Gradient` lowers to exactly `GRADIENT_SLABS` colored quads
    /// whose colors actually vary across the axis (a real ramp), and the slabs tile
    /// the box along the gradient direction.
    #[test]
    fn gradient_lowers_to_a_varying_slab_ramp() {
        use canopy_traits::{GradientDirection, GradientStop, GradientStops};
        let mut engine = TextEngine::new();
        let scene = DisplayList {
            items: vec![DisplayItem::Gradient {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size { w: 64.0, h: 32.0 },
                },
                stops: GradientStops::from_slice(&[
                    GradientStop {
                        color: Color {
                            r: 0,
                            g: 0,
                            b: 0,
                            a: 255,
                        },
                        position: 0.0,
                    },
                    GradientStop {
                        color: Color {
                            r: 255,
                            g: 255,
                            b: 255,
                            a: 255,
                        },
                        position: 1.0,
                    },
                ]),
                direction: GradientDirection::Vertical,
            }],
        };
        let cmds = lower(&scene, &mut engine);
        assert_eq!(
            cmds.len(),
            GRADIENT_SLABS as usize,
            "one quad per gradient slab"
        );
        // Every command is a rect; the first and last slabs differ in color (a real
        // ramp, not a flat fill).
        let first_color = match &cmds[0] {
            DrawCmd::Rect(q) => q.color,
            _ => panic!("gradient slabs are rects"),
        };
        let last_color = match cmds.last().unwrap() {
            DrawCmd::Rect(q) => q.color,
            _ => panic!("gradient slabs are rects"),
        };
        assert_ne!(
            first_color, last_color,
            "the ramp's ends must differ (the slabs interpolate)"
        );
    }

    /// A `DisplayItem::Border` lowers to four edge-band quads — a stroked frame, not a
    /// fill.
    #[test]
    fn border_lowers_to_four_edge_bands() {
        let mut engine = TextEngine::new();
        let scene = DisplayList {
            items: vec![DisplayItem::Border {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size { w: 40.0, h: 30.0 },
                },
                color: Color {
                    r: 200,
                    g: 0,
                    b: 0,
                    a: 255,
                },
                width: 3.0,
                radius: 4.0,
            }],
        };
        let cmds = lower(&scene, &mut engine);
        assert_eq!(cmds.len(), 4, "a border is four edge bands");
        assert!(
            cmds.iter().all(|c| matches!(c, DrawCmd::Rect(_))),
            "every band is a rect quad"
        );
    }

    /// A `DisplayItem::Shadow` lowers to a single softened, offset quad — and a fully
    /// transparent shadow emits nothing.
    #[test]
    fn shadow_lowers_to_one_softened_quad_or_nothing() {
        let mut engine = TextEngine::new();
        let visible = DisplayList {
            items: vec![DisplayItem::Shadow {
                rect: Rect {
                    origin: Point { x: 10.0, y: 10.0 },
                    size: Size { w: 20.0, h: 20.0 },
                },
                color: Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 200,
                },
                blur: 6.0,
                offset: Point { x: 2.0, y: 4.0 },
            }],
        };
        let cmds = lower(&visible, &mut engine);
        assert_eq!(cmds.len(), 1, "a visible shadow is one quad");
        match &cmds[0] {
            DrawCmd::Rect(q) => {
                // Offset applied: the shadow's origin is the box origin + offset.
                assert_eq!(q.origin, [12.0, 14.0], "shadow is offset behind the box");
                // Softened: a blurred shadow's alpha is below the source alpha.
                assert!(
                    q.color[3] < 200.0 / 255.0,
                    "blur softens (fades) the shadow alpha"
                );
                // Rounded by the blur radius.
                assert!(q.radius > 0.0, "shadow corners are rounded by the blur");
            }
            _ => panic!("shadow lowers to a rect quad"),
        }

        // A fully transparent shadow draws nothing.
        let invisible = DisplayList {
            items: vec![DisplayItem::Shadow {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size { w: 10.0, h: 10.0 },
                },
                color: Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 0,
                },
                blur: 2.0,
                offset: Point { x: 0.0, y: 0.0 },
            }],
        };
        assert!(
            lower(&invisible, &mut engine).is_empty(),
            "a transparent shadow emits no draw commands"
        );
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
                radius: 0.0,
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

    /// THE GLYPH PARITY TEST: render a real Text run on a contrasting background
    /// on the actual Metal GPU and prove the output has true antialiased ink —
    /// partial-coverage gradient pixels between background and full ink, which the
    /// old baked 8×8 path could never produce — and that ink lands where the text
    /// is. Skips cleanly if no adapter exists.
    #[test]
    fn text_renders_antialiased_glyphs_on_gpu() {
        // Big white text on a dark surface, the demo's contrast.
        let size = Size { w: 160.0, h: 48.0 };
        let bg = Color {
            r: 0x1e,
            g: 0x1e,
            b: 0x2e,
            a: 255,
        };
        let ink = Color {
            r: 0xcd,
            g: 0xd6,
            b: 0xf4,
            a: 255,
        };
        let scene = DisplayList {
            items: vec![DisplayItem::Text {
                origin: Point { x: 8.0, y: 8.0 },
                text: "Canopy".into(),
                color: ink,
                size: 32.0,
                // Left-aligned (align 0.0): box_w is irrelevant, no offset.
                box_w: 0.0,
                align: 0.0,
            }],
        };
        let Some(px) = try_render_to_rgba(&scene, size, bg) else {
            eprintln!("no GPU adapter; skipping GPU assertion");
            return;
        };
        let w = size.w as usize;
        let h = size.h as usize;
        assert_eq!(px.len(), w * h * 4);

        // Classify every pixel against the two endpoints. A baked path produces
        // ONLY background or full-ink pixels (hard 0/255 edges); a real AA
        // rasterizer fills the gap with partial-coverage gradient pixels.
        let mut ink_px = 0usize; // close to full ink
        let mut bg_px = 0usize; // close to background
        let mut aa_px = 0usize; // strictly between -> antialiasing
        let (mut sum_x, mut sum_y, mut ink_weight) = (0f64, 0f64, 0f64);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let r = px[i] as i32;
                let g = px[i + 1] as i32;
                let b = px[i + 2] as i32;
                // Distance from the two endpoints in RGB.
                let d_bg =
                    (r - bg.r as i32).abs() + (g - bg.g as i32).abs() + (b - bg.b as i32).abs();
                let d_ink =
                    (r - ink.r as i32).abs() + (g - ink.g as i32).abs() + (b - ink.b as i32).abs();
                if d_bg <= 6 {
                    bg_px += 1;
                } else if d_ink <= 6 {
                    ink_px += 1;
                } else {
                    // Genuinely between the endpoints: an antialiased edge pixel.
                    aa_px += 1;
                }
                // Weight ink-ward pixels for a rough centroid of the text.
                if d_bg > 6 {
                    let weight = (d_bg as f64) / ((d_bg + d_ink) as f64 + 1.0);
                    sum_x += x as f64 * weight;
                    sum_y += y as f64 * weight;
                    ink_weight += weight;
                }
            }
        }
        eprintln!(
            "glyph GPU render: bg={bg_px} ink={ink_px} aa(partial)={aa_px} of {} px",
            w * h
        );

        // Real ink must be present...
        assert!(ink_px > 0, "expected full-ink pixels in the glyphs");
        // ...and crucially, antialiased gradient pixels must exist. This is the
        // parity proof: the baked path could only ever emit 0/255, never these.
        assert!(
            aa_px > 20,
            "expected many antialiased (partial-coverage) pixels, got {aa_px}"
        );
        // Most of the surface is still background (text is sparse).
        assert!(
            bg_px > ink_px,
            "background should dominate a short text run"
        );

        // Ink must land roughly where the text is: left-anchored at x=8, on the
        // upper band. The centroid should sit in the left-center, not off-canvas.
        assert!(ink_weight > 0.0, "must have inked pixels for a centroid");
        let cx = sum_x / ink_weight;
        let cy = sum_y / ink_weight;
        eprintln!("ink centroid ~ ({cx:.1}, {cy:.1})");
        assert!(
            cx > 8.0 && cx < (w as f64),
            "ink centroid x {cx:.1} should be right of the origin and on-canvas"
        );
        assert!(
            cy > 0.0 && cy < (h as f64),
            "ink centroid y {cy:.1} should be on-canvas"
        );
    }

    /// Glyph ink composites OVER a background rect: with a rect behind the text,
    /// the off-glyph pixels show the rect color (not the clear), proving the two
    /// pipelines share one alpha-blended pass in back-to-front order.
    #[test]
    fn text_composites_over_rect_on_gpu() {
        let size = Size { w: 96.0, h: 40.0 };
        let clear = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let card = Color {
            r: 0x45,
            g: 0x47,
            b: 0x5a,
            a: 255,
        };
        let ink = Color {
            r: 0xa6,
            g: 0xe3,
            b: 0xa1,
            a: 255,
        };
        let scene = DisplayList {
            items: vec![
                // A full-surface card behind the text.
                DisplayItem::Rect {
                    rect: Rect {
                        origin: Point { x: 0.0, y: 0.0 },
                        size: Size { w: 96.0, h: 40.0 },
                    },
                    color: card,
                    radius: 0.0,
                },
                DisplayItem::Text {
                    origin: Point { x: 6.0, y: 8.0 },
                    text: "Hi".into(),
                    color: ink,
                    size: 24.0,
                    // Left-aligned (align 0.0): box_w is irrelevant, no offset.
                    box_w: 0.0,
                    align: 0.0,
                },
            ],
        };
        let Some(px) = try_render_to_rgba(&scene, size, clear) else {
            eprintln!("no GPU adapter; skipping GPU assertion");
            return;
        };
        let w = size.w as usize;
        let h = size.h as usize;

        // A corner well away from any glyph shows the card color, never the
        // black clear — so the rect painted and the text didn't wipe it.
        let corner = {
            let i = ((h - 1) * w + (w - 1)) * 4;
            [px[i], px[i + 1], px[i + 2]]
        };
        assert_eq!(
            corner,
            [card.r, card.g, card.b],
            "off-glyph pixel must show the card behind the text"
        );

        // Some pixel must be near the green ink (the glyphs actually drew).
        let mut found_ink = false;
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let d = (px[i] as i32 - ink.r as i32).abs()
                    + (px[i + 1] as i32 - ink.g as i32).abs()
                    + (px[i + 2] as i32 - ink.b as i32).abs();
                if d <= 12 {
                    found_ink = true;
                }
            }
        }
        assert!(found_ink, "expected green glyph ink over the card");
    }

    /// THE ROUNDED-RECT GPU TEST: render a filled rounded rect (radius > 0) on a
    /// contrasting background on the real Metal adapter and prove the corners are
    /// rounded *away* on the GPU — the same property the CPU test
    /// `round_rect_clears_corners_keeps_center` checks, now on the GPU.
    ///
    /// The four extreme corner pixels lie well outside the corner arcs, so the
    /// SDF's coverage there is 0 and the background shows through *exactly* (no AA
    /// blend reaches that far). The center and the straight mid-edges are fully
    /// inside, so they are the exact fill color. This is the GPU/CPU parity proof
    /// for rounded corners. Skips cleanly if no adapter exists.
    #[test]
    fn round_rect_clears_corners_keeps_center_on_gpu() {
        // Mirror the CPU test's geometry: a 40x40 rect, radius 12, fill on bg.
        let bg = Color {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 255,
        };
        let fill = Color {
            r: 0xf3,
            g: 0x8b,
            b: 0xa8,
            a: 255,
        };
        let size = Size { w: 40.0, h: 40.0 };
        let scene = DisplayList {
            items: vec![DisplayItem::Rect {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size,
                },
                color: fill,
                radius: 12.0,
            }],
        };
        let Some(px) = try_render_to_rgba(&scene, size, bg) else {
            eprintln!("no GPU adapter; skipping GPU assertion");
            return;
        };
        let w = size.w as usize;
        let h = size.h as usize;
        assert_eq!(px.len(), w * h * 4);
        let at = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };

        // The four extreme corners are carved away: each sits ~4px outside the
        // r=12 arc, far beyond the ~1px AA band, so coverage is 0 -> exact bg.
        let bg_px = [bg.r, bg.g, bg.b, bg.a];
        assert_eq!(at(0, 0), bg_px, "top-left corner rounded away");
        assert_eq!(at(w - 1, 0), bg_px, "top-right corner rounded away");
        assert_eq!(at(0, h - 1), bg_px, "bottom-left corner rounded away");
        assert_eq!(at(w - 1, h - 1), bg_px, "bottom-right corner rounded away");

        // The center is solidly inside the rounded region -> exact fill.
        let fill_px = [fill.r, fill.g, fill.b, fill.a];
        assert_eq!(at(w / 2, h / 2), fill_px, "center is the fill color");

        // The straight mid-edges (a few px in from the edge, clear of both the
        // corner arcs and the AA band) are also exact fill — the rounding only
        // touches the corners, never the flat sides.
        assert_eq!(at(w / 2, 2), fill_px, "top mid-edge is straight (fill)");
        assert_eq!(at(2, h / 2), fill_px, "left mid-edge is straight (fill)");
        assert_eq!(
            at(w / 2, h - 3),
            fill_px,
            "bottom mid-edge is straight (fill)"
        );
        assert_eq!(
            at(w - 3, h / 2),
            fill_px,
            "right mid-edge is straight (fill)"
        );
    }

    /// An oversized radius clamps to half the shorter side and the square rect
    /// becomes a **circle** on the GPU — the GPU analog of the CPU
    /// `oversized_radius_clamps_to_a_circle` guarantee.
    ///
    /// With a 40x40 rect and a radius far larger than the box, the clamp pins it
    /// to 20 (half of 40), so the filled region is a disc of radius 20 centered in
    /// the square. We probe: the four corners are bg (well outside the disc); the
    /// center is fill; and a point just inside the disc along an axis is fill
    /// while the matching corner-diagonal point is bg. Skips without an adapter.
    #[test]
    fn oversized_radius_is_a_circle_on_gpu() {
        let bg = Color {
            r: 0x18,
            g: 0x18,
            b: 0x24,
            a: 255,
        };
        let fill = Color {
            r: 0x89,
            g: 0xb4,
            b: 0xfa,
            a: 255,
        };
        let size = Size { w: 40.0, h: 40.0 };
        let scene = DisplayList {
            items: vec![DisplayItem::Rect {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size,
                },
                color: fill,
                // Absurd radius: clamped to 20 -> a full circle inscribed in the
                // 40x40 square. (The CPU `fill_round_rect` clamps identically.)
                radius: 1000.0,
            }],
        };
        let Some(px) = try_render_to_rgba(&scene, size, bg) else {
            eprintln!("no GPU adapter; skipping GPU assertion");
            return;
        };
        let w = size.w as usize;
        let h = size.h as usize;
        let at = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        let bg_px = [bg.r, bg.g, bg.b, bg.a];
        let fill_px = [fill.r, fill.g, fill.b, fill.a];

        // Corners are outside the inscribed disc -> bg.
        assert_eq!(at(0, 0), bg_px, "corner outside the circle is bg");
        assert_eq!(at(w - 1, h - 1), bg_px, "opposite corner is bg");
        // Center is inside -> fill.
        assert_eq!(at(w / 2, h / 2), fill_px, "circle center is fill");
        // A point a couple px inside the disc along the top axis is fill (the
        // circle reaches the mid-edges), where the square's corner is empty —
        // proving it is a disc, not a square.
        assert_eq!(at(w / 2, 2), fill_px, "top of the circle is fill");
        assert_eq!(at(2, h / 2), fill_px, "left of the circle is fill");
    }

    /// A `radius == 0` rect must still draw a crisp **square** on the GPU: the SDF
    /// path collapses to a flat coverage of 1, so a corner pixel of a full-surface
    /// rect is the exact fill (no rounding, no AA fade). This guards the "keep the
    /// existing square behavior when radius == 0" requirement against regressions
    /// in the new shader.
    #[test]
    fn zero_radius_stays_square_on_gpu() {
        let bg = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let fill = Color {
            r: 0,
            g: 200,
            b: 80,
            a: 255,
        };
        let size = Size { w: 32.0, h: 24.0 };
        let scene = DisplayList {
            items: vec![DisplayItem::Rect {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size,
                },
                color: fill,
                radius: 0.0,
            }],
        };
        let Some(px) = try_render_to_rgba(&scene, size, bg) else {
            eprintln!("no GPU adapter; skipping GPU assertion");
            return;
        };
        let w = size.w as usize;
        let h = size.h as usize;
        let at = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        let fill_px = [fill.r, fill.g, fill.b, fill.a];
        // Every corner of a full-surface square rect is the exact fill — the new
        // SDF must not nibble corners when radius is 0.
        assert_eq!(at(0, 0), fill_px, "square keeps its top-left corner");
        assert_eq!(at(w - 1, 0), fill_px, "square keeps its top-right corner");
        assert_eq!(at(0, h - 1), fill_px, "square keeps its bottom-left corner");
        assert_eq!(
            at(w - 1, h - 1),
            fill_px,
            "square keeps its bottom-right corner"
        );
    }

    /// THE FADED-RECT GPU TEST: a translucent rect (alpha < 255) over an opaque
    /// background must **blend**, not overwrite. We paint a half-alpha white rect over
    /// an opaque blue clear and assert the covered pixel is strictly between the two —
    /// brighter than the blue on every channel, but not full white. This proves the
    /// rect pipeline's `ALPHA_BLENDING` is doing source-over for reduced-opacity fills
    /// (the GPU half of the fade-in story). Skips cleanly without an adapter.
    #[test]
    fn translucent_rect_blends_over_background_on_gpu() {
        let bg = Color {
            r: 0,
            g: 0,
            b: 200,
            a: 255,
        };
        let fill = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 128, // ~50% — a faded rect
        };
        let size = Size { w: 24.0, h: 24.0 };
        let scene = DisplayList {
            items: vec![DisplayItem::Rect {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size,
                },
                color: fill,
                radius: 0.0,
            }],
        };
        let Some(px) = try_render_to_rgba(&scene, size, bg) else {
            eprintln!("no GPU adapter; skipping GPU assertion");
            return;
        };
        let w = size.w as usize;
        let i = ((size.h as usize / 2) * w + w / 2) * 4;
        let [r, g, b, _a] = [px[i], px[i + 1], px[i + 2], px[i + 3]];

        // The half-alpha white lifts R and G off the blue floor (they were 0) but
        // does not reach full white, and pulls B down from 200 toward white's 255 is
        // not relevant — what matters is the pixel is a genuine blend, not either
        // endpoint. Blending happens in linear space (sRGB target), so we assert
        // ranges, not exact bytes.
        assert!(
            r > 40 && r < 255,
            "red channel must be a blend (lifted off 0, below full), got {r}"
        );
        assert!(g > 40 && g < 255, "green channel must be a blend, got {g}");
        // Not the untouched background and not the opaque fill.
        assert_ne!(
            [r, g, b],
            [bg.r, bg.g, bg.b],
            "must not be the bare background"
        );
        assert_ne!(
            [r, g, b],
            [fill.r, fill.g, fill.b],
            "must not overwrite to full fill"
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
