# Vector Tube Renderer — Document Bundle

Design documents for a physically-modelled CRT vector display renderer.
Rust, native Linux (winit + wgpu/Vulkan), WGSL, designed against the WebGPU
feature set. The Vectrex is the calibration target; the renderer itself is
machine-agnostic and will later serve Atari colour vector titles (Space
Duel via the Chill65 project, eventually Star Wars).

## Where to start

Read in this order:

1. **ARCHITECTURE.md** — what the project is and is not, the three-layer
   design (signal source → beam rasteriser → tube model), interface
   rationale, parameter-provenance policy, machine notes, roadmap, proposed
   crate layout.
2. **TRACE-FORMAT.md** — the byte-level specification of the beam trace,
   the single interface between producers and the renderer. Normative.
3. **RENDERER.md** — the tube model: retained state, frame loop, every
   pass, the full parameter table with provenance classes, debug views.
4. **FIRST-SLICE.md** — the implementation brief to build now. Deliverables,
   scope exclusions, eight acceptance test patterns with pass criteria, and
   a suggested build order. **This is the actionable document.**

## Contents

| File | Purpose |
|---|---|
| CONTENTS.md | this file |
| ARCHITECTURE.md | project overview and design decisions |
| TRACE-FORMAT.md | beam trace spec v0 (normative) |
| RENDERER.md | tube-model pass spec v0 and parameter table |
| FIRST-SLICE.md | implementation brief and acceptance criteria |

## Ground rules carried through all documents

- Field model, not geometry: phosphor state lives in pixel buffers;
  geometry exists only as the integration domain during deposition.
- Analytic span integration; point splats are forbidden (beading).
- Glow is read-out only, never fed back into phosphor state.
- All maths in linear light; tonemap once at the end.
- Every parameter carries a provenance class (datasheet / schematic /
  fitted / artistic); this classification is part of the public docs.
- No emulation in the first slice: synthetic and XY-audio sources only.
