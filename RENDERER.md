# Tube Renderer — Pass Specification v0

Layer 3. Consumes beam traces (TRACE-FORMAT.md), owns all tube physics.
wgpu, WGSL, compute-based deposition (WebGPU feature set; no WebGL2 path).
All internal maths in linear light; tonemap exactly once, at the end.

---

## 1. Retained state

The renderer's only frame-to-frame state is the phosphor field:

| Texture | Format | Resolution | Contents |
|---|---|---|---|
| `phosphor_fast` | rgba16f | deposit res | fast-decay component excitation |
| `phosphor_slow` | rgba32f | deposit res | slow-decay component excitation |
| transient: `deposit_scratch`, `readout`, glow mip chain | rgba16f | various | no state carried |

`phosphor_slow` is 32f because it is multiplied by a per-substep decay factor
across hundreds of frames; 16f mantissa drift is audible there. Revisit if
memory ever matters (it won't on the target machines).

**Deposit resolution:** 2× the display resolution linearly, aspect from the
**tube profile** (Vectrex is portrait 3:4-ish; Atari monitors landscape), not
from the window. The field is bandlimited by spot size but thin bright
strokes alias at 1:1.

## 2. Frame loop

Per host frame, regardless of host refresh rate:

1. Pull new samples from the ring buffer for the wall-clock interval.
2. Split the interval into **fixed substeps** (default 1.25 ms → 16 substeps
   at 50 Hz content, ~13 at 60 Hz host; constant substep *duration*, so 60 Hz
   and 144 Hz hosts produce identical fields — this also kills 50-vs-60 beat
   artefacts).
3. Per substep: **deposit** that substep's trace spans, then **decay** both
   phosphor buffers by that substep's factors. Interleaving within the frame
   preserves intra-frame ordering (start vs end of a sweep differ by real
   decay, as on hardware).
4. **Readout**: combine fast+slow with their chromaticities → apply optical
   glow to a **copy** → geometry/glass → overlay → tonemap → present.

Hard rule: glow is a read-out operation only. It must NEVER feed back into
the phosphor buffers; blur-in-the-loop dissolves history into grey mush
within a second and is the classic failure mode of this renderer type.

## 3. Passes

### 3.1 Deposition (compute)

Physical quantity per texel: line integral along the beam path of the spot
kernel, weighted by drive and dwell time — energy ∝ drive × ∫kernel ds /
|v|, which is what makes brightness fall out of beam speed (slow beam =
bright, the single most important effect in the whole renderer).

- Per inter-sample span (skip spans following a DISCONTINUITY sample, skip
  spans with both endpoint drives zero — but note zero-drive spans still
  advanced the beam; they simply deposit nothing).
- **Analytic integration, not point splats.** The integral of a Gaussian spot
  along a linear span has a closed form (erf terms along the axis, Gaussian
  across it). Point splats bead on fast strokes; forbidden.
- Spot kernel: isotropic Gaussian, σ = σ0 + σ1 · drive^γs (spot grows and
  defocuses with beam current — bright means *fatter*, not just brighter).
  Drive and σ vary along the span; linearise per span (bound span length via
  the trace ε so this is safe).
- Dispatch: spans → per-tile bins (screen-space tiles, e.g. 16×16), then one
  workgroup per occupied tile integrates its spans into `deposit_scratch`.
  Naive alternative for slice one: one dispatch per span over its bounding
  box; correctness first, binning when profiling says so.
- Accumulate scratch into phosphor buffers through the **saturation** stage:
  excitation added is scaled by a saturating function of current local
  excitation (default: added × 1/(1 + E/E_sat)). Saturation must happen at
  accumulation, per texel — it is non-linear in local state and cannot be
  done at blend time with additive geometry, which is precisely why this is a
  field renderer. Split deposited energy between fast and slow components by
  the phosphor mix ratio.

### 3.2 Decay (compute or full-screen pass)

Per substep: multiply each buffer by exp(−dt/τ). Two buffers, two τ. This is
what produces the characteristic non-exponential aggregate tail; a single
time constant looks either smeary or flickery with no middle ground.

### 3.3 Readout & optics

- Combine: out = chroma_fast × fast + chroma_slow × slow. The two
  chromaticities differ (fast component blue-ish, slow yellow-ish for a
  white blend), so trails **warm as they fade** — free, from the split.
- **Optical glow** (faceplate/glass scatter, distinct phenomenon from spot
  size and applied after emission, never merged with it): multi-scale — a
  tight Gaussian plus a wide low-amplitude halo via a small mip/blur chain.
  CRT halo is genuinely long-range; a single tight blur reads as "neon".
- **Geometry**: tube-profile pincushion/barrel + corner terms, rotation,
  overscan; sample the readout through the distortion. (Vectrex profile:
  slight pincushion, portrait.)
- **Overlay** (optional, per game): scanned translucent overlay multiplied
  over the image, plus a subtle unlit-overlay ambient term.
- **Glass**: vignette, faint specular/room reflection (artistic class,
  default subtle, off switch).
- **Tonemap**: exposure then a filmic-ish curve; operator choice deferred —
  parameterised so it can be swapped. HDR output path (Wayland) is future
  work; design keeps everything linear until this pass precisely so HDR is a
  tonemap swap.

## 4. Parameter table

Classes per ARCHITECTURE.md §4: datasheet / schematic / fitted / artistic.
Fitted defaults are starting guesses to be tuned against test patterns.

| Parameter | Symbol | Default | Class | Notes |
|---|---|---|---|---|
| Deposit supersample | — | 2× | — | quality tier |
| Substep duration | dt | 1.25 ms | — | fixed |
| Spot base sigma | σ0 | 0.0015 (norm. units) | fitted | parked dim dot width |
| Spot growth coeff | σ1 | 0.0025 | fitted | defocus with drive |
| Spot growth exponent | γs | 0.7 | fitted | |
| Saturation level | E_sat | 4.0 | fitted | knee of hot-spot rolloff |
| Fast decay τ | τf | 120 µs | fitted | see phosphor note |
| Slow decay τ | τs | 40 ms | fitted | see phosphor note |
| Fast/slow energy split | — | 0.75 / 0.25 | fitted | |
| Fast chromaticity | — | (0.85, 0.95, 1.0) | fitted | blue-ish |
| Slow chromaticity | — | (1.0, 0.92, 0.70) | fitted | yellow-ish |
| Glow tight sigma | — | 0.004 | fitted | faceplate scatter |
| Glow halo sigma / gain | — | 0.06 / 0.08 | artistic | long-range haze |
| Pincushion coeff | — | 0.02 | fitted | Vectrex profile |
| Tube aspect | — | 3:4 portrait | schematic | Vectrex profile |
| Exposure | — | 1.0 | artistic | |
| Vignette / reflection gains | — | subtle | artistic | |

Phosphor note: the Vectrex tube (Samsung 9" B/W) phosphor type is not stated
in the service manual; standard white TV phosphor (P4-family) is the working
assumption — a zinc-sulfide blend whose components decay at different rates,
which is the physical basis for the two-buffer model. τ values above are
order-of-magnitude placeholders, **fitted** class, to be tuned against
reference footage and, eventually, community test-card photographs.

Layer-2 constants (recorded here for later, not used in slice one, all to be
verified against the actual datasheets/service-manual scan during phase 2):
LF353/LF347 slew rate and GBW (datasheet); MC1408 settling/glitch
(datasheet); CD4052 on-resistance and switch time (datasheet); DAC reference
swing ±2.5 V (Programmer's Manual); integrator RC, deflection gains, S/H hold
capacitors (schematic — beware the manual's known component errata; trust
topology, verify values).

## 5. Debug views

First-class, not afterthoughts — the development loop is trace replay +
shader hot-reload + these views: raw `phosphor_fast` / `phosphor_slow`
(exposure-scaled), deposit-only (decay frozen), false-colour energy,
sample-point overlay (dots at trace samples over the beauty render — makes
the adaptive sampling and any beading instantly visible), and per-pass
timings.
