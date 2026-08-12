// Shell background.
//
// The first slice's real passes arrive with the deposition shader; until then
// this flat field is the subject that proves the hot-reload path end to end.
// Edit the colour below with the shell running and it takes effect on save.

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // Fullscreen triangle: (-1,-1), (-1,3), (3,-1).
    let x = f32(index / 2u) * 4.0 - 1.0;
    let y = f32(index % 2u) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.02, 0.02, 0.03, 1.0);
}
