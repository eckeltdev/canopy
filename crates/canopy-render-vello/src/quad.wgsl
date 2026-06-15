// Instanced colored-quad shader.
//
// Each instance is a rectangle in *pixel* space (origin = top-left, plus a size)
// carrying a straight-alpha RGBA color. The vertex stage expands the unit quad
// [0,1]x[0,1] into the instance rect, then maps pixel space to clip space:
// x: [0, vp.x] -> [-1, 1], y: [0, vp.y] -> [1, -1] (y flips so +y is downward,
// matching the CPU renderer and the readback row order).

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
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
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
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
