// Separable Gaussian blur, one axis per pass. Used for both halves of the
// optical glow chain (RENDERER.md §3.3).
//
// The target may be smaller than the source: the wide halo is blurred at
// reduced resolution, which is what "a small mip/blur chain" means in practice
// and what makes a σ of 0.06 affordable at all.
//
// Taps are evenly spaced and weighted by the Gaussian, with the spacing chosen
// on the CPU so that RADIUS taps reach about 3σ — beyond which the kernel
// contributes under half a percent.

struct Blur {
    // Per-tap offset in source UV, along this pass's axis.
    step: vec2<f32>,
    _pad: vec2<f32>,
}

const RADIUS: i32 = 8;
// RADIUS taps reach 3σ, so one tap is 3/8 of a σ.
const INV_SIGMA_TAPS: f32 = 0.375;

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

    for (var i = -RADIUS; i <= RADIUS; i++) {
        let d = f32(i) * INV_SIGMA_TAPS;
        let weight = exp(-0.5 * d * d);
        let uv = in.uv + blur.step * f32(i);
        total += textureSample(source, source_sampler, uv).rgb * weight;
        weight_total += weight;
    }

    return vec4<f32>(total / weight_total, 1.0);
}
