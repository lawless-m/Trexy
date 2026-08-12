# Vector Tube Renderer — Architecture Overview

**Status:** agreed design, pre-implementation.
**Language:** Rust. **Platform:** native Linux first (X11/Wayland via winit), GPU via wgpu.
**Working name:** none yet; crate names below are placeholders.

---

## 1. What this project is

A physically-modelled renderer for electrostatic/magnetic-deflection CRT vector
displays, built as a standalone, parameterised component with pluggable signal
sources. The Vectrex is the named calibration target from day one, but the
renderer is machine-agnostic: the same tube model must serve the Vectrex's 9"
Samsung tube and the Atari colour XY monitors (Wells-Gardner 6100, Amplifone)
used by Space Duel / Star Wars, differing only in parameter profiles.

The renderer is the deliverable. Emulation cores (Vectrex 6809/VIA, Atari AVG)
are *producers* that arrive later and bolt on. A sibling project (Chill65,
currently implementing Space Duel) is the first expected external consumer.

### What it is not

- Not an SVG/segment renderer. The display is modelled as a phosphor **field**
  (pixel arrays with energy deposition, spread, saturation and decay), because
  saturation, persistence and two-rate decay are per-texel state and cannot be
  expressed with additive geometry. Geometry survives only as the integration
  domain inside the deposition step.
- Not a Vectrex emulator (yet). No 6809, no VIA, no BIOS in the first slice.
- Not web-first. Designed against WebGPU's feature set so a browser build stays
  a build-target flip, but native is the platform. WebGL2 is explicitly
  unsupported (no compute pipeline). Note: as of mid-2026 WebGPU on Linux is
  still flag-gated in Chromium and absent in Firefox, so the web build has no
  urgency for the realistic audience.

---

## 2. The three layers

### Layer 1 — Signal source

Emits *commanded* beam behaviour over time. Implementations, in build order:

1. **Synthetic generator** — Lissajous figures, test patterns, scripted
   sequences. First slice.
2. **XY audio player** — stereo WAV, left channel → X, right → Y, amplitude
   or a third channel → Z. Exercises the renderer harder than any game
   (oscilloscope-music content). First slice.
3. **Vectrex core** — 6809 + 6522 VIA + MC1408 DAC + CD4052 mux. Later.
4. **Chill65 / Atari AVG** — display-list processor. External; adapter only.

### Layer 2 — Beam rasteriser (deflection model)

A **stateful streaming filter** that converts machine drive events into an
actual beam trajectory. Models the analogue chain: op-amp slew limiting and
settling, integrator behaviour and drift (Vectrex), sample-and-hold droop
(Vectrex Y and Z paths — note X is driven directly, so X and Y are
asymmetric), DAC settling glitch, Z-axis (blanking) rise/fall.

Key properties:

- Runs **producer-side**, on the machine's own timebase. The trace that
  crosses the interface records where the beam *was*, not what was commanded.
- Shipped as a shared library component instantiated with per-machine
  constants (Vectrex gets LF353/MC1408/CD4052 numbers off the datasheets; an
  Atari profile gets different ones). No machine reimplements a slew limiter.
- It is a filter over an event stream, **not** a per-command function: state
  (amplifier slew state, integrator charge, S/H voltages) persists across
  commands, and commands can be interrupted mid-flight (DAC rewritten while
  RAMP is asserted → curved stroke; shift register chopping BLANK mid-stroke
  → dashes). It must handle events arriving inside a ramp.
- Event vocabulary is deliberately tiny and machine-neutral:
  - beam drive rate changed to (rx, ry) at time t
  - beam current changed to (r, g, b) at time t
  - position discontinuity (e.g. Vectrex ZERO integrator dump) at time t
- Output sampling is **adaptive to a positional error bound** (see
  TRACE-FORMAT.md §4), with analytic fast-forward through steady ramps
  (the slew-limited response to a constant input is closed-form; numerical
  stepping is only needed around events and high curvature).

Layer 2 does not exist in the first slice — synthetic sources write traces
directly — but the trace contract is written for it now so nothing changes
when it arrives.

### Layer 3 — Tube model (the renderer)

Consumes beam traces, owns all per-tube physics: spot size vs beam current,
energy-deposition rasterisation, phosphor saturation, two-rate persistence
with decay colour shift, optical glow, geometry (aspect, pincushion,
rotation), glass, tonemap. Fully specified in RENDERER.md.

The renderer's entire input is the trace and its entire retained state is a
handful of textures — exactly mirroring the real device, whose only memory is
the phosphor itself.

---

## 3. The interface between layers 2 and 3

A flat sample array in linear memory (zero-copy to GPU), byte-specified in
TRACE-FORMAT.md. Design points already settled:

- **Continuous polyline, not segments.** Blanked travel is present as samples
  with zero beam current, because it determines amplifier state entering the
  next visible stroke and because blanking transitions (tapers, spurs) are
  visible physics. True discontinuities (integrator dump) are a flag bit.
- **Beam drive is three-channel, unclamped, linear light.** The Vectrex is
  monochrome (colour comes from plastic overlays) but Space Duel and Star
  Wars are colour vector games; a scalar here would be a painful retrofit.
- **Normalised deflection space**, −1..+1 both axes, y-up. Aspect ratio and
  geometry are tube-profile properties, not producer properties (the Vectrex
  tube is portrait; Atari monitors are landscape — same trace space).
- **Absolute f32 timestamps against a per-buffer f64 epoch.**
- **Time-sized ring buffer** (cover longest phosphor tail + margin, ~200 ms).
  Renderer asks for state at wall-clock T; it never owns the clock.
- On-disk fixture format = in-memory layout + small header, so capturing a
  regression trace is a memcpy and a write.

Rationale for producer-side deflection modelling: the analogue chain is
machine hardware, it interacts with the emulated clock, and the interface then
has one unambiguous meaning. Cost accepted: tuning slew constants means
re-running the producer (traces are cheap to recapture).

---

## 4. Parameter provenance policy

Every parameter is classified, and the classification is part of the public
documentation:

| Class | Meaning | Examples |
|---|---|---|
| **datasheet** | Constant with a citation; not user-adjustable in normal UI | LF353 slew rate, MC1408 settling, integrator RC |
| **schematic** | Derived from the service manual circuit; cite page. Manual is authoritative on topology but has known component errata — verify values | Deflection gain, DAC reference swing (±2.5 V) |
| **fitted** | Tube property with no paper source; exposed slider, documented default, honest about being fitted by eye/footage | Phosphor decay constants, spot size vs current, saturation knee |
| **artistic** | Deliberate taste on top of the physical model; clearly separated | Extra glow gain, overlay tint strength |

This split *is* the accuracy claim: anyone auditing the renderer can see which
numbers are physics and which are taste.

Calibration path (no hardware purchase planned): datasheets + published
phosphor decay data + reference footage now; a 6809 test-card ROM later whose
patterns isolate one parameter each, publishable so community members with
real units can photograph results at known exposure and the fitted class
shrinks over time.

---

## 5. Machine notes (for later layers, recorded now)

**Vectrex.** No display processor. The 6809 bit-bangs the VIA: 8-bit DAC
(MC1408, ±2.5 V) drives X directly and, via CD4052 mux, the Y and Z
sample-and-holds and an active-ground reference S/H. Lines are turtle-style:
signed (dy,dx) rate pair × VIA T1 duration = length; absolute positioning is
only trustworthy after a ZERO recal (integrator drift), hence the software
frame convention (Wait_Recal, ~50 Hz). The 6522 shift register can gate BLANK
at bit rate → dashes and crude raster strips, so intensity varies *along*
strokes; this falls out of the polyline-with-current trace for free. Games
bypass the BIOS freely — there is no valid interception point above the VIA.
VIA/T1 timing accuracy is the whole ballgame for the eventual core; the CPU is
comparatively forgiving (TomHarte single-step JSON tests exist for the 6809).
BIOS is copyrighted: user supplies it.

**Atari AVG (Chill65).** Genuine display-list processor: digital position/rate
counters, per-vector intensity and colour, subroutines. Layer 1/2 differ
almost entirely from the Vectrex; layer 3 is shared — that sharing is the test
that the abstraction is real. Chill65's current segment renderer keeps
shipping; an adapter (segment list → polyline with synthesised timestamps from
known AVG timing) makes the swap a one-file change on its side, done whenever
convenient.

**Star Wars (future).** 6809 (not 6502) + Mathbox bit-slice coprocessor. A
Vectrex-grade 6809 core is directly reusable; the Mathbox is the genuinely new
work. Dependency graph: renderer → Space Duel display upgrade → Vectrex (adds
6809 + analogue deflection) → Star Wars (adds Mathbox only).

---

## 6. Roadmap

1. **First slice** (FIRST-SLICE.md): native shell, synthetic + XY-audio
   sources, full tube model, live sliders, trace record/replay. No emulation.
2. Beam rasteriser library with Vectrex constants; drive it from scripted
   VIA-level event sequences (still no CPU) to validate slew/S-H behaviour.
3. Chill65 adapter (whenever Chill65 wants it).
4. Vectrex core: 6809 (validated against single-step suite) + VIA + analogue
   front end; MineStorm from BIOS is the integration test. Test-card ROM.
5. Tube profiles: WG6100 / Amplifone; HDR output on Wayland.
6. Optional: wasm/WebGPU build when Linux browser support makes it worthwhile.

---

## 7. Crate layout (proposed)

| Crate | Contents | Depends on |
|---|---|---|
| `beam-trace` | Trace types, byte layout, ring buffer, file I/O, validation | — |
| `tube-renderer` | Layer 3: wgpu pipelines, WGSL shaders, tube profiles, parameter registry | `beam-trace`, wgpu |
| `beam-sources` | Synthetic generator, XY-audio player | `beam-trace` |
| `beam-rasteriser` | Layer 2 shared filter (phase 2, stub now) | `beam-trace` |
| `tube-shell` | winit app: window, UI (egui), source selection, record/replay | all of the above |

Workspace-level decisions: shaders in WGSL only (native + WebGPU identical);
no WebGL2 fallback path anywhere; hot-reload for WGSL in the shell from day
one (the renderer is developed by iterating shaders against replayed traces).
