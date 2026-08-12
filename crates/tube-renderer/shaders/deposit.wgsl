// Deposition — analytic integration of a Gaussian spot along a linear span.
// RENDERER.md §3.1. One dispatch per span over its bounding box.
//
// ============================================================================
// DERIVATION — keep this. A future port to a compute-binned dispatch changes
// the work decomposition, not the integral, and must not re-derive it.
// ============================================================================
//
// A span runs from p0 to p1 over wall-clock Δt. Parametrise by arclength
// ℓ ∈ [0, L] where L = |p1 − p0|, so the beam is at p(ℓ) = p0 + ℓ·u with
// u = (p1 − p0)/L, and |v| = L/Δt.
//
// The energy landing on a point q is the line integral of the spot kernel
// along the path, weighted by drive and dwell time (RENDERER.md §3.1):
//
//     E(q) = ∫ drive · K(|p − q|) dt
//          = (1/|v|) ∫₀^L drive(ℓ) · K(|p(ℓ) − q|) dℓ
//
// The 1/|v| is the whole point: a slow beam dwells longer and deposits more.
// Brightness falls out of speed rather than being authored.
//
// Resolve q into the span's frame. With n = perp(u):
//
//     a = (q − p0)·u        distance along the span axis
//     b = (q − p0)·n        perpendicular distance
//     |p(ℓ) − q|² = (ℓ − a)² + b²
//
// The kernel is an isotropic 2-D Gaussian, normalised to unit total energy:
//
//     K(r) = exp(−r² / 2σ²) / (2πσ²)
//
// The perpendicular term is constant in ℓ and factors straight out:
//
//     E(q) = (1/|v|) · exp(−b²/2σ²)/(2πσ²) · ∫₀^L drive(ℓ) exp(−(ℓ−a)²/2σ²) dℓ
//
// Drive is linear between samples — that is the trace's promise
// (TRACE-FORMAT.md §4), and it is what makes blanking tapers come out right —
// so write drive(ℓ) = d₀ + k·ℓ with k = (d₁ − d₀)/L. Substituting x = ℓ − a
// splits the remaining integral into two standard forms:
//
//     ∫₀^L (d₀ + kℓ) exp(−(ℓ−a)²/2σ²) dℓ
//         = (d₀ + k·a) · I₀  +  k · I₁
//
//     I₀ = ∫ exp(−x²/2σ²) dx  over x ∈ [−a, L−a]
//        = σ√(π/2) · [ erf((L−a)/(σ√2)) − erf(−a/(σ√2)) ]
//
//     I₁ = ∫ x·exp(−x²/2σ²) dx  over the same interval
//        = −σ² · [ exp(−(L−a)²/2σ²) − exp(−a²/2σ²) ]
//
// Both are closed form, which is why point splats are forbidden: a splat
// samples this integral at discrete points and beads on any stroke that moves
// further than σ between samples.
//
// The parked-beam case (L → 0) is the limit where the integral collapses to
// the kernel itself times the dwell time:
//
//     E(q) = Δt · drive · K(|q − p0|)
//
// handled as a separate branch below, since u is undefined at L = 0.
//
// ----------------------------------------------------------------------------
// Units. The integral is evaluated in FACE space: isotropic, one unit = one
// y-deflection unit, so the spot is round — which is the physical claim, the
// spot being round on the glass.
//
// Deflection space itself is NOT isotropic once a non-square tube aspect is
// applied (the Vectrex face is 3:4 portrait): a Gaussian round in deflection
// space would be an ellipse on the tube. Texel space is isotropic but its
// scale depends on the deposit resolution, which would make σ — quoted in
// deflection units (RENDERER.md §4) — resolution-dependent, and would push the
// deposited magnitudes down to ~1e-6, where rgba16f is already subnormal.
// Face space is isotropic AND resolution-independent, so σ needs no conversion
// and a drive-1.0 stroke lands near unity. Positions go deflection → texel
// (which is where aspect is applied) → face by dividing through by scale_y.
// ============================================================================

const PI: f32 = 3.14159265358979;
const SQRT_PI_OVER_2: f32 = 1.25331413731550;
const SQRT_2: f32 = 1.41421356237310;

// Gaussian support cutoff, in σ. Beyond 4σ the kernel is below 3e-4 of peak.
const CUTOFF_SIGMAS: f32 = 4.0;

// TRACE-FORMAT.md §3.
const DISCONTINUITY: u32 = 1u;

// TRACE-FORMAT.md §2, byte for byte. Scalar fields only: a vec3<f32> would be
// 16-byte aligned in WGSL and silently break the 32-byte stride that lets the
// CPU sample array be the upload source unchanged.
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
    // Texels per deflection unit. These differ when the tube is not square.
    scale_x: f32,
    scale_y: f32,
    sigma0: f32,
    sigma1: f32,
    gamma_s: f32,
    _pad: f32,
}

struct SpanDispatch {
    // Index of the span's first sample; the second is `first + 1`.
    first: u32,
    origin_x: u32,
    origin_y: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> samples: array<Sample>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var<uniform> span: SpanDispatch;
// Accumulation needs read-modify-write, which core WebGPU does not offer for
// float storage textures. Dispatches within a compute pass are ordered, and
// within one dispatch each texel is touched by exactly one invocation, so a
// plain storage buffer accumulates safely. A resolve pass copies it into the
// rgba16f `deposit_scratch` texture.
@group(0) @binding(3) var<storage, read_write> accumulator: array<vec4<f32>>;

// Abramowitz & Stegun 7.1.26. Maximum absolute error 1.5e-7, which is well
// under half-float precision at the far end of this pipeline.
fn erf(x: f32) -> f32 {
    let a = abs(x);
    let t = 1.0 / (1.0 + 0.3275911 * a);
    let poly = t * (0.254829592
        + t * (-0.284496736
        + t * (1.421413741
        + t * (-1.453152027
        + t * 1.061405429))));
    return sign(x) * (1.0 - poly * exp(-a * a));
}

// Spot size grows and defocuses with beam current: bright means fatter, not
// just brighter (RENDERER.md §3.1).
fn spot_sigma(drive: f32) -> f32 {
    return params.sigma0 + params.sigma1 * pow(max(drive, 0.0), params.gamma_s);
}

// Deflection space (−1..+1, y-up) to texel space (y-down, origin top-left).
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

    // A discontinuity means the beam did not travel this path at all — it was
    // dumped elsewhere — so nothing is deposited across the gap.
    if (s1.flags & DISCONTINUITY) != 0u {
        return;
    }

    let d0 = vec3<f32>(s0.drive_r, s0.drive_g, s0.drive_b);
    let d1 = vec3<f32>(s1.drive_r, s1.drive_g, s1.drive_b);
    // The beam still moved, it simply emitted nothing.
    if all(d0 <= vec3<f32>(0.0)) && all(d1 <= vec3<f32>(0.0)) {
        return;
    }

    let dt = s1.t - s0.t;
    if dt <= 0.0 {
        return;
    }

    // Texel space applies the tube aspect; dividing by scale_y then puts
    // everything in isotropic face units. See the units note above.
    let to_face = 1.0 / params.scale_y;
    let p0 = to_texels(vec2<f32>(s0.x, s0.y)) * to_face;
    let p1 = to_texels(vec2<f32>(s1.x, s1.y)) * to_face;
    let q = (vec2<f32>(f32(texel.x) + 0.5, f32(texel.y) + 0.5)) * to_face;

    // One σ per span, taken at the span's mean drive. σ genuinely varies along
    // the span, but the trace's ε bound keeps spans short enough that the
    // variation is small, and a varying σ has no closed form (RENDERER.md
    // §3.1: "linearise per span").
    let sigma = spot_sigma(0.5 * (mean_drive(s0) + mean_drive(s1)));
    let two_sigma_sq = 2.0 * sigma * sigma;

    let delta = p1 - p0;
    let length_face = length(delta);

    var energy: vec3<f32>;

    if length_face < 1e-9 {
        // Parked beam: the integral degenerates to dwell time × kernel.
        let r = q - p0;
        let kernel = exp(-dot(r, r) / two_sigma_sq) / (2.0 * PI * sigma * sigma);
        energy = 0.5 * (d0 + d1) * dt * kernel;
    } else {
        let u = delta / length_face;
        let n = vec2<f32>(-u.y, u.x);
        let rel = q - p0;
        let a = dot(rel, u);
        let b = dot(rel, n);

        let far = length_face - a;
        let scale = sigma * SQRT_2;
        let i0 = sigma * SQRT_PI_OVER_2 * (erf(far / scale) - erf(-a / scale));
        let i1 = -sigma * sigma
            * (exp(-far * far / two_sigma_sq) - exp(-a * a / two_sigma_sq));

        // drive(ℓ) = d0 + k·ℓ
        let k = (d1 - d0) / length_face;
        let weighted = (d0 + k * a) * i0 + k * i1;

        let across = exp(-b * b / two_sigma_sq) / (2.0 * PI * sigma * sigma);
        // 1/|v| = Δt/L
        energy = weighted * across * (dt / length_face);
    }

    let index = texel.y * params.resolution.x + texel.x;
    accumulator[index] += vec4<f32>(max(energy, vec3<f32>(0.0)), 0.0);
}
