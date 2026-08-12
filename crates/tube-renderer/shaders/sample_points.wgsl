// Sample-point overlay — RENDERER.md §5.
//
// A dot at every trace sample, drawn over the beauty render. This is the view
// that makes the adaptive sampling contract visible: where the producer chose
// to spend samples, and whether anything beads between them
// (TRACE-FORMAT.md §4).
//
// One instance per sample, each a small screen-space quad.

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

struct Overlay {
    // Half-size of a dot, in normalised device coordinates.
    radius: vec2<f32>,
    aspect: f32,
    pincushion: f32,
    rotation: f32,
    overscan: f32,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var<storage, read> samples: array<Sample>;
@group(0) @binding(1) var<uniform> overlay: Overlay;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) offset: vec2<f32>,
    @location(1) blanked: f32,
}

/// Place a point on the face where the tonemap pass would have found it.
///
/// tonemap.wgsl maps output to source with r → r(1 + k·r²); this is the
/// inverse, which has no closed form, so it is solved by fixed-point iteration.
/// Three rounds are ample at the coefficients a tube profile uses.
fn undistort(p: vec2<f32>) -> vec2<f32> {
    var q = vec2<f32>(p.x * overlay.aspect, p.y);
    let wanted = q;
    for (var i = 0; i < 3; i++) {
        q = wanted / (1.0 + overlay.pincushion * dot(q, q));
    }
    q *= overlay.overscan;

    let c = cos(-overlay.rotation);
    let s = sin(-overlay.rotation);
    q = vec2<f32>(q.x * c - q.y * s, q.x * s + q.y * c);

    q.x /= overlay.aspect;
    return q;
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex: u32,
    @builtin(instance_index) instance: u32,
) -> VertexOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let corner = corners[vertex];
    let s = samples[instance];
    let at = undistort(vec2<f32>(s.x, s.y));

    var out: VertexOut;
    out.position = vec4<f32>(at + corner * overlay.radius, 0.0, 1.0);
    out.offset = corner;
    out.blanked = select(0.0, 1.0, s.drive_r + s.drive_g + s.drive_b <= 0.0);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    if dot(in.offset, in.offset) > 1.0 {
        discard;
    }
    // Blanked samples in a colder tint: travel the beam made with the gun off
    // is present in the trace and worth being able to see.
    let lit = vec3<f32>(1.0, 0.35, 0.1);
    let dark = vec3<f32>(0.1, 0.45, 1.0);
    return vec4<f32>(mix(lit, dark, in.blanked), 1.0);
}
