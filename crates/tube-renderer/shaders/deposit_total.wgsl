// Deposit-only accumulation for the decay-frozen debug view (RENDERER.md §5).
//
// Deliberately a separate pass rather than an extra output on phosphor.wgsl:
// the production accumulation maths stays exactly as specified, and this is
// plainly instrumentation. It sums every substep's deposition with no
// saturation and no decay, which is what "deposit-only, decay frozen" means —
// where did deposition put energy, before the phosphor did anything with it.

struct Resolution {
    size: vec2<u32>,
    _pad: vec2<u32>,
}

@group(0) @binding(0) var deposit: texture_2d<f32>;
@group(0) @binding(1) var total_in: texture_2d<f32>;
@group(0) @binding(2) var total_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var<uniform> resolution: Resolution;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= resolution.size.x || gid.y >= resolution.size.y {
        return;
    }
    let at = vec2<i32>(gid.xy);
    let total = textureLoad(total_in, at, 0).rgb + textureLoad(deposit, at, 0).rgb;
    textureStore(total_out, at, vec4<f32>(total, 1.0));
}
