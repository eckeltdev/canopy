// Instanced colored-quad shader, with optional **rounded corners** via a
// signed-distance function (SDF).
//
// Each instance is a rectangle in *pixel* space (origin = top-left, plus a size)
// carrying a straight-alpha RGBA color and a corner `radius` (in pixels; 0 =
// square). The vertex stage expands the unit quad [0,1]x[0,1] into the instance
// rect, then maps pixel space to clip space:
// x: [0, vp.x] -> [-1, 1], y: [0, vp.y] -> [1, -1] (y flips so +y is downward,
// matching the CPU renderer and the readback row order).
//
// ## Why an SDF instead of carving pixels (like the CPU path)
// The CPU `fill_round_rect` skips pixels whose center falls outside a corner's
// quarter-circle — a hard in/out test. On the GPU we instead evaluate the exact
// rounded-rectangle signed distance per fragment and alpha-fade the boundary
// over ~1px with `smoothstep`. That gives **antialiased** corners (the CPU tier
// has hard edges), and the fragment's coverage drops to 0 well outside the arc,
// so the same "corner pixel keeps the background" property still holds — the GPU
// test asserts exactly that. When `radius == 0` the distance to the (un-rounded)
// box is <= 0 for every interior fragment, so coverage is a flat 1.0 and we
// reproduce the old square behavior bit-for-bit.

struct Viewport {
    // Surface size in pixels (x = width, y = height); z, w are padding to keep
    // the uniform buffer 16-byte aligned.
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> vp: Viewport;

struct Instance {
    // Top-left origin in pixels.
    @location(0) origin: vec2<f32>,
    // Size in pixels.
    @location(1) size: vec2<f32>,
    // Straight-alpha RGBA in [0, 1].
    @location(2) color: vec4<f32>,
    // Corner radius in pixels (already clamped to half the shorter side on the
    // CPU side). 0 = square.
    @location(3) radius: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Fragment position *relative to the rect center*, in pixels. The SDF works
    // in this centered frame so the four corners are symmetric.
    @location(1) local: vec2<f32>,
    // Half-extent of the rect (size * 0.5), in pixels — the SDF's box half-size.
    // (Named `half_size`, not `half`, because `half` is a reserved type name in
    // Metal/MSL and the WGSL->MSL translation would collide with it.)
    @location(2) half_size: vec2<f32>,
    // Corner radius in pixels, carried to the fragment stage.
    @location(3) radius: f32,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32, inst: Instance) -> VsOut {
    // Unit quad as two triangles (CCW): (0,0) (1,0) (0,1) | (0,1) (1,0) (1,1).
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vid];
    let px = inst.origin + corner * inst.size;

    // Pixel -> normalized [0,1], then -> clip with a y flip.
    let ndc = px / vp.size;
    let clip = vec2<f32>(ndc.x * 2.0 - 1.0, 1.0 - ndc.y * 2.0);

    var out: VsOut;
    out.clip = vec4<f32>(clip, 0.0, 1.0);
    out.color = inst.color;
    // Centered local position: corner (0,0) maps to -half, corner (1,1) to +half.
    // The rasterizer interpolates this linearly across the quad, so each fragment
    // carries its own pixel offset from the rect center.
    out.local = (corner - vec2<f32>(0.5, 0.5)) * inst.size;
    out.half_size = inst.size * 0.5;
    out.radius = inst.radius;
    return out;
}

// Signed distance from point `p` to a rounded box centered at the origin with
// half-extents `b` and corner radius `r`. Negative inside, positive outside,
// zero on the boundary. The classic Inigo-Quilez rounded-box SDF: shrink the box
// half-extents by `r`, measure the distance to that inner box, then subtract `r`
// to re-inflate the corners into quarter-circles.
fn rounded_box_sdf(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // radius <= 0 -> a plain square: the distance to the un-rounded box is <= 0
    // for every interior fragment, so coverage is a flat 1.0 below.
    let dist = rounded_box_sdf(in.local, in.half_size, max(in.radius, 0.0));

    // Antialias the boundary over ~1px: coverage is 1 well inside (dist <= -0.5),
    // 0 well outside (dist >= +0.5), linearly faded across the 1px band. A pixel
    // center clearly outside the rounded region (the extreme corners for a
    // non-trivial radius) gets coverage 0 -> the background shows through,
    // matching the CPU `fill_round_rect` "corner cleared" guarantee.
    let coverage = 1.0 - smoothstep(-0.5, 0.5, dist);

    // Fold coverage into alpha (straight-alpha). With ALPHA_BLENDING this gives
    // antialiased rounded edges over whatever is already in the target; for a
    // square (coverage == 1 inside) it is the original opaque fill.
    return vec4<f32>(in.color.rgb, in.color.a * coverage);
}
