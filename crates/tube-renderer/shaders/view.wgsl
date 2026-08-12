// Debug views — RENDERER.md §5. First-class, not afterthoughts: the
// development loop is trace replay plus shader hot-reload plus these.
//
// Read-only instrumentation. This pass samples buffers the rest of the chain
// produced and writes only to the output image; it never touches renderer
// state.

struct View {
    // 0 phosphor_fast, 1 phosphor_slow, 2 deposit-only, 3 false-colour energy.
    mode: u32,
    exposure: f32,
    // Decades of range covered by the false-colour ramp.
    decades: f32,
    _pad: f32,
}

@group(0) @binding(0) var fast: texture_2d<f32>;
@group(0) @binding(1) var slow: texture_2d<f32>;
@group(0) @binding(2) var deposit_total: texture_2d<f32>;
@group(0) @binding(3) var<uniform> view: View;

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

// Blue through green and yellow to red. Not perceptually uniform — it is a
// legibility aid for reading magnitudes off the screen, not a colour map for
// publication.
fn false_colour(t: f32) -> vec3<f32> {
    let x = clamp(t, 0.0, 1.0);
    let r = smoothstep(0.4, 0.8, x);
    let g = smoothstep(0.0, 0.4, x) - smoothstep(0.75, 1.0, x) * 0.6;
    let b = 1.0 - smoothstep(0.1, 0.5, x);
    return vec3<f32>(r, g, b);
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // The debug buffers are at deposit resolution and the output is at display
    // resolution, so scale the integer lookup accordingly.
    let size = vec2<f32>(textureDimensions(fast));
    let at = vec2<i32>(in.uv * size);

    let f = textureLoad(fast, at, 0).rgb;
    let s = textureLoad(slow, at, 0).rgb;

    var colour: vec3<f32>;
    switch view.mode {
        case 0u: {
            colour = f * view.exposure;
        }
        case 1u: {
            colour = s * view.exposure;
        }
        case 2u: {
            colour = textureLoad(deposit_total, at, 0).rgb * view.exposure;
        }
        default: {
            // Total excitation on a log scale: the field spans several decades
            // between a fresh stroke and the tail of one, and a linear ramp
            // shows only the brightest.
            let energy = f + s;
            let level = (energy.r + energy.g + energy.b) / 3.0;
            let decades = log2(max(level * view.exposure, 1e-12)) / 3.321928;
            colour = false_colour(1.0 + decades / view.decades);
            if level <= 0.0 {
                colour = vec3<f32>(0.0);
            }
        }
    }

    return vec4<f32>(colour, 1.0);
}
