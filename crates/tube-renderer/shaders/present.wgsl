// Debug present: show a deposit-resolution buffer on the window surface,
// letterboxed to the tube aspect. Exposure-scaled only — the real readout
// chain (combine, glow, geometry, glass, tonemap) is RENDERER.md §3.3.

struct Present {
    // Fraction of the window the tube face occupies, per axis.
    fit: vec2<f32>,
    exposure: f32,
    _pad: f32,
}

@group(0) @binding(0) var field: texture_2d<f32>;
@group(0) @binding(1) var field_sampler: sampler;
@group(0) @binding(2) var<uniform> present: Present;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    // Fullscreen triangle: (-1,-1), (-1,3), (3,-1).
    let ndc = vec2<f32>(
        f32(index / 2u) * 4.0 - 1.0,
        f32(index % 2u) * 4.0 - 1.0,
    );
    var out: VertexOut;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    // Letterbox, and flip y: texture row 0 is the top of the tube face.
    out.uv = vec2<f32>(
        (ndc.x / present.fit.x) * 0.5 + 0.5,
        0.5 - (ndc.y / present.fit.y) * 0.5,
    );
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    if any(in.uv < vec2<f32>(0.0)) || any(in.uv > vec2<f32>(1.0)) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let energy = textureSample(field, field_sampler, in.uv).rgb;
    return vec4<f32>(energy * present.exposure, 1.0);
}
