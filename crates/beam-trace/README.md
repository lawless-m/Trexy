# beam-trace

The `BTR0` beam trace: a description of **where a vector-CRT beam actually
went, and how hard it was driven**.

A trace is a time-ordered, piecewise-linear beam trajectory. It is not a
picture — it carries no resolution and no aspect ratio, only normalised
deflection, linear-light drive and time. What a display makes of that (spot
size, phosphor persistence, glow, geometry, tonemapping) belongs downstream,
which is what lets one trace be rendered by different tube models, replayed
from a file, or compared against real hardware.

```rust
use beam_trace::{Sample, TraceHeader, flags, write_file};

let samples = vec![
    Sample::blanked(0.0, 0.0, 0.000),           // beam parked at centre
    Sample::mono(-0.5, 0.25, 1.0, 0.001),       // lit, full drive
    Sample::mono(0.5, 0.25, 1.0, 0.002),        // a stroke across the tube
];

write_file("stroke.btr0", &TraceHeader {
    producer_id: "example/one-stroke".to_owned(),
    nominal_refresh_hz: 50.0,
    ..TraceHeader::default()
}, &samples)?;
# Ok::<(), beam_trace::TraceError>(())
```

## What is in the box

- **`Sample`** — the 32-byte record: `x`, `y`, three channels of drive, a
  timestamp, and a flag bit for a position discontinuity. It is `#[repr(C)]`
  and `Pod`, so a `&[Sample]` uploads to the GPU with no repacking; the same
  layout serves memory, disk and storage buffer.
- **`RingBuffer`** — single producer, single consumer, sized in time rather
  than samples. `spans_in` answers "everything needed to draw the interval
  I am about to render", including the sample before it, because a span that
  straddles the window still has to be drawn.
- **File I/O** — `write_file` / `read_file` for a 64-byte header and tightly
  packed records, little-endian throughout.
- **`validate`** — a trace that fails is rejected, never repaired. Timestamps
  must strictly increase, drive must be non-negative, floats must be finite.

## Conventions worth knowing before you write one

- Coordinates are normalised deflection, `x, y ∈ [−1, +1]` nominal, **y-up**,
  with `(0, 0)` the centre. Overscan past ±1 is legal and is the tube's
  problem, not the format's.
- Drive is **radiant drive in linear light**, unclamped. Zero is blanked, 1.0
  is nominal full, and overdrive above 1.0 is meaningful — saturation models
  want it. Monochrome producers write equal channels.
- **Blanked travel belongs in the trace.** A beam that moves without emitting
  still takes time doing it, and that time is phosphor decay. Omit it and a
  renderer cannot know the beam was ever elsewhere.
- Sampling is adaptive to a positional error bound, not a fixed rate. A
  digital vector generator's straight ramps need only endpoints; analogue
  slew curves need samples through the corners.

The normative specification is `TRACE-FORMAT.md` in the
[repository](https://github.com/lawless-m/Trexy).

## Licence

MIT OR Apache-2.0.
