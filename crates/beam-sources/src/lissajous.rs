//! The general Lissajous generator — FIRST-SLICE.md §1 deliverable 4.
//!
//! Every field here is a live control: frequency ratio, phase, drive and beam
//! speed all take effect without restarting the source.

use std::f32::consts::TAU;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lissajous {
    /// Frequency ratio numerator; the figure's horizontal lobes.
    pub freq_x: f32,
    /// Frequency ratio denominator; the vertical lobes.
    pub freq_y: f32,
    /// Radians of x against y. A quarter turn opens a line into an ellipse.
    pub phase: f32,
    /// Deflection amplitude.
    pub amplitude: f32,
    /// Beam current, all three channels.
    pub drive: f32,
    /// Base cycles per second. This is the beam-speed control: the figure is
    /// the same shape however fast it is traced, and only the brightness
    /// changes — which is exactly the effect the renderer exists to model.
    pub speed: f32,
}

impl Default for Lissajous {
    fn default() -> Self {
        Self {
            freq_x: 3.0,
            freq_y: 2.0,
            phase: TAU / 4.0,
            amplitude: 0.8,
            drive: 1.0,
            speed: 5.0,
        }
    }
}

impl Lissajous {
    /// Position and drive at `seconds`.
    pub fn at(&self, seconds: f32) -> ([f32; 2], [f32; 3]) {
        let w = TAU * self.speed * seconds;
        (
            [
                self.amplitude * (self.freq_x * w + self.phase).sin(),
                self.amplitude * (self.freq_y * w).sin(),
            ],
            [self.drive; 3],
        )
    }

    /// How long one closed traversal of the figure takes, when the ratio is
    /// rational. The figure repeats every base cycle.
    pub fn period(&self) -> f32 {
        1.0 / self.speed.max(f32::EPSILON)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_figure_closes_after_one_base_period() {
        let figure = Lissajous::default();
        let (start, _) = figure.at(0.0);
        let (end, _) = figure.at(figure.period());
        assert!((start[0] - end[0]).abs() < 1e-4);
        assert!((start[1] - end[1]).abs() < 1e-4);
    }

    #[test]
    fn speed_changes_timing_but_not_the_shape() {
        let slow = Lissajous {
            speed: 2.0,
            ..Default::default()
        };
        let fast = Lissajous {
            speed: 20.0,
            ..Default::default()
        };
        // The same fraction through the figure is the same point on it.
        for step in 0..16 {
            let f = step as f32 / 16.0;
            let (a, _) = slow.at(f * slow.period());
            let (b, _) = fast.at(f * fast.period());
            assert!((a[0] - b[0]).abs() < 1e-3, "{a:?} vs {b:?}");
            assert!((a[1] - b[1]).abs() < 1e-3, "{a:?} vs {b:?}");
        }
    }
}
