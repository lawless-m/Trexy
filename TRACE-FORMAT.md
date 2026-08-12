# Beam Trace Format — Specification v0

The single interface between producers (layer 1+2) and the tube renderer
(layer 3). Same layout in memory and on disk. Little-endian throughout.

---

## 1. Semantics

A trace is a time-ordered sequence of samples describing the **actual beam
trajectory** — post-deflection-model where a deflection model exists — as a
piecewise-linear path with per-sample beam drive.

- The path is **continuous** between consecutive samples unless the later
  sample carries the DISCONTINUITY flag. The renderer interpolates position
  and beam drive linearly across each inter-sample span and deposits energy
  along it.
- **Blanked movement is included** as samples with zero beam drive. Producers
  must not omit beam travel merely because the beam is off.
- Beam drive is **radiant drive in linear light**, unclamped, three channels.
  Monochrome producers write equal channels (or a documented tube-white; the
  renderer's phosphor chromaticity handles colour, so equal channels is
  correct). Zero = blanked. Values are relative to a nominal full drive of
  1.0; overdrive above 1.0 is legal and meaningful (saturation model).
- Coordinates are **normalised deflection space**: x,y ∈ [−1, +1] nominal,
  y-up, (0,0) = screen centre = zeroed-integrator position. Values slightly
  outside ±1 are legal (overscan / mis-adjustment) and are the tube profile's
  problem. Aspect ratio is NOT encoded here.

## 2. Sample record — 32 bytes

| Offset | Size | Type | Field | Notes |
|---|---|---|---|---|
| 0 | 4 | f32 | x | normalised deflection |
| 4 | 4 | f32 | y | normalised deflection |
| 8 | 4 | f32 | drive_r | linear light, ≥ 0, unclamped |
| 12 | 4 | f32 | drive_g | |
| 16 | 4 | f32 | drive_b | |
| 20 | 4 | f32 | t | seconds since buffer epoch |
| 24 | 4 | u32 | flags | bit field, see §3 |
| 28 | 4 | u32 | reserved | must write 0; readers ignore |

32-byte stride: aligns for GPU storage-buffer array access with no repacking;
the CPU-side array **is** the upload source.

f32 `t` against an f64 epoch: f32 seconds holds ~microsecond resolution over
a few tens of seconds; the epoch is rebased per ring-buffer generation (and
per file), so precision never degrades over a session. Producers must emit
strictly increasing `t` within a buffer.

## 3. Flags

| Bit | Name | Meaning |
|---|---|---|
| 0 | DISCONTINUITY | This sample is NOT path-continuous with the previous one (e.g. Vectrex ZERO integrator dump). Renderer deposits nothing across the gap. |
| 1–31 | reserved | write 0 |

## 4. Sampling contract

Producers emit samples adaptively subject to:

- **Positional error bound ε:** the piecewise-linear path must deviate from
  the true (modelled) beam trajectory by at most ε, where ε defaults to
  1/4096 of full deflection (≈ one deposit-buffer texel at 2× supersampling).
  A digital vector generator's straight ramps need only endpoints; analogue
  slew curves need dense samples through corners and transitions.
- **Drive linearity:** beam drive is linearly interpolated between samples,
  so any non-linear drive transition (Z-axis rise/fall edge, shift-register
  chop) needs samples bracketing it tightly enough that linear interpolation
  is within ~1% of modelled drive.
- Samples SHOULD be sparse where nothing changes. There is no fixed rate and
  the renderer must not assume one.

## 5. Ring buffer (in memory)

Sized in **time**, not samples: capacity must cover the longest phosphor tail
plus margin (default 200 ms at an assumed worst-case density; implementation
picks a sample capacity accordingly and documents it). Overrun drops oldest
samples silently — by construction they are fully decayed. The renderer
queries "all samples in (T_last_rendered, T_now]"; it never blocks the
producer and never owns the time base. Single producer, single consumer.

## 6. File format (fixtures / record-replay)

Header, then raw records as §2, tightly packed.

| Offset | Size | Type | Field | Notes |
|---|---|---|---|---|
| 0 | 4 | bytes | magic | ASCII "BTR0" |
| 4 | 4 | u32 | version | 0 |
| 8 | 8 | f64 | epoch | seconds, producer-defined origin (may be 0) |
| 16 | 8 | u64 | sample_count | |
| 24 | 4 | f32 | epsilon | positional bound this trace honours |
| 28 | 4 | f32 | nominal_refresh_hz | informational; 0 = none/unknown |
| 32 | 32 | bytes | producer_id | UTF-8, NUL-padded (e.g. "synthetic/lissajous", "vectrex/via") |
| 64 | — | records | samples | sample_count × 32 bytes |

Validation on load: magic/version, monotonic t, non-negative drive, finite
floats. A trace that fails validation is rejected, not repaired.

## 7. Explicitly out of scope

Spot size, intensity-as-brightness, phosphor behaviour, geometry, colour
transforms: all tube-model concerns, computed downstream from position,
velocity (differenced from consecutive samples) and drive. The trace carries
physical beam state only — this is what lets tube sliders react live without
re-running any producer.
