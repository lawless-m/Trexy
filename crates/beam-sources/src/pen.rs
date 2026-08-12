//! Building traces that honour the sampling contract — TRACE-FORMAT.md §4.
//!
//! The contract binds synthetic producers exactly as it binds a deflection
//! model, and deliberately so: exercising it from day one is what stops the
//! renderer being quietly tuned against traces no real producer would emit
//! (FIRST-SLICE.md §1 deliverable 4).

use beam_trace::Sample;

/// How long a drive step is bracketed over. Linear interpolation between the
/// samples either side of a step spans only this, so the "within ~1% of
/// modelled drive" bound holds trivially through a transition.
///
/// A real Z-axis has a rise and fall time; supplying that is layer 2's job
/// (ARCHITECTURE.md §2), not the synthetic generator's.
const DRIVE_STEP_SECONDS: f32 = 1e-6;

/// Drive is linear between samples, so a curve whose drive bends needs samples
/// where it bends. One percent of nominal full drive.
const DRIVE_TOLERANCE: f32 = 0.01;

/// Bisection gives up here rather than emitting samples too close together to
/// keep `t` strictly increasing in f32.
const MAX_DEPTH: u32 = 16;

/// And gives up here too, whichever comes first. Two samples a microsecond
/// apart already resolve far more than the substep loop can use.
const MIN_SPAN_SECONDS: f32 = 1e-6;

/// A beam being walked around the tube, emitting samples as it goes.
pub struct Pen {
    samples: Vec<Sample>,
    epsilon: f32,
    now: f32,
    at: [f32; 2],
    drive: [f32; 3],
}

impl Pen {
    /// Start at `at`, dark. The first sample establishes where the beam is.
    pub fn new(at: [f32; 2], epsilon: f32) -> Self {
        let mut pen = Self {
            samples: Vec::new(),
            epsilon,
            now: 0.0,
            at,
            drive: [0.0; 3],
        };
        pen.push(at, [0.0; 3]);
        pen
    }

    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    pub fn into_samples(self) -> Vec<Sample> {
        self.samples
    }

    pub fn now(&self) -> f32 {
        self.now
    }

    pub fn position(&self) -> [f32; 2] {
        self.at
    }

    fn push(&mut self, at: [f32; 2], drive: [f32; 3]) {
        // `t` must strictly increase (TRACE-FORMAT.md §2). The callers below
        // all advance time before pushing, but f32 rounding at the far end of
        // a long trace can still collapse two instants onto one value, so the
        // invariant is guaranteed here rather than assumed.
        if let Some(previous) = self.samples.last()
            && self.now <= previous.t
        {
            self.now = f32::from_bits(previous.t.to_bits() + 1);
        }
        self.samples.push(Sample {
            x: at[0],
            y: at[1],
            drive_r: drive[0],
            drive_g: drive[1],
            drive_b: drive[2],
            t: self.now,
            flags: 0,
            reserved: 0,
        });
        self.at = at;
        self.drive = drive;
    }

    /// Change beam current with the beam standing still.
    ///
    /// Only correct when the beam really is parked — a gun switched on while
    /// the beam is stationary burns a dot into the phosphor, and at 1 µs that
    /// dot is brighter than a fast stroke. Moving changes go through
    /// [`Pen::stroke`], which releases blanking as the sweep begins.
    pub fn set_drive(&mut self, drive: [f32; 3]) {
        if drive == self.drive {
            return;
        }
        self.now += DRIVE_STEP_SECONDS;
        self.push(self.at, drive);
    }

    /// Sweep to `to` at `speed` deflection units per second, at constant drive.
    ///
    /// A straight span at constant drive needs no intermediate samples: the
    /// renderer interpolates position and drive linearly, so two endpoints
    /// reproduce it exactly. That sparsity is the contract working, and it is
    /// what beads a point-splat renderer.
    pub fn stroke(&mut self, to: [f32; 2], drive: [f32; 3], speed: f32) {
        let from = self.at;
        let total = ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2)).sqrt();

        if drive != self.drive {
            if total <= 0.0 {
                // Nowhere to move; the change is genuinely stationary.
                self.set_drive(drive);
            } else {
                // Release blanking as the sweep starts, not before it. The
                // transition rides the first microsecond of travel, so it
                // deposits a short faint segment instead of a terminal dot.
                let f = (DRIVE_STEP_SECONDS * speed / total).min(0.5);
                let point = [
                    from[0] + (to[0] - from[0]) * f,
                    from[1] + (to[1] - from[1]) * f,
                ];
                self.travel(point, speed);
                self.push(point, drive);
            }
        }

        self.travel(to, speed);
        self.push(to, drive);
    }

    /// Sweep with the gun off. Blanked travel is part of the trace, never
    /// omitted (TRACE-FORMAT.md §1).
    pub fn blank_to(&mut self, to: [f32; 2], speed: f32) {
        self.stroke(to, [0.0; 3], speed);
    }

    /// Hold still, emitting the parked span.
    pub fn park(&mut self, drive: [f32; 3], seconds: f32) {
        self.set_drive(drive);
        self.now += seconds;
        self.push(self.at, drive);
    }

    /// Sweep to `to` while drive ramps from the current value to `drive_to`,
    /// with `steps` samples across the transition.
    ///
    /// Linear interpolation would render a two-sample ramp identically; the
    /// extra samples exist so a trace of a *modelled* taper has somewhere to
    /// put its curvature, and so the fixture visibly contains the transition.
    pub fn taper(&mut self, to: [f32; 2], drive_to: [f32; 3], speed: f32, steps: usize) {
        let from = self.at;
        let drive_from = self.drive;
        let steps = steps.max(1);

        for step in 1..=steps {
            let f = step as f32 / steps as f32;
            let point = [
                from[0] + (to[0] - from[0]) * f,
                from[1] + (to[1] - from[1]) * f,
            ];
            let drive = std::array::from_fn(|c| drive_from[c] + (drive_to[c] - drive_from[c]) * f);
            self.travel(point, speed);
            self.push(point, drive);
        }
    }

    fn travel(&mut self, to: [f32; 2], speed: f32) {
        let distance = ((to[0] - self.at[0]).powi(2) + (to[1] - self.at[1]).powi(2)).sqrt();
        self.now += distance / speed.max(f32::EPSILON);
    }

    /// Follow a parametric path for `seconds`, subdividing until the
    /// piecewise-linear trace is within ε of it.
    ///
    /// `path` takes u ∈ [0, 1] and returns position and drive. The beam is
    /// assumed to already be at `path(0)`.
    ///
    /// Returns the instant u = 0 lands on, which is after the bracketed drive
    /// step rather than at the call. Callers mapping u back to trace time need
    /// that instant, not the one before.
    pub fn curve<F>(&mut self, seconds: f32, path: F) -> f32
    where
        F: Fn(f32) -> ([f32; 2], [f32; 3]),
    {
        // u = 0 is the instant the caller left the beam at; the trace already
        // has a sample there.
        let start = self.now;

        // As in `stroke`, a drive change rides the opening of the sweep rather
        // than happening while the beam is parked at the figure's start point.
        let mut from = 0.0;
        if path(0.0).1 != self.drive && seconds > 0.0 {
            from = (DRIVE_STEP_SECONDS / seconds).min(0.5);
            let (point, drive) = path(from);
            self.now = start + from * seconds;
            self.push(point, drive);
        }

        self.refine(&path, from, 1.0, start, seconds, 0);
        self.now = start + seconds;
        start
    }

    fn refine<F>(&mut self, path: &F, u0: f32, u1: f32, start: f32, seconds: f32, depth: u32)
    where
        F: Fn(f32) -> ([f32; 2], [f32; 3]),
    {
        let (p0, d0) = path(u0);
        let (p1, d1) = path(u1);
        let um = 0.5 * (u0 + u1);
        let (pm, dm) = path(um);

        // Error of the chord against the true path, measured at the midpoint.
        let chord = [0.5 * (p0[0] + p1[0]), 0.5 * (p0[1] + p1[1])];
        let deviation = ((pm[0] - chord[0]).powi(2) + (pm[1] - chord[1]).powi(2)).sqrt();
        let drive_error = (0..3)
            .map(|c| (dm[c] - 0.5 * (d0[c] + d1[c])).abs())
            .fold(0.0f32, f32::max);

        let divisible = (u1 - u0) * seconds > 2.0 * MIN_SPAN_SECONDS;
        if depth < MAX_DEPTH
            && divisible
            && (deviation > self.epsilon || drive_error > DRIVE_TOLERANCE)
        {
            self.refine(path, u0, um, start, seconds, depth + 1);
            self.refine(path, um, u1, start, seconds, depth + 1);
            return;
        }

        self.now = start + u1 * seconds;
        self.push(p1, d1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = beam_trace::DEFAULT_EPSILON;

    #[test]
    fn a_straight_stroke_needs_no_intermediate_samples() {
        let mut pen = Pen::new([-1.0, 0.0], EPSILON);
        pen.stroke([1.0, 0.0], [1.0; 3], 100.0);
        // Start, the bracketed drive step, and the end.
        assert_eq!(pen.samples().len(), 3);
    }

    #[test]
    fn timestamps_strictly_increase() {
        let mut pen = Pen::new([0.0, 0.0], EPSILON);
        pen.stroke([0.5, 0.5], [1.0; 3], 50.0);
        pen.park([2.0; 3], 0.001);
        pen.blank_to([-0.5, -0.5], 500.0);
        pen.curve(0.01, |u| {
            let a = u * std::f32::consts::TAU;
            ([0.5 * a.cos(), 0.5 * a.sin()], [1.0; 3])
        });
        for pair in pen.samples().windows(2) {
            assert!(
                pair[1].t > pair[0].t,
                "{} did not exceed {}",
                pair[1].t,
                pair[0].t
            );
        }
    }

    #[test]
    fn blanked_travel_is_present_as_zero_drive_samples() {
        let mut pen = Pen::new([0.0, 0.0], EPSILON);
        pen.blank_to([0.8, 0.0], 1000.0);
        let last = pen.samples().last().unwrap();
        assert_eq!([last.drive_r, last.drive_g, last.drive_b], [0.0; 3]);
        assert_eq!(last.x, 0.8);
    }

    #[test]
    fn a_curve_stays_within_epsilon_of_the_true_path() {
        let path = |u: f32| {
            let a = u * std::f32::consts::TAU;
            ([0.8 * a.cos(), 0.8 * a.sin()], [1.0; 3])
        };
        let mut pen = Pen::new(path(0.0).0, EPSILON);
        let seconds = 0.02;
        let start = pen.curve(seconds, path);

        // Walk the emitted polyline against a dense evaluation of the circle.
        let samples: Vec<_> = pen
            .samples()
            .iter()
            .filter(|s| s.t >= start)
            .copied()
            .collect();
        let mut worst = 0.0f32;
        for pair in samples.windows(2) {
            for step in 0..=16 {
                let f = step as f32 / 16.0;
                let t = pair[0].t + (pair[1].t - pair[0].t) * f;
                let (truth, _) = path((t - start) / seconds);
                let x = pair[0].x + (pair[1].x - pair[0].x) * f;
                let y = pair[0].y + (pair[1].y - pair[0].y) * f;
                worst = worst.max(((truth[0] - x).powi(2) + (truth[1] - y).powi(2)).sqrt());
            }
        }
        assert!(
            worst <= EPSILON,
            "worst deviation was {worst}, ε is {EPSILON}"
        );
    }

    #[test]
    fn a_curve_is_sparse_where_nothing_is_happening() {
        // A straight path dressed up as a curve still needs only its ends.
        let mut pen = Pen::new([-0.5, 0.0], EPSILON);
        pen.curve(0.01, |u| ([-0.5 + u, 0.0], [1.0; 3]));
        assert!(
            pen.samples().len() <= 3,
            "a straight path took {} samples",
            pen.samples().len()
        );
    }

    #[test]
    fn a_taper_puts_samples_across_the_transition() {
        let mut pen = Pen::new([-0.5, 0.0], EPSILON);
        pen.set_drive([1.0; 3]);
        let before = pen.samples().len();
        pen.taper([0.5, 0.0], [0.0; 3], 100.0, 6);
        let taper = &pen.samples()[before..];
        assert_eq!(taper.len(), 6);
        for pair in taper.windows(2) {
            assert!(pair[1].drive_r < pair[0].drive_r);
        }
        assert_eq!(taper.last().unwrap().drive_r, 0.0);
    }
}
