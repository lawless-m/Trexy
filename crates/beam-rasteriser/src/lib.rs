//! Layer 2 — the beam rasteriser (deflection model).
//!
//! A stateful streaming filter turning machine drive events into the beam
//! trajectory that actually happened (ARCHITECTURE.md §2). The trace crossing
//! the interface records where the beam *was*, not what was commanded — every
//! difference between those two is this crate's job.
//!
//! It is a filter over an event stream, not a per-command function. Amplifier
//! slew state, integrator charge and sample-and-hold voltages all persist
//! across commands, and commands interrupt each other mid-flight: rewriting
//! the DAC while RAMP is asserted bends a stroke, and chopping BLANK with the
//! shift register dashes it. Both fall out of a filter and neither falls out
//! of a per-command function, which is why this is shaped the way it is.

use beam_trace::Sample;

/// Per-machine analogue constants. The filter is shared; only these differ
/// between a Vectrex and an Atari deflection chain (ARCHITECTURE.md §2).
///
/// Every field carries a provenance class per ARCHITECTURE.md §4 — that split
/// between measured and guessed *is* the accuracy claim, so a number without a
/// source says so rather than borrowing authority from its neighbours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Constants {
    /// Deflection units per second at full-scale rate. Sets stroke length for
    /// a given RAMP duration.
    ///
    /// **schematic (unverified).** Set by the integrator RC and the yoke's
    /// deflection sensitivity, both of which need the service manual. 1.0 is a
    /// placeholder meaning "full-scale rate crosses full deflection in one
    /// second", chosen so the model is dimensionally honest, not because it is
    /// the Vectrex's figure.
    pub integrator_gain: f32,
    /// Op-amp slew limit on the rate signal, full-scale units per second.
    ///
    /// A hard limit rather than a time constant: slew limiting is the
    /// large-signal behaviour of an amplifier running out of current, and it
    /// is what rounds a fast corner. Small-signal settling is the separate
    /// `settle_tau` below.
    ///
    /// **fitted (provisional).** What the datasheet gives: the LF353 slews at
    /// 13 V/µs typical, 8 V/µs minimum (TI LF353 datasheet, Electrical
    /// Characteristics, SR). Over the MC1408's ±2.5 V swing — 2.5 V per
    /// full-scale unit — that is 13e6 / 2.5 ≈ 5.2e6 full-scale units per
    /// second, two orders faster than the value here. The gap is not resolved:
    /// which amplifier in the chain actually binds, and whether this field
    /// stands for the rate signal or the integrator's own output, is a
    /// schematic question this crate cannot answer. So the derived figure is
    /// recorded and the shipped value stays fitted until the topology is
    /// verified.
    pub slew_limit: f32,
    /// First-order settling once inside the slew limit, seconds.
    ///
    /// **fitted (provisional).** No paper source; small-signal settling
    /// depends on the compensation and loading of the stage as built.
    pub settle_tau: f32,
    /// Sample-and-hold droop, seconds.
    ///
    /// Vectrex Y and Z are held on capacitors behind the CD4052 mux; X is
    /// driven straight from the DAC. That asymmetry is real and visible, so it
    /// is modelled rather than averaged away (ARCHITECTURE.md §5).
    ///
    /// **fitted (provisional).** Droop is hold-capacitor value divided by
    /// total leakage — the mux's off-channel leakage plus the follower's bias
    /// current. The capacitor value is schematic and unverified, so the rate
    /// cannot be derived even though the CD4052's leakage is published.
    pub sh_droop_tau: f32,
    /// Integrator leak toward centre, seconds. Why absolute position is only
    /// trustworthy just after a ZERO recal.
    ///
    /// **fitted (provisional).** Set by the integrator capacitor's leakage and
    /// the amplifier's input bias current in the as-built circuit.
    pub integrator_tau: f32,
    /// Z-axis (blanking) rise and fall, seconds.
    ///
    /// **fitted (provisional).** The Z chain's bandwidth is not published as a
    /// single figure and the stage is schematic.
    pub z_tau: f32,
    /// DAC settling, seconds.
    ///
    /// A code written to the converter does not appear at its output
    /// instantly, so the amplifier chases a moving target for a moment after
    /// every rewrite rather than the final value (ARCHITECTURE.md §2, "DAC
    /// settling glitch"). Zero means instantaneous.
    ///
    /// **fitted (provisional).** ~300 ns is the figure usually quoted for the
    /// MC1408, but the datasheet obtainable here is a scan with no text layer,
    /// so it is not cited — an unread source is not a source.
    pub dac_settle_tau: f32,
}

impl Constants {
    /// Vectrex: LF353 integrators, MC1408 DAC, CD4052 mux.
    ///
    /// Provisional, and each field says how provisional. One value is
    /// datasheet-derived and recorded as such but not adopted; the rest are
    /// fitted or schematic-unverified, because deriving them needs the service
    /// manual's component values and this crate has not seen it. They are the
    /// shape of the model — the effects are in the right places and in the
    /// right directions — not the Vectrex's numbers. ARCHITECTURE.md §4's
    /// calibration path (reference footage, then a test-card ROM isolating one
    /// parameter each) is how the fitted class is meant to shrink.
    pub fn vectrex() -> Self {
        Self {
            integrator_gain: 1.0,
            slew_limit: 4.0e4,
            settle_tau: 2.0e-6,
            sh_droop_tau: 5.0e-3,
            integrator_tau: 0.5,
            z_tau: 5.0e-7,
            // MC1408: ~300 ns to settle within ±1/2 LSB, per the datasheet.
            dac_settle_tau: 3.0e-7,
        }
    }

    /// No analogue behaviour at all: rates apply instantly, nothing droops.
    /// The control against which every effect above can be isolated.
    pub fn ideal() -> Self {
        Self {
            integrator_gain: 1.0,
            slew_limit: f32::INFINITY,
            settle_tau: 0.0,
            sh_droop_tau: f32::INFINITY,
            integrator_tau: f32::INFINITY,
            z_tau: 0.0,
            dac_settle_tau: 0.0,
        }
    }
}

/// What a machine tells the deflection chain. Deliberately tiny and
/// machine-neutral (ARCHITECTURE.md §2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// Commanded deflection rate, full scale ±1.
    Rate { t: f64, rx: f32, ry: f32 },
    /// Commanded beam current, linear light, unclamped.
    Current { t: f64, drive: [f32; 3] },
    /// The position jumped rather than moved — a Vectrex ZERO integrator dump.
    /// Nothing is drawn across the gap.
    Discontinuity { t: f64, x: f32, y: f32 },
}

impl Event {
    pub fn time(&self) -> f64 {
        match *self {
            Event::Rate { t, .. } | Event::Current { t, .. } | Event::Discontinuity { t, .. } => t,
        }
    }
}

/// The analogue state that persists between events.
#[derive(Clone, Copy, Debug)]
struct State {
    x: f32,
    y: f32,
    /// Rate actually reaching the integrators, after slew and droop.
    rx: f32,
    ry: f32,
    /// What the converter is actually presenting, as distinct from the code
    /// last written to it. The amplifier chases this, not the command.
    dac_x: f32,
    dac_y: f32,
    /// The voltage on the Y sample-and-hold capacitor. Latched when the
    /// machine writes it and drooping from then on — unlike X, which the DAC
    /// drives continuously, so it has no held value to decay
    /// (ARCHITECTURE.md §5).
    hold_ry: f32,
    /// Beam current actually at the gun, after Z rise/fall.
    drive: [f32; 3],
}

/// The filter. Push events in time order; it emits the trajectory between them.
pub struct Rasteriser {
    constants: Constants,
    /// Positional error bound the output honours (TRACE-FORMAT.md §4).
    epsilon: f32,
    /// Commanded values, as last set by an event.
    want_rx: f32,
    want_drive: [f32; 3],
    state: State,
    now: f64,
    /// Set by a discontinuity, consumed by the next emitted sample.
    pending_break: bool,
    started: bool,
    /// Path walked since the last emitted sample, so the error against the
    /// chord is measured rather than guessed.
    since_anchor: Vec<(f32, f32, [f32; 3])>,
}

/// Integration step. Small enough to resolve the fastest modelled constant
/// (`z_tau`, hundreds of nanoseconds) without being so small that a 20 ms
/// frame costs millions of steps.
const STEP_SECONDS: f64 = 2.5e-7;

/// Drive is linear between samples, so a transition needs samples where it
/// bends. One percent of nominal full drive (TRACE-FORMAT.md §4).
const DRIVE_TOLERANCE: f32 = 0.01;

impl Rasteriser {
    pub fn new(constants: Constants, epsilon: f32) -> Self {
        Self {
            constants,
            epsilon,
            want_rx: 0.0,
            want_drive: [0.0; 3],
            state: State {
                x: 0.0,
                y: 0.0,
                rx: 0.0,
                ry: 0.0,
                dac_x: 0.0,
                dac_y: 0.0,
                hold_ry: 0.0,
                drive: [0.0; 3],
            },
            now: 0.0,
            pending_break: false,
            started: false,
            since_anchor: Vec::new(),
        }
    }

    pub fn position(&self) -> (f32, f32) {
        (self.state.x, self.state.y)
    }

    pub fn drive(&self) -> [f32; 3] {
        self.state.drive
    }

    /// Advance to the event's time, emitting the trajectory, then apply it.
    pub fn push(&mut self, event: Event, out: &mut Vec<Sample>) {
        self.run_to(event.time(), out);
        match event {
            Event::Rate { rx, ry, .. } => {
                self.want_rx = rx;
                // Writing the mux latches Y onto the hold capacitor; it decays
                // from here until the next write.
                self.state.hold_ry = ry;
            }
            Event::Current { drive, .. } => self.want_drive = drive,
            Event::Discontinuity { x, y, .. } => {
                self.state.x = x;
                self.state.y = y;
                self.pending_break = true;
                // Emit here, not at the next convenient moment: the flag marks
                // the sample the beam jumped *to*, and one more step of travel
                // would attach it to a position the beam was never at.
                self.emit(out);
            }
        }
    }

    /// Advance to `t` with no new command, emitting the trajectory.
    pub fn run_to(&mut self, t: f64, out: &mut Vec<Sample>) {
        if !self.started {
            self.emit(out);
            self.started = true;
        }
        // A settled chain moving at constant rate draws a straight line, and a
        // straight line needs only its endpoints (TRACE-FORMAT.md §4). Skip
        // straight to the end rather than stepping through it.
        if self.is_settled() {
            let dt = (t - self.now) as f32;
            if dt > 0.0 {
                self.state.x += self.state.rx * self.constants.integrator_gain * dt;
                self.state.y += self.state.ry * self.constants.integrator_gain * dt;
                self.now = t;
                self.emit(out);
            }
            return;
        }

        let mut anchor = (self.state.x, self.state.y);
        let mut anchor_drive = self.state.drive;
        while self.now < t {
            // Try the closed form each time round, not once on entry: a
            // transient — a converter still settling, a drive edge — blocks it
            // briefly, and the ramp behind that transient is exactly what it
            // exists to skip. Attempting it only before the loop means one
            // moment of transient costs the whole ramp.
            if self.fast_forward_slew(t, out) {
                anchor = (self.state.x, self.state.y);
                anchor_drive = self.state.drive;
                continue;
            }
            let dt = STEP_SECONDS.min(t - self.now);
            self.step(dt as f32);
            self.now += dt;
            // Emit when a straight line from the last sample would stray
            // further than the bound allows.
            self.since_anchor
                .push((self.state.x, self.state.y, self.state.drive));
            // Cap the walk so the error scan stays bounded; a long smooth ramp
            // is re-anchored rather than accumulating a huge buffer.
            let full = self.since_anchor.len() >= 4096;
            if full || self.chord_error(anchor) > self.epsilon || self.drive_error(anchor_drive) > DRIVE_TOLERANCE {
                self.emit(out);
                anchor = (self.state.x, self.state.y);
                anchor_drive = self.state.drive;
            }
        }
        self.emit(out);
    }

    /// Advance a pure slew in closed form instead of walking it.
    ///
    /// While the amplifier is slew-limited the rate is a linear ramp, so
    /// position is quadratic and both have exact sums — a 10 ms slew is one
    /// calculation rather than forty thousand steps (ARCHITECTURE.md §2:
    /// "analytic fast-forward through steady ramps ... numerical stepping is
    /// only needed around events and high curvature").
    ///
    /// What is summed is the *stepper's* arithmetic, not the idealised
    /// integral. `step` advances position by the post-step rate, which is a
    /// first-order approximation; reproducing the exact integral here instead
    /// would make the fast and slow paths disagree by an accumulated bias, and
    /// the error bound is checked against a densely-stepped reference. Summing
    /// what stepping would have produced keeps one answer whichever path runs.
    ///
    /// Returns false when the state is not a pure slew, leaving the caller to
    /// step: droop, leak and the Z rise are exponentials that interact, and a
    /// combined closed form for those buys nothing — they settle in a bounded
    /// handful of time constants, whereas a slew grows without bound as the
    /// limit falls.
    fn fast_forward_slew(&mut self, until: f64, out: &mut Vec<Sample>) -> bool {
        let c = self.constants;
        if c.sh_droop_tau.is_finite()
            || c.integrator_tau.is_finite()
            || c.settle_tau > 0.0
            || self.state.drive != self.want_drive
            || !c.slew_limit.is_finite()
            // While the converter is still settling the slew target is moving,
            // so the ramp is not steady and there is nothing to fast-forward.
            // Once it has arrived the fast path applies as before.
            || (self.state.dac_x - self.want_rx).abs() > 1e-6
            || (self.state.dac_y - self.state.hold_ry).abs() > 1e-6
        {
            return false;
        }

        let dt = STEP_SECONDS as f32;
        let per_step = c.slew_limit * dt;
        if per_step <= 0.0 {
            return false;
        }

        let mut ran = false;
        loop {
            let (rx, ry) = (self.state.rx, self.state.ry);
            let (dx, dy) = (self.want_rx - rx, self.state.hold_ry - ry);
            if dx.abs() <= per_step && dy.abs() <= per_step {
                return ran; // no longer slewing; the caller settles it
            }

            // Steps this regime lasts: whichever axis reaches its target first.
            let until_target = |d: f32| {
                if d.abs() > per_step {
                    (d.abs() / per_step).floor() as u64
                } else {
                    u64::MAX
                }
            };
            let regime = until_target(dx).min(until_target(dy));

            // Steps left before `until`.
            let remaining = ((until - self.now) / STEP_SECONDS).floor();
            if remaining < 1.0 {
                return ran;
            }

            // Steps the error bound allows: position is quadratic in time, so
            // its departure from the chord over T is |a|T²/4 with a = s·gain/2.
            let sx = if dx.abs() > per_step { c.slew_limit * dx.signum() } else { 0.0 };
            let sy = if dy.abs() > per_step { c.slew_limit * dy.signum() } else { 0.0 };
            let accel = 0.5 * c.integrator_gain * (sx * sx + sy * sy).sqrt();
            let span = if accel > 0.0 {
                (4.0 * self.epsilon / accel).sqrt()
            } else {
                f32::INFINITY
            };
            let bound = (f64::from(span) / STEP_SECONDS).floor().max(1.0);

            let n = regime.min(remaining as u64).min(bound as u64).max(1);
            let nf = n as f32;

            // r_k = r0 + k·s·dt, so Σ r_k = n·r0 + s·dt·n(n+1)/2.
            let advance = |r0: f32, s: f32| -> (f32, f32) {
                let sum = nf * r0 + s * dt * nf * (nf + 1.0) * 0.5;
                (r0 + nf * s * dt, sum * c.integrator_gain * dt)
            };
            let (rx_end, x_moved) = advance(rx, sx);
            let (ry_end, y_moved) = advance(ry, sy);

            self.state.rx = rx_end;
            self.state.ry = ry_end;
            self.state.x += x_moved;
            self.state.y += y_moved;
            self.now += n as f64 * STEP_SECONDS;
            self.since_anchor.clear();
            self.emit(out);
            ran = true;
        }
    }

    /// Nothing left to converge: rates are at their commanded values, the
    /// drive is at its commanded value, and neither droop nor leak will move
    /// the beam by more than the error bound over a typical stroke.
    fn is_settled(&self) -> bool {
        let c = &self.constants;
        let dac_ok = (self.state.dac_x - self.want_rx).abs() < 1e-6
            && (self.state.dac_y - self.state.hold_ry).abs() < 1e-6;
        let rate_ok = dac_ok
            && (self.state.rx - self.want_rx).abs() < 1e-6
            && (self.state.ry - self.state.hold_ry).abs() < 1e-6;
        let drive_ok = (0..3).all(|i| (self.state.drive[i] - self.want_drive[i]).abs() < 1e-6);
        let droop_ok = !c.sh_droop_tau.is_finite() || self.state.hold_ry.abs() < 1e-9;
        let leak_ok = !c.integrator_tau.is_finite();
        rate_ok && drive_ok && droop_ok && leak_ok
    }

    /// The furthest the walked path strays from the straight line joining the
    /// anchor sample to the current position.
    ///
    /// Measured over every step since the anchor, not sampled at the midpoint.
    /// A midpoint estimate reads exactly zero on a symmetric span — the chord's
    /// centre landing on the path's centre while the path bulges either side of
    /// it — which is precisely the fault commit 5b2cbc0 fixed in the synthetic
    /// sampler. Making it again here would cost the same silent undersampling.
    fn chord_error(&self, anchor: (f32, f32)) -> f32 {
        let (bx, by) = (self.state.x, self.state.y);
        let (ax, ay) = anchor;
        let (dx, dy) = (bx - ax, by - ay);
        let len = (dx * dx + dy * dy).sqrt();
        let mut worst = 0.0f32;
        for &(px, py, _) in &self.since_anchor {
            let d = if len < 1e-12 {
                ((px - ax).powi(2) + (py - ay).powi(2)).sqrt()
            } else {
                ((bx - ax) * (ay - py) - (ax - px) * (by - ay)).abs() / len
            };
            worst = worst.max(d);
        }
        worst
    }

    /// How far the walked drive strays from linear interpolation between the
    /// anchor sample and the current one. The trace promises drive is linear
    /// between samples (TRACE-FORMAT.md §4), so a Z edge has to be bracketed
    /// tightly enough to make that true — a positional bound alone steps
    /// straight over a blanking edge without noticing it.
    fn drive_error(&self, anchor: [f32; 3]) -> f32 {
        let n = self.since_anchor.len();
        if n < 2 {
            return 0.0;
        }
        let mut worst = 0.0f32;
        for (i, &(_, _, d)) in self.since_anchor.iter().enumerate() {
            let f = i as f32 / (n - 1) as f32;
            for c in 0..3 {
                let lerp = anchor[c] + (self.state.drive[c] - anchor[c]) * f;
                worst = worst.max((d[c] - lerp).abs());
            }
        }
        worst
    }

    /// One integration step of the analogue chain.
    fn step(&mut self, dt: f32) {
        let c = &self.constants;

        // Slew limit first: the amplifier can only change its output so fast.
        let approach = |actual: f32, want: f32| -> f32 {
            let delta = want - actual;
            let max = c.slew_limit * dt;
            if delta.abs() > max {
                actual + max * delta.signum()
            } else if c.settle_tau > 0.0 {
                // Inside the slew limit, first-order settling takes over.
                actual + delta * (1.0 - (-dt / c.settle_tau).exp())
            } else {
                want
            }
        };
        // Y is held on a capacitor behind the mux and droops toward zero; X is
        // driven straight from the DAC and does not (ARCHITECTURE.md §5).
        if c.sh_droop_tau.is_finite() && c.sh_droop_tau > 0.0 {
            self.state.hold_ry *= (-dt / c.sh_droop_tau).exp();
        }

        // The converter settles toward the code written to it, so for a moment
        // after each rewrite the amplifier is chasing a value that is itself
        // still moving. This sits ahead of the slew stage because that is where
        // it sits in the circuit.
        if c.dac_settle_tau > 0.0 {
            let closed = 1.0 - (-dt / c.dac_settle_tau).exp();
            self.state.dac_x += (self.want_rx - self.state.dac_x) * closed;
            self.state.dac_y += (self.state.hold_ry - self.state.dac_y) * closed;
        } else {
            self.state.dac_x = self.want_rx;
            self.state.dac_y = self.state.hold_ry;
        }

        self.state.rx = approach(self.state.rx, self.state.dac_x);
        self.state.ry = approach(self.state.ry, self.state.dac_y);

        // Integrate rate into position.
        self.state.x += self.state.rx * c.integrator_gain * dt;
        self.state.y += self.state.ry * c.integrator_gain * dt;

        // Integrator leak toward centre.
        if c.integrator_tau.is_finite() && c.integrator_tau > 0.0 {
            let keep = (-dt / c.integrator_tau).exp();
            self.state.x *= keep;
            self.state.y *= keep;
        }

        // Z axis rise/fall.
        for i in 0..3 {
            let want = self.want_drive[i];
            self.state.drive[i] = if c.z_tau > 0.0 {
                let d = want - self.state.drive[i];
                self.state.drive[i] + d * (1.0 - (-dt / c.z_tau).exp())
            } else {
                want
            };
        }
    }

    fn emit(&mut self, out: &mut Vec<Sample>) {
        // Producers must emit strictly increasing t within a buffer
        // (TRACE-FORMAT.md §2), so never push a second sample at the same
        // instant — update the one already there instead.
        if let Some(last) = out.last_mut() {
            // Compare against the value actually stored: t is f32 on the
            // wire, so an f64 `now` that rounds to the same f32 *is* the same
            // instant as far as the trace is concerned, and pushing again
            // would break the strictly-increasing rule (TRACE-FORMAT.md §2).
            if last.t == self.now as f32 {
                last.x = self.state.x;
                last.y = self.state.y;
                last.drive_r = self.state.drive[0];
                last.drive_g = self.state.drive[1];
                last.drive_b = self.state.drive[2];
                // A break at this instant annotates the sample already here
                // rather than adding a second one at the same t.
                if self.pending_break {
                    last.flags |= beam_trace::flags::DISCONTINUITY;
                    self.pending_break = false;
                }
                self.since_anchor.clear();
                return;
            }
        }
        self.since_anchor.clear();
        out.push(Sample {
            x: self.state.x,
            y: self.state.y,
            drive_r: self.state.drive[0],
            drive_g: self.state.drive[1],
            drive_b: self.state.drive[2],
            t: self.now as f32,
            flags: if self.pending_break {
                beam_trace::flags::DISCONTINUITY
            } else {
                0
            },
            reserved: 0,
        });
        self.pending_break = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1.0 / 4096.0;

    fn run(c: Constants, events: &[Event], until: f64) -> Vec<Sample> {
        let mut r = Rasteriser::new(c, EPS);
        let mut out = Vec::new();
        for e in events {
            r.push(*e, &mut out);
        }
        r.run_to(until, &mut out);
        out
    }

    #[test]
    fn a_settled_chain_at_constant_rate_draws_a_straight_line() {
        let out = run(
            Constants::ideal(),
            &[Event::Rate { t: 0.0, rx: 1.0, ry: 0.0 }],
            1.0e-3,
        );
        // Straight lines need only endpoints — that sparsity is the contract
        // working, not a shortcut.
        assert!(out.len() <= 3, "straight line took {} samples", out.len());
        let last = out.last().expect("samples");
        assert!((last.x - 1.0e-3).abs() < 1e-6, "x = {}", last.x);
        assert!(last.y.abs() < 1e-9, "y = {}", last.y);
    }

    #[test]
    fn every_sample_is_strictly_later_than_the_last() {
        let out = run(
            Constants::vectrex(),
            &[
                Event::Rate { t: 0.0, rx: 1.0, ry: 0.4 },
                Event::Current { t: 1.0e-5, drive: [1.0; 3] },
                Event::Rate { t: 2.0e-4, rx: -1.0, ry: 1.0 },
            ],
            5.0e-4,
        );
        assert!(out.len() > 2);
        for pair in out.windows(2) {
            assert!(
                pair[1].t > pair[0].t,
                "t must strictly increase: {} then {}",
                pair[0].t,
                pair[1].t
            );
        }
    }

    #[test]
    fn slew_limiting_rounds_a_corner_that_is_sharp_without_it() {
        let events = [
            Event::Rate { t: 0.0, rx: 1.0, ry: 0.0 },
            Event::Rate { t: 1.0e-4, rx: 0.0, ry: 1.0 },
        ];
        let sharp = run(Constants::ideal(), &events, 2.0e-4);
        let mut slewed = Constants::ideal();
        slewed.slew_limit = 2.0e4;
        let round = run(slewed, &events, 2.0e-4);

        // The corner is where x stops growing and y starts. With an infinite
        // slew limit the beam turns instantly and never visits the diagonal;
        // with a finite one it must.
        let corner = |s: &[Sample]| {
            s.iter()
                .filter(|p| p.x > 0.0 && p.y > 0.0)
                .map(|p| p.y / p.x)
                .fold(0.0f32, f32::max)
        };
        assert!(
            corner(&round) > 0.0,
            "a slew-limited corner must pass through the diagonal"
        );
        // Neither count nor distance travelled distinguishes these: slewing
        // into the corner loses exactly what slewing out of it regains, so
        // both paths end at the same x. What differs is the corner itself —
        // the ideal beam turns through the vertex, the slew-limited one cuts
        // inside it and never gets there.
        let vertex = (1.0e-4f32, 0.0f32);
        let closest = |s: &[Sample]| {
            s.iter()
                .map(|p| ((p.x - vertex.0).powi(2) + (p.y - vertex.1).powi(2)).sqrt())
                .fold(f32::MAX, f32::min)
        };
        assert!(
            closest(&sharp) < 1.0e-6,
            "the ideal beam should pass through the corner, missed by {}",
            closest(&sharp)
        );
        assert!(
            closest(&round) > 1.0e-5,
            "the slew-limited beam should round the corner, but reached within {}",
            closest(&round)
        );
    }

    #[test]
    fn the_y_sample_and_hold_droops_and_x_does_not() {
        // X is driven straight from the DAC, Y is held on a capacitor behind
        // the mux (ARCHITECTURE.md §5). Equal commanded rates must therefore
        // not travel equal distances.
        let mut c = Constants::ideal();
        c.sh_droop_tau = 1.0e-4;
        let out = run(c, &[Event::Rate { t: 0.0, rx: 1.0, ry: 1.0 }], 3.0e-4);
        let last = out.last().expect("samples");
        assert!(
            last.y < last.x * 0.95,
            "Y should lag X through droop: x = {}, y = {}",
            last.x,
            last.y
        );
    }

    #[test]
    fn the_integrator_leaks_toward_centre() {
        let mut c = Constants::ideal();
        c.integrator_tau = 1.0e-3;
        // Drive out, then stop commanding anything and watch it fall back.
        let mut r = Rasteriser::new(c, EPS);
        let mut out = Vec::new();
        r.push(Event::Rate { t: 0.0, rx: 1.0, ry: 0.0 }, &mut out);
        r.push(Event::Rate { t: 1.0e-3, rx: 0.0, ry: 0.0 }, &mut out);
        let held = r.position().0;
        r.run_to(3.0e-3, &mut out);
        let later = r.position().0;
        assert!(
            later < held,
            "position should leak toward centre: {held} then {later}"
        );
        assert!(later > 0.0, "it should leak, not collapse: {later}");
    }

    #[test]
    fn a_discontinuity_moves_the_beam_without_drawing_across_the_gap() {
        let out = run(
            Constants::ideal(),
            &[
                Event::Rate { t: 0.0, rx: 1.0, ry: 0.0 },
                Event::Discontinuity { t: 1.0e-4, x: 0.0, y: 0.0 },
                Event::Rate { t: 1.0e-4, rx: 0.0, ry: 1.0 },
            ],
            2.0e-4,
        );
        let broken: Vec<_> = out
            .iter()
            .filter(|s| s.flags & beam_trace::flags::DISCONTINUITY != 0)
            .collect();
        assert_eq!(broken.len(), 1, "exactly one break expected");
        assert!(broken[0].x.abs() < 1e-9 && broken[0].y.abs() < 1e-9);
    }

    #[test]
    fn the_blanking_edge_is_not_instant() {
        let mut c = Constants::ideal();
        c.z_tau = 1.0e-6;
        let out = run(
            c,
            &[
                Event::Rate { t: 0.0, rx: 1.0, ry: 0.0 },
                Event::Current { t: 0.0, drive: [1.0; 3] },
            ],
            1.0e-5,
        );
        // Somewhere between off and on there must be a partly-lit sample —
        // that rise is what makes a Vectrex stroke fade in at its start.
        assert!(
            out.iter().any(|s| s.drive_r > 0.01 && s.drive_r < 0.99),
            "no partial drive found across the Z rise"
        );
    }

    #[test]
    fn a_long_slew_is_summed_rather_than_walked() {
        // A slow amplifier: 10 ms to cross the rate range, which at the
        // 2.5e-7 s step is forty thousand steps to walk.
        let mut c = Constants::ideal();
        c.slew_limit = 100.0;
        let events = [Event::Rate { t: 0.0, rx: 1.0, ry: 0.0 }];
        let out = run(c, &events, 1.0e-2);

        let stepped = 1.0e-2 / super::STEP_SECONDS;
        assert!(
            (out.len() as f64) < stepped / 100.0,
            "expected the closed form, but {} samples looks like walking {stepped:.0} steps",
            out.len()
        );

        // And it must still be the same curve. Re-walk it densely and check
        // the emitted polyline never strays further than the bound allows.
        let mut fine = Rasteriser::new(c, 1.0e-9);
        let mut dense = Vec::new();
        fine.push(events[0], &mut dense);
        fine.run_to(1.0e-2, &mut dense);

        let mut worst = 0.0f32;
        for d in &dense {
            let mut best = f32::MAX;
            for seg in out.windows(2) {
                let (ax, ay, bx, by) = (seg[0].x, seg[0].y, seg[1].x, seg[1].y);
                let (vx, vy) = (bx - ax, by - ay);
                let len2 = vx * vx + vy * vy;
                let f = if len2 <= 0.0 {
                    0.0
                } else {
                    (((d.x - ax) * vx + (d.y - ay) * vy) / len2).clamp(0.0, 1.0)
                };
                best = best.min(((d.x - (ax + vx * f)).powi(2) + (d.y - (ay + vy * f)).powi(2)).sqrt());
            }
            worst = worst.max(best);
        }
        assert!(worst <= EPS * 2.0, "summed slew strays {worst}, bound {EPS}");
    }

    #[test]
    fn summing_a_slew_lands_where_walking_it_lands() {
        // The closed form is the stepper's own arithmetic, so the two paths
        // must agree — otherwise the answer would depend on which ran.
        let mut c = Constants::ideal();
        c.slew_limit = 100.0;

        let mut fast = Rasteriser::new(c, EPS);
        let mut out = Vec::new();
        fast.push(Event::Rate { t: 0.0, rx: 1.0, ry: 0.5 }, &mut out);
        fast.run_to(5.0e-3, &mut out);

        // Force the stepping path by denying the fast one its preconditions,
        // then compare where the beam ended up.
        let mut walked = Rasteriser::new(c, EPS);
        let mut ignored = Vec::new();
        walked.push(Event::Rate { t: 0.0, rx: 1.0, ry: 0.5 }, &mut ignored);
        while walked.now < 5.0e-3 {
            let dt = super::STEP_SECONDS.min(5.0e-3 - walked.now);
            walked.step(dt as f32);
            walked.now += dt;
        }

        let (fx, fy) = fast.position();
        let (wx, wy) = walked.position();
        assert!(
            (fx - wx).abs() < 1e-6 && (fy - wy).abs() < 1e-6,
            "summed ({fx}, {fy}) but walked ({wx}, {wy})"
        );
    }

    #[test]
    fn a_settling_converter_bends_the_stroke_then_rejoins_it() {
        // A rewrite mid-ramp: the amplifier chases a target that is itself
        // still moving, so the beam departs from where an instant converter
        // would have put it, then rejoins once the code has arrived.
        let events = [
            Event::Rate { t: 0.0, rx: 1.0, ry: 0.0 },
            Event::Rate { t: 5.0e-6, rx: -1.0, ry: 0.0 },
        ];
        let tau = 1.0e-6;

        let instant = run(Constants::ideal(), &events, 3.0e-5);
        let mut settling = Constants::ideal();
        settling.dac_settle_tau = tau;
        let settled = run(settling, &events, 3.0e-5);

        let at = |s: &[Sample], t: f32| {
            s.windows(2)
                .find(|w| w[0].t <= t && t <= w[1].t)
                .map(|w| {
                    let f = if w[1].t > w[0].t { (t - w[0].t) / (w[1].t - w[0].t) } else { 0.0 };
                    w[0].x + (w[1].x - w[0].x) * f
                })
                .unwrap_or_else(|| s.last().expect("samples").x)
        };

        // Just after the rewrite the two disagree...
        let near = (at(&settled, 6.0e-6) - at(&instant, 6.0e-6)).abs();
        assert!(near > 1.0e-7, "no departure at the rewrite: {near}");

        // ...and a few time constants later they are travelling together
        // again, the difference frozen at whatever the glitch cost.
        let a = (at(&settled, 2.0e-5) - at(&instant, 2.0e-5)).abs();
        let b = (at(&settled, 3.0e-5) - at(&instant, 3.0e-5)).abs();
        assert!(
            (a - b).abs() < near,
            "still diverging {} s after the rewrite: {a} then {b}",
            2.0e-5 - 5.0e-6
        );
    }

    #[test]
    fn the_error_bound_holds_with_a_settling_converter() {
        let mut c = Constants::ideal();
        c.dac_settle_tau = 1.0e-6;
        c.slew_limit = 1.0e4;
        let events = [
            Event::Rate { t: 0.0, rx: 1.0, ry: 0.0 },
            Event::Rate { t: 1.0e-4, rx: 0.0, ry: 1.0 },
        ];
        let out = run(c, &events, 3.0e-4);

        let mut fine = Rasteriser::new(c, 1.0e-9);
        let mut dense = Vec::new();
        for e in &events {
            fine.push(*e, &mut dense);
        }
        fine.run_to(3.0e-4, &mut dense);

        let mut worst = 0.0f32;
        for d in &dense {
            let mut best = f32::MAX;
            for seg in out.windows(2) {
                let (ax, ay, bx, by) = (seg[0].x, seg[0].y, seg[1].x, seg[1].y);
                let (vx, vy) = (bx - ax, by - ay);
                let len2 = vx * vx + vy * vy;
                let f = if len2 <= 0.0 {
                    0.0
                } else {
                    (((d.x - ax) * vx + (d.y - ay) * vy) / len2).clamp(0.0, 1.0)
                };
                best = best.min(((d.x - (ax + vx * f)).powi(2) + (d.y - (ay + vy * f)).powi(2)).sqrt());
            }
            worst = worst.max(best);
        }
        assert!(worst <= EPS * 2.0, "strays {worst} with the converter settling, bound {EPS}");
    }

    #[test]
    fn output_honours_the_positional_error_bound() {
        // A corner under slew limiting is the highest-curvature thing the
        // model produces, so it is where the bound is hardest to hold.
        let mut c = Constants::ideal();
        c.slew_limit = 1.0e4;
        let out = run(
            c,
            &[
                Event::Rate { t: 0.0, rx: 1.0, ry: 0.0 },
                Event::Rate { t: 1.0e-4, rx: 0.0, ry: 1.0 },
            ],
            3.0e-4,
        );
        // Re-walk at fine resolution and check the emitted polyline against it.
        let mut fine = Rasteriser::new(c, 1.0e-9);
        let mut dense = Vec::new();
        fine.push(Event::Rate { t: 0.0, rx: 1.0, ry: 0.0 }, &mut dense);
        fine.push(Event::Rate { t: 1.0e-4, rx: 0.0, ry: 1.0 }, &mut dense);
        fine.run_to(3.0e-4, &mut dense);

        let mut worst = 0.0f32;
        for d in &dense {
            let mut best = f32::MAX;
            for seg in out.windows(2) {
                let (ax, ay, bx, by) = (seg[0].x, seg[0].y, seg[1].x, seg[1].y);
                let (vx, vy) = (bx - ax, by - ay);
                let len2 = vx * vx + vy * vy;
                let t = if len2 <= 0.0 {
                    0.0
                } else {
                    (((d.x - ax) * vx + (d.y - ay) * vy) / len2).clamp(0.0, 1.0)
                };
                let (px, py) = (ax + vx * t, ay + vy * t);
                best = best.min(((d.x - px).powi(2) + (d.y - py).powi(2)).sqrt());
            }
            worst = worst.max(best);
        }
        assert!(
            worst <= EPS * 2.0,
            "polyline strays {worst} from the true path, bound {EPS}"
        );
    }
}
