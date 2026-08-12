// Readout: combine the two phosphor components into emitted light.
// RENDERER.md §3.3, first step.
//
//     out = chroma_fast × fast + chroma_slow × slow
//
// The two chromaticities differ — fast blue-ish, slow yellow-ish, for a white
// blend — so trails warm as they fade. That comes free from the split; nothing
// anywhere authors a colour ramp over time.
//
// This is a read of the phosphor state. Nothing downstream of here writes back
// into it (RENDERER.md §2).

struct Chroma {
    fast: vec4<f32>,
    slow: vec4<f32>,
}

@group(0) @binding(0) var fast: texture_2d<f32>;
@group(0) @binding(1) var slow: texture_2d<f32>;
@group(0) @binding(2) var<uniform> chroma: Chroma;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // Fullscreen triangle: (-1,-1), (-1,3), (3,-1).
    return vec4<f32>(
        f32(index / 2u) * 4.0 - 1.0,
        f32(index % 2u) * 4.0 - 1.0,
        0.0,
        1.0,
    );
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let at = vec2<i32>(position.xy);
    // textureLoad, not textureSample: phosphor_slow is rgba32f and float32
    // textures are not filterable in core WebGPU. One-to-one anyway.
    let f = textureLoad(fast, at, 0).rgb;
    let s = textureLoad(slow, at, 0).rgb;
    return vec4<f32>(chroma.fast.rgb * f + chroma.slow.rgb * s, 1.0);
}
