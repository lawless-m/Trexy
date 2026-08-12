//! The seven acceptance patterns — FIRST-SLICE.md §3.
//!
//! Each isolates one thing and has a named pass criterion. They are the
//! renderer's calibration targets, not decoration.

use std::f32::consts::TAU;

use beam_trace::{Trace, TraceHeader};

use crate::lissajous::Lissajous;
use crate::pen::Pen;

/// The positional bound every fixture honours (TRACE-FORMAT.md §4).
pub const EPSILON: f32 = beam_trace::DEFAULT_EPSILON;

/// Flyback speed, deflection units per second. Fast, but finite — a real beam
/// takes time to cross the tube, and that travel is in the trace.
const BLANK_SPEED: f32 = 40_000.0;

/// Where every frame starts and ends.
const HOME: [f32; 2] = [0.0, 0.0];

/// One of the seven, with everything needed to build and name it.
#[derive(Clone, Copy, Debug)]
pub struct Pattern {
    pub number: u8,
    pub slug: &'static str,
    /// What it isolates, per the FIRST-SLICE.md §3 table.
    pub isolates: &'static str,
    /// Total fixture length.
    pub seconds: f32,
    /// Redraw rate. Pattern 6 depends on this being exactly 50.
    pub refresh_hz: f32,
}

pub const PATTERNS: [Pattern; 7] = [
    Pattern {
        number: 1,
        slug: "speed-ramp",
        isolates: "dwell-time brightness",
        seconds: 0.5,
        refresh_hz: 50.0,
    },
    Pattern {
        number: 2,
        slug: "parked-dots",
        isolates: "spot growth, saturation",
        seconds: 0.5,
        refresh_hz: 50.0,
    },
    Pattern {
        number: 3,
        slug: "square-corner",
        isolates: "renderer honesty at a corner",
        seconds: 0.5,
        refresh_hz: 50.0,
    },
    Pattern {
        number: 4,
        slug: "flash-decay",
        isolates: "tau_f, tau_s, chromaticities",
        seconds: 1.0,
        refresh_hz: 2.0,
    },
    Pattern {
        number: 5,
        slug: "blank-taper",
        isolates: "drive interpolation",
        seconds: 0.5,
        refresh_hz: 50.0,
    },
    Pattern {
        number: 6,
        slug: "refresh-beat",
        isolates: "substep correctness",
        seconds: 1.0,
        refresh_hz: 50.0,
    },
    Pattern {
        number: 7,
        slug: "lissajous-torture",
        isolates: "overall stability",
        seconds: 1.0,
        refresh_hz: 25.0,
    },
];

impl Pattern {
    pub fn file_name(&self) -> String {
        format!("pattern-{:02}-{}.btr0", self.number, self.slug)
    }

    pub fn producer_id(&self) -> String {
        format!("synthetic/{}", self.slug)
    }

    /// Generate the fixture.
    pub fn build(&self) -> Trace {
        let mut pen = Pen::new(HOME, EPSILON);
        let frame_seconds = 1.0 / self.refresh_hz;
        let frames = (self.seconds * self.refresh_hz).round().max(1.0) as usize;

        for _ in 0..frames {
            let started = pen.now();
            self.draw(&mut pen);

            // Wait for the next refresh, dark. A machine that finishes its
            // display list early sits idle until its frame interrupt, and that
            // idle time is exactly what the phosphor decays through.
            let next = started + frame_seconds;
            if pen.now() < next {
                pen.park([0.0; 3], next - pen.now());
            }
        }

        Trace {
            header: TraceHeader {
                epoch: 0.0,
                epsilon: EPSILON,
                nominal_refresh_hz: self.refresh_hz,
                producer_id: self.producer_id(),
            },
            samples: pen.into_samples(),
        }
    }

    fn draw(&self, pen: &mut Pen) {
        match self.number {
            1 => speed_ramp(pen),
            2 => parked_dots(pen),
            3 => square_corner(pen),
            4 => flash_decay(pen),
            5 => blank_taper(pen),
            6 => refresh_beat(pen),
            _ => lissajous_torture(pen),
        }
        pen.blank_to(HOME, BLANK_SPEED);
    }
}

/// 1 — the same stroke at four speeds, each four times the last, at one drive.
///
/// Drawn as four parallel lines because four brightnesses cannot be compared
/// if they are drawn on top of each other. Fastest at the bottom, so the
/// expected ordering reads down the screen.
fn speed_ramp(pen: &mut Pen) {
    const SPEEDS: [f32; 4] = [140.0, 560.0, 2240.0, 8960.0];
    const ROWS: [f32; 4] = [0.6, 0.2, -0.2, -0.6];

    for (row, speed) in ROWS.iter().zip(SPEEDS) {
        pen.blank_to([-0.7, *row], BLANK_SPEED);
        pen.stroke([0.7, *row], [1.0; 3], speed);
    }
}

/// 2 — stationary dots at drive 0.25, 1.0 and 4.0.
fn parked_dots(pen: &mut Pen) {
    const DOTS: [(f32, f32); 3] = [(-0.45, 0.25), (0.0, 1.0), (0.45, 4.0)];

    for (x, drive) in DOTS {
        pen.blank_to([x, 0.0], BLANK_SPEED);
        pen.park([drive; 3], 0.004);
    }
}

/// 3 — a square at high beam speed. Every corner is a 90° turn, and with a
/// synthetic trace it is genuinely sharp: this is the baseline that layer 2's
/// slew limiting will later round off.
fn square_corner(pen: &mut Pen) {
    const SPEED: f32 = 2000.0;
    const CORNERS: [[f32; 2]; 4] = [[0.4, -0.4], [0.4, 0.4], [-0.4, 0.4], [-0.4, -0.4]];

    pen.blank_to([-0.4, -0.4], BLANK_SPEED);
    for corner in CORNERS {
        pen.stroke(corner, [1.0; 3], SPEED);
    }
}

/// 4 — a bright full-screen X, then half a second of darkness.
///
/// The gap is where the two decay rates separate: a fast drop and a long tail,
/// warming in hue as the slow component outlives the fast one.
fn flash_decay(pen: &mut Pen) {
    const SPEED: f32 = 2000.0;
    const BRIGHT: [f32; 3] = [3.0; 3];

    pen.blank_to([-0.9, -0.9], BLANK_SPEED);
    pen.stroke([0.9, 0.9], BRIGHT, SPEED);
    pen.blank_to([-0.9, 0.9], BLANK_SPEED);
    pen.stroke([0.9, -0.9], BRIGHT, SPEED);
}

/// 5 — a stroke whose drive steps 1 → 0 partway along, over a few samples.
///
/// The beam keeps moving after the gun is off, so the trace continues as
/// zero-drive travel. Correct rendering tapers with no gap and no bright
/// terminal dot.
fn blank_taper(pen: &mut Pen) {
    const SPEED: f32 = 300.0;

    pen.blank_to([-0.7, 0.0], BLANK_SPEED);
    pen.stroke([0.2, 0.0], [1.0; 3], SPEED);
    pen.taper([0.5, 0.0], [0.0; 3], SPEED, 6);
    // Still moving, still dark, still in the trace.
    pen.blank_to([0.7, 0.0], SPEED);
}

/// 6 — a figure redrawn at exactly 50 Hz, against a host that is not 50 Hz.
fn refresh_beat(pen: &mut Pen) {
    const SPEED: f32 = 800.0;
    const CORNERS: [[f32; 2]; 4] = [[0.5, -0.5], [0.5, 0.5], [-0.5, 0.5], [-0.5, -0.5]];

    pen.blank_to([-0.5, -0.5], BLANK_SPEED);
    for corner in CORNERS {
        pen.stroke(corner, [1.0; 3], SPEED);
    }
}

/// 7 — a dense Lissajous at overdrive, one closed figure per frame.
fn lissajous_torture(pen: &mut Pen) {
    let figure = Lissajous {
        freq_x: 7.0,
        freq_y: 5.0,
        phase: TAU / 4.0,
        amplitude: 0.85,
        drive: 2.0,
        speed: 25.0,
    };
    let period = figure.period();

    pen.blank_to(figure.at(0.0).0, BLANK_SPEED);
    pen.curve(period, |u| figure.at(u * period));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fixture_survives_a_round_trip_through_the_format() {
        for pattern in PATTERNS {
            let trace = pattern.build();
            let bytes = beam_trace::encode(&trace.header, &trace.samples)
                .unwrap_or_else(|e| panic!("{}: {e}", pattern.slug));
            let read =
                beam_trace::decode(&bytes).unwrap_or_else(|e| panic!("{}: {e}", pattern.slug));
            assert_eq!(read.samples, trace.samples, "{}", pattern.slug);
            assert_eq!(read.header.producer_id, pattern.producer_id());
            assert_eq!(read.header.epsilon, EPSILON);
            assert_eq!(read.header.nominal_refresh_hz, pattern.refresh_hz);
        }
    }

    #[test]
    fn every_fixture_is_about_as_long_as_advertised() {
        for pattern in PATTERNS {
            let trace = pattern.build();
            let end = trace.samples.last().unwrap().t;
            let expected = pattern.seconds;
            assert!(
                (end - expected).abs() < expected * 0.05,
                "{} ran {end} s, expected about {expected}",
                pattern.slug
            );
        }
    }

    #[test]
    fn the_taper_pattern_contains_the_mid_stroke_drive_step() {
        let pattern = PATTERNS[4];
        assert_eq!(pattern.slug, "blank-taper");
        let trace = pattern.build();

        // Samples strictly between off and full drive: the transition itself.
        let mid: Vec<f32> = trace
            .samples
            .iter()
            .filter(|s| s.drive_r > 0.0 && s.drive_r < 1.0)
            .map(|s| s.drive_r)
            .collect();
        assert!(
            mid.len() >= 5,
            "expected several samples across the taper, found {}",
            mid.len()
        );

        // And it happens mid-stroke, not at an endpoint.
        let taper_x: Vec<f32> = trace
            .samples
            .iter()
            .filter(|s| s.drive_r > 0.0 && s.drive_r < 1.0)
            .map(|s| s.x)
            .collect();
        assert!(taper_x.iter().all(|x| *x > -0.7 && *x < 0.7));
    }

    #[test]
    fn the_torture_pattern_stays_within_epsilon_of_its_figure() {
        let figure = Lissajous {
            freq_x: 7.0,
            freq_y: 5.0,
            phase: TAU / 4.0,
            amplitude: 0.85,
            drive: 2.0,
            speed: 25.0,
        };
        let period = figure.period();

        let mut pen = Pen::new(figure.at(0.0).0, EPSILON);
        let start = pen.curve(period, |u| figure.at(u * period));
        let samples: Vec<_> = pen
            .samples()
            .iter()
            .filter(|s| s.t >= start)
            .copied()
            .collect();

        // Compare the emitted polyline against a dense evaluation of the
        // analytic figure, sampling inside every span.
        let mut worst = 0.0f32;
        for pair in samples.windows(2) {
            if pair[1].t <= pair[0].t {
                continue;
            }
            for step in 0..=8 {
                let f = step as f32 / 8.0;
                let t = pair[0].t + (pair[1].t - pair[0].t) * f;
                let (truth, _) = figure.at(t - start);
                let x = pair[0].x + (pair[1].x - pair[0].x) * f;
                let y = pair[0].y + (pair[1].y - pair[0].y) * f;
                worst = worst.max(((truth[0] - x).powi(2) + (truth[1] - y).powi(2)).sqrt());
            }
        }
        assert!(worst <= EPSILON, "worst deviation {worst}, ε is {EPSILON}");
    }

    #[test]
    fn sampling_is_adaptive_rather_than_a_fixed_rate() {
        // A square needs almost nothing; a dense Lissajous needs a great deal.
        // If the generator were emitting at a fixed rate these would be equal.
        let square = PATTERNS[5].build();
        let torture = PATTERNS[6].build();
        let rate = |trace: &Trace| trace.samples.len() as f32 / trace.samples.last().unwrap().t;
        assert!(
            rate(&torture) > rate(&square) * 20.0,
            "square {} samples/s, torture {} samples/s",
            rate(&square),
            rate(&torture)
        );
    }
}
