// Textured glyph-quad shader: draws ONE alpha-blended quad textured by a glyph
// run's 8-bit alpha-coverage mask (an `R8Unorm` texture produced by
// `canopy-text-parley`).
//
// This is the GPU analog of the CPU "sharp text" path: instead of expanding a
// 1-bit baked font into opaque pixel quads, we sample a real antialiased
// coverage mask and emit `(ink.rgb, coverage * ink.a)`, so the blend stage
// composites true AA edges over whatever is already in the target.
//
// Geometry is the same pixel-space -> clip-space mapping the colored-quad shader
// uses (y flips so +y is downward), expanding the unit quad over the run's
// pixel rectangle. The vertex stage also emits the matching [0,1] UVs so the
// fragment stage can sample the coverage texture.

struct Viewport {
    // Surface size in pixels (x = width, y = height); z, w are padding to keep
    // the uniform buffer 16-byte aligned.
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> vp: Viewport;

// The glyph run: where to place the coverage mask (pixel-space rect) and the
// ink color to tint it with. Lives in its own uniform so each run can rebind a
// fresh placement + color without a vertex buffer.
struct GlyphRun {
    // Top-left origin of the coverage rect, in pixels.
    origin: vec2<f32>,
    // Size of the coverage rect, in pixels (= the mask's width/height).
    size: vec2<f32>,
    // Straight-alpha RGBA ink color in [0, 1]. The texture supplies coverage,
    // which we fold into alpha; `color.a` further scales it for translucent ink.
    color: vec4<f32>,
};

@group(1) @binding(0) var<uniform> run: GlyphRun;
// The coverage mask: one 8-bit channel, sampled as `r` in [0, 1].
@group(1) @binding(1) var cov_tex: texture_2d<f32>;
@group(1) @binding(2) var cov_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Unit quad as two triangles (CCW), same winding as the colored-quad shader.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vid];
    let px = run.origin + corner * run.size;

    // Pixel -> normalized [0,1], then -> clip with a y flip.
    let ndc = px / vp.size;
    let clip = vec2<f32>(ndc.x * 2.0 - 1.0, 1.0 - ndc.y * 2.0);

    var out: VsOut;
    out.clip = vec4<f32>(clip, 0.0, 1.0);
    // UVs follow the corner directly: (0,0) is the mask's top-left row-major
    // origin, which matches how the coverage bytes were uploaded.
    out.uv = corner;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // The mask stores coverage in the red channel (R8Unorm).
    let coverage = textureSample(cov_tex, cov_samp, in.uv).r;
    // Straight-alpha output: ink color with coverage (scaled by ink alpha) as
    // alpha. The pipeline's ALPHA_BLENDING then composites AA edges correctly.
    return vec4<f32>(run.color.rgb, run.color.a * coverage);
}
