// Point-splat deposition — THE COUNTER-EXAMPLE. Debug only.
//
// This is what the renderer must NOT do. CONTENTS.md forbids point splats and
// RENDERER.md §3.1 says why: a splat evaluates the spot kernel at discrete
// sample positions instead of integrating along the path, so any stroke that
// moves further than σ between samples renders as a string of beads rather
// than a line. It exists solely so the failure has a picture next to it, and
// so the beading self-check has something to prove it can discriminate.
//
// Nothing selects this path except an explicit debug flag. It is never part of
// a render anyone is meant to look at.
//
// Energy is matched to the analytic path deliberately — the same ∫drive dt is
// deposited per span, merely concentrated at a point instead of spread along
// the span. The contrast is therefore purely about distribution, which is the
// honest comparison.

const PI: f32 = 3.14159265358979;

const DISCONTINUITY: u32 = 1u;

// TRACE-FORMAT.md §2, byte for byte. See deposit.wgsl.
struct Sample {
    x: f32,
    y: f32,
    drive_r: f32,
    drive_g: f32,
    drive_b: f32,
    t: f32,
    flags: u32,
    reserved: u32,
}

struct Params {
    resolution: vec2<u32>,
    scale_x: f32,
    scale_y: f32,
    sigma0: f32,
    sigma1: f32,
    gamma_s: f32,
    _pad: f32,
}

struct SpanDispatch {
    first: u32,
    origin_x: u32,
    origin_y: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> samples: array<Sample>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var<uniform> span: SpanDispatch;
@group(0) @binding(3) var<storage, read_write> accumulator: array<vec4<f32>>;

fn spot_sigma(drive: f32) -> f32 {
    return params.sigma0 + params.sigma1 * pow(max(drive, 0.0), params.gamma_s);
}

fn to_texels(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        (p.x + 1.0) * params.scale_x,
        (1.0 - p.y) * params.scale_y,
    );
}

fn mean_drive(s: Sample) -> f32 {
    return (s.drive_r + s.drive_g + s.drive_b) / 3.0;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let texel = vec2<u32>(span.origin_x + gid.x, span.origin_y + gid.y);
    if texel.x >= params.resolution.x || texel.y >= params.resolution.y {
        return;
    }

    let s0 = samples[span.first];
    let s1 = samples[span.first + 1u];

    if (s1.flags & DISCONTINUITY) != 0u {
        return;
    }

    let d0 = vec3<f32>(s0.drive_r, s0.drive_g, s0.drive_b);
    let d1 = vec3<f32>(s1.drive_r, s1.drive_g, s1.drive_b);
    if all(d0 <= vec3<f32>(0.0)) && all(d1 <= vec3<f32>(0.0)) {
        return;
    }

    let dt = s1.t - s0.t;
    if dt <= 0.0 {
        return;
    }

    let to_face = 1.0 / params.scale_y;
    let p0 = to_texels(vec2<f32>(s0.x, s0.y)) * to_face;
    let q = (vec2<f32>(f32(texel.x) + 0.5, f32(texel.y) + 0.5)) * to_face;

    let sigma = spot_sigma(0.5 * (mean_drive(s0) + mean_drive(s1)));

    // The whole span's energy, dumped at one point. This single line is the
    // entire difference from the analytic path — and it is enough to bead.
    let r = q - p0;
    let kernel = exp(-dot(r, r) / (2.0 * sigma * sigma)) / (2.0 * PI * sigma * sigma);
    let energy = 0.5 * (d0 + d1) * dt * kernel;

    let index = texel.y * params.resolution.x + texel.x;
    accumulator[index] += vec4<f32>(max(energy, vec3<f32>(0.0)), 0.0);
}
