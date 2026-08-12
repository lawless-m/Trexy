// Copy the deposition accumulator into `deposit_scratch` (rgba16f).
//
// The accumulator exists only because core WebGPU has no read-modify-write for
// float storage textures; see the note in deposit.wgsl. This pass is where the
// buffer becomes the texture the rest of the chain samples.

struct Resolution {
    size: vec2<u32>,
    _pad: vec2<u32>,
}

@group(0) @binding(0) var<storage, read> accumulator: array<vec4<f32>>;
@group(0) @binding(1) var<uniform> resolution: Resolution;
@group(0) @binding(2) var scratch: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= resolution.size.x || gid.y >= resolution.size.y {
        return;
    }
    let index = gid.y * resolution.size.x + gid.x;
    textureStore(scratch, vec2<i32>(gid.xy), accumulator[index]);
}
