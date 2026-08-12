// The end of the chain: geometry, glow composite, overlay hook, glass, and the
// single tonemap. RENDERER.md §3.3, steps 3 to 6.
//
// Everything above this pass is linear light. Everything below it is display
// values. That boundary exists in exactly one place, which is what keeps an
// HDR output path a tonemap swap rather than a rewrite (CONTENTS.md).
//
// This pass also resolves the 2x deposit supersample: it renders at display
// resolution and samples the readout with a linear filter.

struct Tonemap {
    /// Face aspect (width/height), so the distortion stays circular.
    aspect: f32,
    /// Tube-profile pincushion coefficient.
    pincushion: f32,
    /// Radians.
    rotation: f32,
    /// >1 shows more than the face; <1 crops into it.
    overscan: f32,

    /// Long-range haze amplitude.
    halo_gain: f32,
    vignette: f32,
    reflection: f32,
    exposure: f32,
}

@group(0) @binding(0) var glow_tight: texture_2d<f32>;
@group(0) @binding(1) var glow_wide: texture_2d<f32>;
@group(0) @binding(2) var field_sampler: sampler;
@group(0) @binding(3) var<uniform> params: Tonemap;

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

/// Where on the tube face this output pixel is looking.
///
/// Distortion is applied in isotropic face coordinates — x scaled by the
/// aspect — so a round tube stays round on a 3:4 face. The sign convention is
/// that a positive coefficient samples further out with radius, which reads as
/// pincushion on screen.
fn distort(uv: vec2<f32>) -> vec2<f32> {
    var p = (uv - vec2<f32>(0.5)) * 2.0;
    p.x *= params.aspect;
    p /= params.overscan;

    let c = cos(params.rotation);
    let s = sin(params.rotation);
    p = vec2<f32>(p.x * c - p.y * s, p.x * s + p.y * c);

    p *= 1.0 + params.pincushion * dot(p, p);

    p.x /= params.aspect;
    return p * 0.5 + vec2<f32>(0.5);
}

/// Overlay pass hook. The feature is off in this slice (FIRST-SLICE.md §2);
/// when it arrives, a scanned translucent overlay multiplies here and an
/// unlit-overlay ambient term is added (RENDERER.md §3.3).
fn apply_overlay(light: vec3<f32>) -> vec3<f32> {
    return light;
}

/// Vignette plus a faint room reflection. Artistic class throughout; both
/// gains reach zero, which is the off switch.
fn apply_glass(light: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    let centred = (uv - vec2<f32>(0.5)) * 2.0;
    let r2 = dot(centred, centred);
    let vignette = 1.0 - params.vignette * r2 * 0.5;

    // A broad soft sheen off the upper left, as a lit room would leave.
    let sheen = exp(-4.0 * dot(uv - vec2<f32>(0.3, 0.25), uv - vec2<f32>(0.3, 0.25)));
    return light * max(vignette, 0.0) + vec3<f32>(params.reflection * sheen);
}

/// Narkowicz's ACES fit. The operator is deliberately one swappable function:
/// RENDERER.md §3.3 defers the choice, and the HDR path replaces exactly this.
fn tonemap(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let uv = distort(in.uv);
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        // Off the face entirely: no phosphor, so no light.
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // The tight blur is the faceplate scatter: the phosphor is never seen
    // directly, only through the glass. It is a distinct phenomenon from spot
    // size and is applied after emission, never merged with it
    // (RENDERER.md §3.3).
    let scattered = textureSample(glow_tight, field_sampler, uv).rgb;
    let halo = textureSample(glow_wide, field_sampler, uv).rgb;
    var light = scattered + params.halo_gain * halo;

    light = apply_overlay(light);
    light = apply_glass(light, in.uv);

    return vec4<f32>(tonemap(light * params.exposure), 1.0);
}
