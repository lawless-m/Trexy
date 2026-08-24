//! The command chain between the VIA's pins and the deflection amplifiers.
//!
//! Pure translation. Slew, droop, converter settling and the shape of a
//! blanking edge all belong to `beam_rasteriser`, which models the analogue
//! chain properly; this layer only says what the machine *asked for*
//! (ARCHITECTURE.md §2, §5). Keeping the two apart is what stops the same
//! physics being modelled twice with different constants.

use crate::via::Pins;

/// Port B bit assignments, as the reference emulator has them.
pub mod pb {
    /// Mux enable, active low.
    pub const MUX_DISABLE: u8 = 0x01;
    /// Mux channel select, two bits.
    pub const MUX_SELECT: u8 = 0x06;
    pub const MUX_SELECT_SHIFT: u32 = 1;
}

/// What the mux is routing the DAC to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Y-axis rate, held on a capacitor.
    Y,
    /// The zero reference the integrators settle against.
    ZeroReference,
    /// Z-axis: beam current.
    Z,
    /// The sound chip, which the beam does not care about.
    Sound,
}

impl Channel {
    fn of(port_b: u8) -> Self {
        match (port_b & pb::MUX_SELECT) >> pb::MUX_SELECT_SHIFT {
            0 => Channel::Y,
            1 => Channel::ZeroReference,
            2 => Channel::Z,
            _ => Channel::Sound,
        }
    }
}

/// What the machine has commanded, ready to become rasteriser events.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Command {
    /// X rate, full scale ±1. The DAC drives this directly.
    pub rx: f32,
    /// Y rate, full scale ±1, from the sample-and-hold.
    pub ry: f32,
    /// Beam current, 0..1, from the Z sample-and-hold.
    pub z: f32,
    /// True while the beam is lit.
    pub lit: bool,
    /// True on the cycle the integrators were dumped.
    pub zeroed: bool,
}

/// The DAC, the mux and the three sample-and-holds behind it.
#[derive(Clone, Debug, Default)]
pub struct Frontend {
    /// Value latched onto the Y hold capacitor.
    hold_y: f32,
    /// Value latched onto the Z hold capacitor.
    hold_z: f32,
    /// The offset both axes are measured against, latched like the others.
    /// Not a constant: the machine rewrites it constantly, and a stroke drawn
    /// against a stale one lands in the wrong place.
    hold_ref: f32,
    /// ~ZERO's level last cycle, so a pulse is reported once.
    zero_was_low: bool,
}

/// The DAC is 8 bits over a ±2.5 V swing, which is full-scale deflection
/// either way — so a code maps linearly onto ±1. The code is **signed**: zero
/// is the middle of the swing, not the bottom of it, which is why the machine
/// writes `0x00` when it wants the beam to stop moving.
fn dac(code: u8) -> f32 {
    f32::from(code as i8) / 128.0
}

impl Frontend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Translate one cycle's pin state.
    ///
    /// X comes straight off the converter while Y comes off a capacitor: that
    /// asymmetry is real, and it is why the rasteriser droops one axis and not
    /// the other (ARCHITECTURE.md §5).
    pub fn update(&mut self, pins: Pins) -> Command {
        let value = dac(pins.a);

        // The mux only latches while it is enabled.
        if pins.b & pb::MUX_DISABLE == 0 {
            match Channel::of(pins.b) {
                Channel::Y => self.hold_y = value,
                Channel::Z => self.hold_z = value,
                Channel::ZeroReference => self.hold_ref = value,
                Channel::Sound => {}
            }
        }

        // ~ZERO is active low, and the dump happens on the edge.
        let zero_low = !pins.ca2;
        let zeroed = zero_low && !self.zero_was_low;
        self.zero_was_low = zero_low;

        // ~RAMP is active low: while it is high the integrators hold, so
        // nothing is commanded however the converter is set.
        let ramping = !pins.pb7;
        // Both axes deflect by how far they sit from the shared reference —
        // the converter drives X live, while Y comes off its capacitor.
        Command {
            rx: if ramping { value - self.hold_ref } else { 0.0 },
            ry: if ramping { self.hold_y - self.hold_ref } else { 0.0 },
            // Z is a level, not a rate, so it survives RAMP being deasserted,
            // and only the positive half of the swing lights anything.
            z: self.hold_z.max(0.0),
            // ~BLANK: the line is the *un*-blanking one, so high is lit.
            lit: pins.cb2,
            zeroed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pins(a: u8, b: u8, ca2: bool, cb2: bool, pb7: bool) -> Pins {
        Pins { a, b, ca2, cb2, pb7 }
    }

    /// Mux enabled, routed to `channel`, ramping.
    fn routed(a: u8, channel: u8) -> Pins {
        pins(a, channel << pb::MUX_SELECT_SHIFT, true, true, false)
    }

    #[test]
    fn the_converter_drives_x_directly_and_y_through_a_latch() {
        let mut front = Frontend::new();
        let hot = front.update(routed(0x40, 0)); // route to Y
        assert!((hot.rx - 0.5).abs() < 1e-6, "x follows the converter now");
        assert!((hot.ry - 0.5).abs() < 1e-6, "y took the same value");

        // Move the converter with the mux pointed elsewhere: X follows, Y holds.
        let later = front.update(routed(0xC0, 3));
        assert!((later.rx + 0.5).abs() < 1e-6);
        assert!((later.ry - 0.5).abs() < 1e-6, "y is on a capacitor, not a wire");
    }

    #[test]
    fn a_disabled_mux_latches_nothing() {
        let mut front = Frontend::new();
        front.update(routed(0x40, 0));
        let held = front.update(pins(0x00, pb::MUX_DISABLE, true, true, false));
        assert!((held.ry - 0.5).abs() < 1e-6, "the latch must ignore a disabled mux");
    }

    #[test]
    fn both_axes_deflect_from_the_shared_reference() {
        let mut front = Frontend::new();
        front.update(routed(0x20, 1)); // the offset: +0.25
        front.update(routed(0x40, 0)); // Y: +0.5
        let out = front.update(routed(0x60, 3)); // X live: +0.75, mux parked
        assert!((out.rx - 0.5).abs() < 1e-6, "x is measured from the offset, not zero");
        assert!((out.ry - 0.25).abs() < 1e-6, "and so is y");
    }

    #[test]
    fn holding_ramp_commands_no_movement() {
        let mut front = Frontend::new();
        front.update(routed(0xFF, 0));
        let held = front.update(pins(0xFF, 0, true, true, true)); // ~RAMP high
        assert_eq!((held.rx, held.ry), (0.0, 0.0), "the integrators are not integrating");
    }

    #[test]
    fn the_intensity_latch_survives_ramp_being_released() {
        let mut front = Frontend::new();
        front.update(routed(0x7F, 2)); // route to Z
        let idle = front.update(pins(0x00, 0, true, true, true));
        assert!(idle.z > 0.9, "brightness is a level, not a rate");
    }

    #[test]
    fn only_the_positive_half_of_the_swing_lights_anything() {
        let mut front = Frontend::new();
        let dim = front.update(routed(0x81, 2)); // a negative intensity code
        assert_eq!(dim.z, 0.0, "below the reference the gun stays off");
    }

    #[test]
    fn the_beam_follows_the_unblanking_line_and_zero_is_active_low() {
        let mut front = Frontend::new();
        let dark = front.update(pins(0x00, 0, true, false, false));
        assert!(!dark.lit && !dark.zeroed);

        let lit = front.update(pins(0x00, 0, true, true, false));
        assert!(lit.lit, "the line un-blanks, so high is the beam on");
    }

    #[test]
    fn the_integrator_dump_is_reported_once_per_pulse() {
        let mut front = Frontend::new();
        let edge = front.update(pins(0x80, 0, false, true, false));
        assert!(edge.zeroed, "the falling edge dumps the integrators");

        let still = front.update(pins(0x80, 0, false, true, false));
        assert!(!still.zeroed, "holding the line low is not a second dump");

        front.update(pins(0x80, 0, true, true, false));
        let again = front.update(pins(0x80, 0, false, true, false));
        assert!(again.zeroed, "a fresh pulse dumps again");
    }

    #[test]
    fn a_zero_code_commands_no_deflection() {
        let mut front = Frontend::new();
        let centre = front.update(routed(0x00, 0));
        assert!(centre.rx.abs() < 1e-6, "mid-scale is the middle of the tube");
    }
}
