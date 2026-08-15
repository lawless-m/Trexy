// Separable Gaussian blur, one axis per pass. Used for both halves of the
// optical glow chain (RENDERER.md §3.3).
//
// The target may be smaller than the source: the wide halo is blurred at
// reduced resolution, which is what "a small mip/blur chain" means in practice
// and what makes a σ of 0.06 affordable at all.
//
// Taps are spaced exactly one *source* texel apart and the count varies with σ.
// The spacing is the part that must not move: a tap stride wider than a source
// texel stops averaging the source and starts sampling it, and a comb of point
// samples run separably — comb across, then comb down — lays a rectangular
// lattice over the picture. That is what a fixed stride of 3σ/8 did here, at a
// halo σ of 30.7 source texels: taps 11.5 texels apart, and a visible wire mesh
// over the whole face.

struct Blur {
    // Per-tap offset in source UV, along this pass's axis: one source texel.
    step: vec2<f32>,
    // σ in source texels, so tap i weighs exp(−i²/2σ²).
    sigma_texels: f32,
    // Taps each side of centre. Bounded on the CPU.
    taps: f32,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> blur: Blur;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    let ndc = vec2<f32>(
        f32(index / 2u) * 4.0 - 1.0,
        f32(index % 2u) * 4.0 - 1.0,
    );
    var out: VertexOut;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = vec2<f32>(ndc.x, -ndc.y) * 0.5 + 0.5;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    var total = vec3<f32>(0.0);
    var weight_total = 0.0;

    let taps = i32(blur.taps);
    let inv_sigma = 1.0 / max(blur.sigma_texels, 1e-6);

    for (var i = -taps; i <= taps; i++) {
        let d = f32(i) * inv_sigma;
        let weight = exp(-0.5 * d * d);
        let uv = in.uv + blur.step * f32(i);
        total += textureSample(source, source_sampler, uv).rgb * weight;
        weight_total += weight;
    }

    return vec4<f32>(total / weight_total, 1.0);
}
