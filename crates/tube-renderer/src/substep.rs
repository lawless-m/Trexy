//! The fixed substep clock and span clipping — RENDERER.md §2.

use beam_trace::{Sample, flags};

/// Substep duration. Constant *duration*, not a fraction of a frame: that is
/// what makes a 60 Hz and a 144 Hz host produce identical fields, and what
/// kills the 50-vs-60 beat (RENDERER.md §2, §4).
pub const SUBSTEP_SECONDS: f64 = 0.001_25;

/// One substep's half-open time window, in seconds since the buffer epoch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Substep {
    pub start: f32,
    pub end: f32,
}

/// Hands out whole substeps on a fixed grid anchored at an origin.
///
/// Boundaries are always `origin + k·dt`, and a substep is always exactly `dt`
/// long. Time left over at the end of a frame is not simulated early; it waits
/// for the next frame. That is the whole trick: how the wall clock is chopped
/// into frames cannot change which substeps run or how long they are, so two
/// hosts at different refresh rates walk through exactly the same sequence.
#[derive(Debug)]
pub struct SubstepClock {
    origin: f64,
    dt: f64,
    /// Counted rather than accumulated, so the grid never drifts.
    steps: u64,
}

impl SubstepClock {
    pub fn new(origin: f64) -> Self {
        Self::with_dt(origin, SUBSTEP_SECONDS)
    }

    pub fn with_dt(origin: f64, dt: f64) -> Self {
        assert!(dt > 0.0, "substep duration must be positive");
        Self {
            origin,
            dt,
            steps: 0,
        }
    }

    pub fn dt(&self) -> f64 {
        self.dt
    }

    /// Absolute time simulated so far.
    pub fn simulated(&self) -> f64 {
        self.origin + self.steps as f64 * self.dt
    }

    /// Every whole substep that completes at or before `now`.
    pub fn advance(&mut self, now: f64) -> Vec<Substep> {
        let mut out = Vec::new();
        while self.origin + (self.steps + 1) as f64 * self.dt <= now {
            let start = self.origin + self.steps as f64 * self.dt;
            self.steps += 1;
            out.push(Substep {
                start: start as f32,
                end: (start + self.dt) as f32,
            });
        }
        out
    }
}

/// The portion of `samples` that falls inside `[start, end)`, as spans clipped
/// to the window.
///
/// Spans routinely straddle substep boundaries — a straight ramp needs only
/// its endpoints (TRACE-FORMAT.md §4), so a single span can cover many
/// substeps. Depositing such a span whole into the substep it starts in would
/// decay all of its energy as though the beam drew it instantly, which shows
/// up directly in the flash-decay and speed-ramp patterns. So endpoints are
/// interpolated at the boundary instead.
///
/// The result is a flat list of two-sample pairs. Every pair after the first
/// starts with a DISCONTINUITY flag, so the renderer does not draw a phantom
/// span from the end of one pair to the start of the next.
pub fn clip_spans(samples: &[Sample], start: f32, end: f32) -> Vec<Sample> {
    if samples.len() < 2 || end <= start {
        return Vec::new();
    }

    // Samples are strictly time-ordered, so skip straight to the first span
    // that can overlap the window.
    let first = samples.partition_point(|s| s.t <= start).saturating_sub(1);

    let mut out: Vec<Sample> = Vec::new();
    for index in first..samples.len() - 1 {
        let (s0, s1) = (samples[index], samples[index + 1]);
        if s0.t >= end {
            break;
        }
        if s1.t <= start || s1.flags & flags::DISCONTINUITY != 0 {
            continue;
        }

        let from = s0.t.max(start);
        let to = s1.t.min(end);
        if to <= from {
            continue;
        }

        let mut a = lerp_sample(s0, s1, from);
        let b = lerp_sample(s0, s1, to);
        if !out.is_empty() {
            a.flags |= flags::DISCONTINUITY;
        }
        out.push(a);
        out.push(b);
    }
    out
}

/// Position and drive interpolate linearly between samples; that is the
/// trace's promise (TRACE-FORMAT.md §1, §4).
fn lerp_sample(s0: Sample, s1: Sample, t: f32) -> Sample {
    let span = s1.t - s0.t;
    let f = if span > 0.0 { (t - s0.t) / span } else { 0.0 };
    let mix = |a: f32, b: f32| a + (b - a) * f;
    Sample {
        x: mix(s0.x, s1.x),
        y: mix(s0.y, s1.y),
        drive_r: mix(s0.drive_r, s1.drive_r),
        drive_g: mix(s0.drive_g, s1.drive_g),
        drive_b: mix(s0.drive_b, s1.drive_b),
        t,
        flags: 0,
        reserved: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk one second of wall clock in frames of `hz`, collecting substeps.
    fn walk(hz: f64) -> Vec<Substep> {
        let mut clock = SubstepClock::new(0.0);
        let mut out = Vec::new();
        let frames = hz.round() as u64;
        for frame in 1..=frames {
            out.extend(clock.advance(frame as f64 / hz));
        }
        out
    }

    #[test]
    fn host_refresh_rate_cannot_change_the_substep_sequence() {
        let at_60 = walk(60.0);
        let at_144 = walk(144.0);
        let at_50 = walk(50.0);

        assert_eq!(at_60.len(), 800, "1 s at 1.25 ms is 800 substeps");
        assert_eq!(at_60, at_144, "60 Hz and 144 Hz must agree exactly");
        assert_eq!(at_60, at_50, "and so must 50 Hz content on either");
    }

    #[test]
    fn substeps_are_contiguous_and_exactly_dt_long() {
        let steps = walk(60.0);
        for pair in steps.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "no gaps, no overlaps");
        }
        for step in &steps {
            let length = f64::from(step.end) - f64::from(step.start);
            // Boundaries are f32, to match the trace's own `t`. A second into
            // the run that quantises to about 0.12 µs, comfortably inside the
            // microsecond resolution TRACE-FORMAT.md §2 claims. The grid itself
            // does not drift — it is recomputed from a step count, not
            // accumulated — so this is rounding at the edges and nothing more.
            assert!(
                (length - SUBSTEP_SECONDS).abs() < 1e-6,
                "substep was {length} s"
            );
        }
    }

    #[test]
    fn a_partial_substep_waits_for_the_next_frame() {
        let mut clock = SubstepClock::new(0.0);
        // Two thirds of a substep buys nothing.
        assert!(clock.advance(SUBSTEP_SECONDS * 0.66).is_empty());
        assert_eq!(clock.simulated(), 0.0);
        // The remainder plus a little completes exactly one.
        assert_eq!(clock.advance(SUBSTEP_SECONDS * 1.5).len(), 1);
        assert_eq!(clock.simulated(), SUBSTEP_SECONDS);
    }

    fn ramp() -> Vec<Sample> {
        // One long span: the beam sweeps x from -1 to 1 over 4 ms.
        vec![
            Sample::mono(-1.0, 0.0, 1.0, 0.0),
            Sample::mono(1.0, 0.0, 1.0, 0.004),
        ]
    }

    #[test]
    fn a_long_span_is_clipped_to_the_substep_window() {
        let clipped = clip_spans(&ramp(), 0.001, 0.002);
        assert_eq!(clipped.len(), 2);
        assert!((clipped[0].t - 0.001).abs() < 1e-9);
        assert!((clipped[1].t - 0.002).abs() < 1e-9);
        // A quarter and a half of the way along.
        assert!((clipped[0].x - -0.5).abs() < 1e-5);
        assert!((clipped[1].x - 0.0).abs() < 1e-5);
    }

    #[test]
    fn clipped_windows_tile_the_original_span() {
        let samples = ramp();
        let mut covered = 0.0f32;
        let mut clock = SubstepClock::new(0.0);
        for step in clock.advance(0.004) {
            let clipped = clip_spans(&samples, step.start, step.end);
            if clipped.is_empty() {
                continue;
            }
            covered += clipped[1].t - clipped[0].t;
        }
        // 4 ms is 3 whole substeps of 1.25 ms; the remainder waits.
        assert!((covered - 3.0 * SUBSTEP_SECONDS as f32).abs() < 1e-6);
    }

    #[test]
    fn drive_is_interpolated_at_the_clip_so_tapers_survive() {
        let samples = vec![
            Sample::mono(-1.0, 0.0, 1.0, 0.0),
            Sample::mono(1.0, 0.0, 0.0, 0.004),
        ];
        let clipped = clip_spans(&samples, 0.002, 0.003);
        assert!((clipped[0].drive_r - 0.5).abs() < 1e-5);
        assert!((clipped[1].drive_r - 0.25).abs() < 1e-5);
    }

    #[test]
    fn separate_pairs_do_not_join_into_phantom_spans() {
        let mut samples = vec![
            Sample::mono(-1.0, 0.0, 1.0, 0.000),
            Sample::mono(-0.5, 0.0, 1.0, 0.001),
            Sample::mono(0.5, 0.0, 1.0, 0.002),
            Sample::mono(1.0, 0.0, 1.0, 0.003),
        ];
        // The beam was dumped between the second and third samples.
        samples[2].flags |= flags::DISCONTINUITY;

        let clipped = clip_spans(&samples, 0.0, 0.004);
        assert_eq!(clipped.len(), 4, "two surviving spans, two samples each");
        assert!(!clipped[0].is_discontinuity());
        assert!(
            clipped[2].is_discontinuity(),
            "the second pair must not be drawn as continuous with the first"
        );
    }

    #[test]
    fn a_window_outside_the_trace_yields_nothing() {
        assert!(clip_spans(&ramp(), 0.010, 0.011).is_empty());
        assert!(clip_spans(&ramp(), -0.002, -0.001).is_empty());
        assert!(clip_spans(&[], 0.0, 1.0).is_empty());
    }
}
