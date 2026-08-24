//! Turning commanded values into beam-rasteriser events.
//!
//! The event vocabulary is deliberately tiny (ARCHITECTURE.md §2), and this
//! layer keeps it that way: it emits an event only when what the machine is
//! asking for actually changes. Everything about how the hardware *responds* —
//! slew, droop, the converter settling, the shape of a blanking edge — belongs
//! to the rasteriser and is not repeated here.

use beam_rasteriser::Event;

use crate::analogue::{Command, Frontend as Analogue};
use crate::via::Pins;

/// The Vectrex's E clock.
pub const CLOCK_HZ: f64 = 1_500_000.0;

/// Watches the commanded values and emits events on the changes.
#[derive(Clone, Debug, Default)]
pub struct Frontend {
    analogue: Analogue,
    last: Option<Command>,
}

impl Frontend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert a cycle index to trace time.
    pub fn seconds(cycle: u64) -> f64 {
        cycle as f64 / CLOCK_HZ
    }

    /// Feed one machine cycle's pins, appending any events it caused.
    pub fn cycle(&mut self, cycle: u64, pins: Pins, out: &mut Vec<Event>) {
        let now = self.analogue.update(pins);
        let t = Self::seconds(cycle);

        // A dump is an instant, not a level: it is the one event that fires
        // even though nothing about the commanded rate changed.
        if now.zeroed {
            out.push(Event::Discontinuity { t, x: 0.0, y: 0.0 });
        }

        let changed = |f: fn(&Command) -> f32| {
            self.last.is_none_or(|was| f(&was) != f(&now))
        };
        if changed(|c| c.rx) || changed(|c| c.ry) {
            out.push(Event::Rate { t, rx: now.rx, ry: now.ry });
        }
        // Beam current is the blanking gate times the intensity level. The
        // Vectrex is monochrome, so the three channels are equal — the trace
        // carries three because Space Duel and Star Wars are not
        // (TRACE-FORMAT.md §1).
        let drive = if now.lit { now.z } else { 0.0 };
        let was_drive = self.last.map(|c| if c.lit { c.z } else { 0.0 });
        if was_drive != Some(drive) {
            out.push(Event::Current { t, drive: [drive; 3] });
        }

        self.last = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::via::{reg, Via};
    use beam_rasteriser::{Constants, Rasteriser};

    /// Run a VIA for `cycles`, feeding its pins through the front end.
    fn run(via: &mut Via, front: &mut Frontend, from: u64, cycles: u64, out: &mut Vec<Event>) -> u64 {
        let mut at = from;
        for _ in 0..cycles {
            front.cycle(at, via.pins(), out);
            via.step();
            at += 1;
        }
        at
    }

    /// A VIA set up the way the machine drives the deflection chain.
    fn wired() -> Via {
        let mut via = Via::new();
        via.write(reg::DDRA, 0xFF); // port A drives the DAC
        via.write(reg::DDRB, 0xFF);
        via.write(reg::PCR, 0b111 << 1 | 0b111 << 5); // ~ZERO and ~BLANK high
        via
    }

    #[test]
    fn a_stroke_is_rate_times_ramp_duration() {
        let mut via = wired();
        let mut front = Frontend::new();
        let mut events = Vec::new();

        // Latch a Y rate, set an X rate, then assert RAMP for a known time.
        via.write(reg::ORB, 0x00); // mux enabled, channel 0 = Y
        via.write(reg::ORA, 0x40); // +0.5
        let mut at = run(&mut via, &mut front, 0, 2, &mut events);
        via.write(reg::ORB, 0x06); // mux to the sound line: Y now holds
        via.write(reg::ORA, 0x40); // X also +0.5

        // ~RAMP is PB7 under the timer; drive it directly instead.
        via.write(reg::ORB, 0x06); // PB7 low = ramping
        at = run(&mut via, &mut front, at, 1500, &mut events);
        via.write(reg::ORB, 0x86); // PB7 high = hold
        run(&mut via, &mut front, at, 4, &mut events);

        let mut r = Rasteriser::new(Constants::ideal(), beam_trace::DEFAULT_EPSILON);
        let mut samples = Vec::new();
        for e in &events {
            r.push(*e, &mut samples);
        }
        r.run_to(Frontend::seconds(at + 8), &mut samples);

        let (x, y) = r.position();
        // 1500 cycles at 1.5 MHz is 1 ms; at rate 0.5 and unit gain that is
        // 0.5 ms-units of deflection on each axis.
        let expected = 0.5 * (1500.0 / CLOCK_HZ) as f32;
        assert!((x - expected).abs() < expected * 0.05, "x travelled {x}, expected ~{expected}");
        assert!((y - expected).abs() < expected * 0.05, "y travelled {y}, expected ~{expected}");
    }

    #[test]
    fn a_zero_pulse_breaks_the_trace_exactly_once() {
        let mut via = wired();
        let mut front = Frontend::new();
        let mut events = Vec::new();
        let mut at = run(&mut via, &mut front, 0, 2, &mut events);

        via.write(reg::PCR, 0b110 << 1 | 0b111 << 5); // ~ZERO low
        at = run(&mut via, &mut front, at, 3, &mut events);
        via.write(reg::PCR, 0b111 << 1 | 0b111 << 5); // and back up
        run(&mut via, &mut front, at, 3, &mut events);

        let breaks = events
            .iter()
            .filter(|e| matches!(e, Event::Discontinuity { .. }))
            .count();
        assert_eq!(breaks, 1, "a pulse is one dump, however long it is held");
    }

    #[test]
    fn the_shift_register_chops_the_beam_into_dashes() {
        // The dash mechanism: ~BLANK follows the shifted pattern, so one
        // stroke comes out as a row of segments without the CPU intervening.
        let mut via = wired();
        let mut front = Frontend::new();
        let mut events = Vec::new();

        // Load the intensity hold first: blanking gates the beam, but the
        // brightness it gates comes from the Z sample-and-hold.
        via.write(reg::ORB, 0x04); // mux channel 2 = Z
        via.write(reg::ORA, 0x7F); // full brightness
        let at = run(&mut via, &mut front, 0, 2, &mut events);
        via.write(reg::ORB, 0x06); // mux away so Z holds

        via.write(reg::ACR, 0x18);
        via.write(reg::SR, 0b1010_1010);
        run(&mut via, &mut front, at, 40, &mut events);

        let mut lit = 0;
        let mut dark = 0;
        for e in &events {
            if let Event::Current { drive, .. } = e {
                if drive[0] > 0.0 {
                    lit += 1;
                } else {
                    dark += 1;
                }
            }
        }
        assert!(lit >= 3 && dark >= 3, "expected alternating spans, got {lit} lit and {dark} dark");
    }

    #[test]
    fn events_are_emitted_only_when_the_command_changes() {
        let mut via = wired();
        let mut front = Frontend::new();
        let mut events = Vec::new();
        via.write(reg::ORB, 0x06);
        via.write(reg::ORA, 0x80);
        run(&mut via, &mut front, 0, 200, &mut events);
        assert!(
            events.len() <= 3,
            "a steady machine should be nearly silent, got {} events",
            events.len()
        );
    }

    #[test]
    fn the_emitted_trace_survives_a_round_trip() {
        let mut via = wired();
        let mut front = Frontend::new();
        let mut events = Vec::new();
        via.write(reg::ORB, 0x04);
        via.write(reg::ORA, 0xFF);
        let at = run(&mut via, &mut front, 0, 2, &mut events);
        via.write(reg::ORB, 0x06);

        via.write(reg::ACR, 0x18);
        via.write(reg::SR, 0b1100_1100);
        via.write(reg::ORA, 0xA0);
        let at = run(&mut via, &mut front, at, 300, &mut events);

        let mut r = Rasteriser::new(Constants::vectrex(), beam_trace::DEFAULT_EPSILON);
        let mut samples = Vec::new();
        for e in &events {
            r.push(*e, &mut samples);
        }
        r.run_to(Frontend::seconds(at), &mut samples);

        for pair in samples.windows(2) {
            assert!(pair[1].t > pair[0].t, "{} did not exceed {}", pair[1].t, pair[0].t);
        }
        let path = std::env::temp_dir().join(format!("vectrex-frontend-{}.btr0", std::process::id()));
        let header = beam_trace::TraceHeader {
            epoch: 0.0,
            epsilon: beam_trace::DEFAULT_EPSILON,
            nominal_refresh_hz: 0.0,
            producer_id: "vectrex/via".to_owned(),
        };
        beam_trace::write_file(&path, &header, &samples).expect("write");
        let back = beam_trace::read_file(&path).expect("the loader validates what it reads");
        assert_eq!(back.samples.len(), samples.len());
        let _ = std::fs::remove_file(&path);
    }
}
