// Accumulation with saturation, then decay — RENDERER.md §3.1 (accumulate)
// and §3.2 (decay), run once per substep in that order.
//
// Saturation happens AT ACCUMULATION, per texel, against the excitation
// already present. That is non-linear in local state, so it cannot be done at
// blend time with additive geometry — which is precisely why this is a field
// renderer and not a line renderer (RENDERER.md §3.1).
//
// Two buffers with two time constants produce the characteristic
// non-exponential aggregate tail. A single τ looks either smeary or flickery
// with nothing in between (RENDERER.md §3.2).
//
// ----------------------------------------------------------------------------
// Ping-pong, not in-place. Core WebGPU gives read-write storage textures only
// for r32uint/r32sint/r32float — rgba16float and rgba32float are write-only.
// The renderer is written against the WebGPU feature set (ARCHITECTURE.md §1),
// so rather than reach for a native-only extension, each substep reads the
// previous pair and writes the next. `phosphor_slow` stays rgba32f as
// specified: it is multiplied by a decay factor hundreds of times per second
// and 16f mantissa drift shows (RENDERER.md §1).
// ----------------------------------------------------------------------------

struct Params {
    resolution: vec2<u32>,
    // Knee of the hot-spot rolloff.
    e_sat: f32,
    // Share of deposited energy going to the fast component.
    fast_split: f32,
    // exp(-dt/τ) for one substep, precomputed on the CPU.
    decay_fast: f32,
    decay_slow: f32,
    // 0 when advancing time with nothing to deposit.
    deposit_gain: f32,
    _pad: f32,
}

@group(0) @binding(0) var deposit: texture_2d<f32>;
@group(0) @binding(1) var fast_in: texture_2d<f32>;
@group(0) @binding(2) var slow_in: texture_2d<f32>;
@group(0) @binding(3) var fast_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var slow_out: texture_storage_2d<rgba32float, write>;
@group(0) @binding(5) var<uniform> params: Params;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.resolution.x || gid.y >= params.resolution.y {
        return;
    }
    let at = vec2<i32>(gid.xy);

    let added = textureLoad(deposit, at, 0).rgb * params.deposit_gain;
    let fast = textureLoad(fast_in, at, 0).rgb;
    let slow = textureLoad(slow_in, at, 0).rgb;

    // Local excitation as a scalar: saturation is a property of the phosphor
    // at this point, not of one gun. Equal channels for a monochrome tube, so
    // this is exact there; a colour tube profile may want to revisit it.
    let excitation = fast + slow;
    let e = (excitation.r + excitation.g + excitation.b) / 3.0;
    let admitted = added / (1.0 + e / params.e_sat);

    let next_fast = (fast + admitted * params.fast_split) * params.decay_fast;
    let next_slow = (slow + admitted * (1.0 - params.fast_split)) * params.decay_slow;

    textureStore(fast_out, at, vec4<f32>(next_fast, 1.0));
    textureStore(slow_out, at, vec4<f32>(next_slow, 1.0));
}
