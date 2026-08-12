# First Slice — Implementation Brief

**Goal:** a native Linux application proving the tube model end-to-end with
synthetic signals. No emulation of any kind. Everything here is bounded and
has no unknowns; anything not listed is out of scope for this slice.

Target dev machine: Debian, RTX 4070, Vulkan via wgpu. Must also run
acceptably on integrated graphics via the quality tier (drop deposit
supersample to 1×), but don't spend time on it.

---

## 1. Deliverables

1. Cargo workspace per ARCHITECTURE.md §7 (`beam-rasteriser` as an empty
   stub crate only).
2. `tube-shell`: winit window, wgpu surface (Vulkan primary), egui side
   panel, WGSL hot-reload (watch shader dir, recompile, keep last good on
   error, surface errors in-app).
3. `beam-trace`: types, ring buffer, file read/write per TRACE-FORMAT.md,
   validation, round-trip tests.
4. `beam-sources`:
   - **Synthetic generator**: produces the test patterns in §3 plus general
     Lissajous (freq ratio, phase, drive, speed all adjustable live).
     Emits adaptive samples honouring ε — even for synthetic paths, so the
     sampling contract is exercised from day one.
   - **XY audio player**: loads stereo WAV (44.1/48 kHz, 16-bit/float),
     L→x, R→y, drive = constant (slider) for stereo files; if a file is
     3-channel or a pair-plus-mono is supplied, third channel → drive.
     Playback clocked to audio out (cpal), samples emitted at audio rate
     (one trace sample per audio frame satisfies ε trivially).
5. `tube-renderer`: all passes in RENDERER.md §3 including the debug views
   (§5). Binned dispatch may be deferred; per-span dispatch is acceptable if
   it holds 60 fps at 500k samples/s on the 4070 (it will).
6. Record / replay: dump the live ring buffer to a .btr0 file; load a file
   and loop it. Replay is the shader-iteration workflow, so it must be
   solid: replay + hot-reload + debug views together.
7. Parameter panel: every table row from RENDERER.md §4 as a live control,
   grouped by class, with class visibly labelled and per-parameter reset.
   Save/load parameter sets as TOML (tube profiles are just named sets;
   ship "vectrex-default" and "neutral").
8. Screenshot key (PNG of final output) and a headless render mode: trace
   file + parameter file in → PNG out, for regression diffs in CI.

## 2. Explicitly out of scope

6809/VIA/anything Vectrex-executable; beam-rasteriser logic; AVG/Chill65
adapter; HDR output; overlays (the pass hook exists, feature is off); web
build; Windows build; gamepads.

## 3. Acceptance test patterns

Each pattern isolates parameters; each has a named acceptance criterion.
All are emitted by the synthetic generator and shipped as .btr0 fixtures.

| # | Pattern | What it isolates | Pass criterion |
|---|---|---|---|
| 1 | **Speed ramp**: the same line redrawn at 4 beam speeds (each ×4 apart), constant drive | dwell-time brightness | perceived brightness ordering strictly inverse to speed; slowest visibly widest (spot growth via energy is NOT expected — width constant here since drive constant; only brightness varies). No beading on the fastest line at any zoom. |
| 2 | **Parked dot series**: stationary dots at drive 0.25 / 1.0 / 4.0 | spot growth, saturation | dot radius increases with drive; the 4.0 dot's core is rolled off (not clipped white) with E_sat at default; lowering E_sat visibly softens it live. |
| 3 | **Square corner**: sharp 90° turn at high beam speed | (renderer honesty) | with synthetic (perfect) traces the corner is sharp; the vertex is brighter than the edges (beam direction change = local dwell). This is the baseline that layer-2 slew will later round. |
| 4 | **Flash-decay**: bright full-screen X drawn once, then nothing for 500 ms, looped | τf, τs, chromaticities | trail visibly two-phase (fast drop then long tail) and warms in hue as it fades; single-τ behaviour (adjust sliders to collapse them) looks visibly wrong by comparison. |
| 5 | **Blank taper**: line with drive stepping 1→0 mid-stroke over a few samples | drive interpolation, deposition correctness | intensity tapers along the stroke; no gap, no bright terminal dot. |
| 6 | **Refresh beat**: pattern redrawn at exactly 50 Hz | substep correctness | zero visible beat/shimmer on a 60 Hz display; field identical (headless diff) when rendered at simulated 60 vs 144 Hz host cadence. |
| 7 | **Lissajous torture**: high-frequency ratio, high speed, drive 2.0 | overall stability | no beading, no aliasing sparkle at 2× supersample, stable 60 fps, glow halo visible around dense crossings but history NOT dissolving (leave running 60 s; oldest excitation must be gone, recent structure crisp — proves glow isn't in the feedback loop). |
| 8 | **XY audio**: any oscilloscope-music WAV | end-to-end | renders recognisably; audio and image in sync (beam position lags audio out by < 30 ms). |

Headless mode + fixtures 1–7 form the initial regression suite: image diff
against blessed PNGs, tolerance small but non-zero (GPU variance).

## 4. Suggested build order

trace crate + file round-trip → shell with blank surface + hot-reload →
deposition of a hardcoded span (debug view only) → analytic span integral
vs point-splat comparison (keep the splat path behind a debug flag as the
"what beading looks like" reference) → decay + two buffers → readout +
glow → patterns 1–7 as generator programs → parameter panel → audio source
→ record/replay → headless + regression suite.

## 5. Notes for the implementer

- Keep the WGSL span-integral derivation as comments in the shader file —
  future porting to a compute-binned dispatch must not re-derive it.
- The ring buffer and the GPU upload must share layout: upload is a raw copy
  of the sample array slice(s) for the frame window, no transform.
- egui immediate-mode is fine; the panel is a dev tool, not a product UI.
- If wgpu validation errors on rgba32f storage use for `phosphor_slow` on
  some backend, fall back to ping-pong render targets for that buffer and
  note it; do not silently drop to 16f.
- Commit the blessed regression PNGs with the fixtures; renderer changes
  that alter them intentionally must re-bless in the same commit.
