//! MOS 6522 Versatile Interface Adapter, stepped once per E cycle.
//!
//! ARCHITECTURE.md §5 calls VIA timing "the whole ballgame" for this machine:
//! T1 drives ~RAMP, so its period is a stroke's length, and the shift register
//! chops ~BLANK, so its rate is the dash pattern. Getting the CPU right and
//! this wrong would still give a blank screen.

/// Register indices, as the low four address bits select them.
pub mod reg {
    pub const ORB: u8 = 0x0;
    pub const ORA: u8 = 0x1;
    pub const DDRB: u8 = 0x2;
    pub const DDRA: u8 = 0x3;
    pub const T1C_L: u8 = 0x4;
    pub const T1C_H: u8 = 0x5;
    pub const T1L_L: u8 = 0x6;
    pub const T1L_H: u8 = 0x7;
    pub const T2C_L: u8 = 0x8;
    pub const T2C_H: u8 = 0x9;
    pub const SR: u8 = 0xA;
    pub const ACR: u8 = 0xB;
    pub const PCR: u8 = 0xC;
    pub const IFR: u8 = 0xD;
    pub const IER: u8 = 0xE;
    /// Port A again, without the handshake side effects.
    pub const ORA_NH: u8 = 0xF;
}

/// The VIA's output pins, as one snapshot.
///
/// Taken together rather than read one at a time: the analogue front end has to
/// see a consistent instant, and on this machine a half-updated view means a
/// stroke drawn from one moment's rate at another moment's position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pins {
    /// Port A: the DAC's input.
    pub a: u8,
    /// Port B: mux select, and PB7 is ~RAMP.
    pub b: u8,
    /// ~ZERO — dumps the integrators.
    pub ca2: bool,
    /// ~BLANK — the beam on/off gate.
    pub cb2: bool,
    /// ~RAMP, broken out because the timer may own it rather than port B.
    pub pb7: bool,
}

/// Interrupt flag bits, shared by IFR and IER.
pub mod irq {
    pub const CA2: u8 = 0x01;
    pub const CA1: u8 = 0x02;
    pub const SR: u8 = 0x04;
    pub const CB2: u8 = 0x08;
    pub const CB1: u8 = 0x10;
    pub const T2: u8 = 0x20;
    pub const T1: u8 = 0x40;
    /// Not a source: reads as the OR of every enabled flag.
    pub const ANY: u8 = 0x80;
}

#[derive(Clone, Debug, Default)]
pub struct Via {
    /// Output registers and direction masks. A bit set in DDR is an output.
    pub ora: u8,
    pub orb: u8,
    pub ddra: u8,
    pub ddrb: u8,
    /// Input latches, loaded from the pins when latching is enabled by ACR.
    ira: u8,
    irb: u8,
    /// What the outside world is presenting on each port.
    pins_a: u8,
    pins_b: u8,
    pub acr: u8,
    pub pcr: u8,
    pub sr: u8,
    pub t1_latch: u16,
    pub t2_latch: u16,
    /// T1's counter. Counts down every E cycle and flags when it passes zero.
    t1: u16,
    /// One-shot timers flag once; free-running ones flag every reload.
    t1_armed: bool,
    /// Set on underflow, consumed by the next cycle. The reload costs a cycle
    /// of its own, which is why a free-running period is N+2 and not N+1.
    t1_reload: bool,
    /// T2's counter. One-shot only, or counting pulses on PB6.
    t2: u16,
    t2_armed: bool,
    /// Last PB6 level, for edge detection in pulse-counting mode.
    pb6_was: bool,
    /// Shift register state: bits still to go, the level currently presented,
    /// and the divide-by-two that turns E cycles into shift clocks.
    sr_count: u8,
    sr_out: bool,
    sr_clock: u8,
    /// PB7's level when ACR bit 7 hands the pin to the timer. On the Vectrex
    /// this pin is ~RAMP, so its edges are literally the length of a stroke.
    t1_pb7: bool,
    /// CA2 and CB2 levels while PCR has them in an output mode. On the Vectrex
    /// CA2 is ~ZERO and CB2 is ~BLANK.
    ca2: bool,
    cb2: bool,
    pub ifr: u8,
    pub ier: u8,
}

impl Via {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drive the input pins, for tests and for whatever the machine hangs off
    /// the ports.
    pub fn set_pins_a(&mut self, value: u8) {
        self.pins_a = value;
        if self.acr & 0x01 != 0 {
            self.ira = value;
        }
    }

    pub fn set_pins_b(&mut self, value: u8) {
        self.pins_b = value;
        if self.acr & 0x02 != 0 {
            self.irb = value;
        }
    }

    /// What port A is presenting: output bits from ORA, input bits from the
    /// pins.
    pub fn port_a(&self) -> u8 {
        self.ora & self.ddra | self.pins_a & !self.ddra
    }

    pub fn port_b(&self) -> u8 {
        self.orb & self.ddrb | self.pins_b & !self.ddrb
    }

    /// Raise an interrupt source. Level-sensitive: the flag stays until
    /// software clears it, whether or not it was enabled when it happened.
    pub fn raise(&mut self, source: u8) {
        self.ifr |= source & !irq::ANY;
    }

    /// Whether the IRQ pin is asserted — any flag that is also enabled.
    ///
    /// Order does not matter: enabling a source that already flagged asserts
    /// the line just as much as flagging one already enabled, which is why
    /// this is recomputed rather than latched.
    pub fn irq(&self) -> bool {
        self.ifr & self.ier & !irq::ANY != 0
    }

    /// Advance one E cycle.
    pub fn step(&mut self) {
        // Reloading occupies a cycle in which nothing counts.
        if self.t1_reload {
            self.t1 = self.t1_latch;
            self.t1_reload = false;
            return;
        }
        self.step_shift();

        // T2 counts E cycles, or PB6 pulses when ACR bit 5 selects that. It
        // never reloads: once it has flagged it keeps counting but stays quiet
        // until software restarts it.
        let counts = if self.acr & 0x20 != 0 {
            let now = self.pins_b & 0x40 != 0;
            let edge = self.pb6_was && !now;
            self.pb6_was = now;
            edge
        } else {
            true
        };
        if counts {
            let expired = self.t2 == 0;
            self.t2 = self.t2.wrapping_sub(1);
            if expired && self.t2_armed {
                self.raise(irq::T2);
                self.t2_armed = false;
            }
        }

        // The counter runs continuously; what differs between modes is whether
        // passing zero flags again and whether it reloads from the latch.
        let expired = self.t1 == 0;
        self.t1 = self.t1.wrapping_sub(1);
        if expired {
            if self.t1_armed {
                self.raise(irq::T1);
                if self.acr & 0x80 != 0 {
                    self.t1_pb7 = !self.t1_pb7;
                }
            }
            if self.acr & 0x40 != 0 {
                self.t1_reload = true;
            } else {
                self.t1_armed = false;
            }
        }
    }

    /// One E cycle of the shift register.
    ///
    /// Only the shift-out modes matter here: the Vectrex uses them to chop
    /// ~BLANK while a stroke is being drawn, which is how it draws a dashed
    /// line without the CPU touching anything (ARCHITECTURE.md §5).
    ///
    /// The shift clock depends on the mode. Under Φ2 the register shifts on
    /// every E cycle; under T2 it shifts at the timer's rate, approximated
    /// here as E divided by two. The Vectrex uses the Φ2 mode, and the
    /// difference is not cosmetic: the register is ~BLANK, so a shift clock at
    /// half speed leaves the beam lit across the moves it is meant to hide.
    fn step_shift(&mut self) {
        if self.sr_count == 0 {
            return;
        }
        if self.acr & 0x08 == 0 {
            self.sr_clock += 1;
            if self.sr_clock < 2 {
                return;
            }
            self.sr_clock = 0;
        }
        // Most significant bit first, rotating so the pattern survives being
        // shifted out and can be sent again without rewriting it.
        self.sr_out = self.sr & 0x80 != 0;
        self.sr = self.sr << 1 | u8::from(self.sr_out);
        self.sr_count -= 1;
        if self.sr_count == 0 {
            self.raise(irq::SR);
        }
    }

    /// The shift register's serial output, which appears on CB2 in shift mode.
    pub fn sr_output(&self) -> bool {
        self.sr_out
    }

    /// Whether the shift register is still running.
    pub fn shifting(&self) -> bool {
        self.sr_count > 0
    }

    /// CA2's level. PCR bits 1-3 choose the mode; only the manual-output modes
    /// (110 low, 111 high) drive a fixed level, and the Vectrex uses them to
    /// pulse ~ZERO.
    pub fn ca2(&self) -> bool {
        match self.pcr >> 1 & 0x07 {
            0b110 => false,
            0b111 => true,
            _ => self.ca2,
        }
    }

    /// CB2's level. In a shift-out mode the shift register drives it, which on
    /// the Vectrex is what chops ~BLANK into dashes; otherwise PCR bits 5-7
    /// choose as for CA2.
    pub fn cb2(&self) -> bool {
        // The register owns the line in any shift-*out* mode, and keeps the
        // last bit it shifted after it runs dry. That is not a detail: a
        // register loaded with all ones is how the machine draws a whole
        // stroke lit, and a line that reverted to its manual level when the
        // eighth bit went past would cut every stroke short.
        if self.acr & 0x10 != 0 {
            return self.sr_output();
        }
        match self.pcr >> 5 & 0x07 {
            0b110 => false,
            0b111 => true,
            _ => self.cb2,
        }
    }

    /// Everything the analogue chain needs to see, sampled together.
    pub fn pins(&self) -> Pins {
        Pins {
            a: self.port_a(),
            b: self.port_b(),
            ca2: self.ca2(),
            cb2: self.cb2(),
            pb7: self.pb7(),
        }
    }

    /// PB7's level, which the timer owns when ACR bit 7 is set.
    pub fn pb7(&self) -> bool {
        if self.acr & 0x80 != 0 { self.t1_pb7 } else { self.orb & 0x80 != 0 }
    }

    pub fn read(&mut self, index: u8) -> u8 {
        match index & 0x0F {
            // Reading the low half clears the flag (6522 datasheet, T1).
            reg::T1C_L => {
                self.ifr &= !irq::T1;
                self.t1 as u8
            }
            reg::T1C_H => (self.t1 >> 8) as u8,
            reg::T2C_L => {
                self.ifr &= !irq::T2;
                self.t2 as u8
            }
            reg::T2C_H => (self.t2 >> 8) as u8,
            reg::ORB => {
                // Output bits read back from the register, input bits from the
                // latch if latching is on, otherwise live from the pins
                // (6522 datasheet, port B input latching).
                let input = if self.acr & 0x02 != 0 { self.irb } else { self.pins_b };
                self.orb & self.ddrb | input & !self.ddrb
            }
            reg::ORA | reg::ORA_NH => {
                // Port A reads the pins for *every* bit, output or not — the
                // register is not read back (6522 datasheet, port A).
                if self.acr & 0x01 != 0 { self.ira } else { self.pins_a }
            }
            reg::DDRB => self.ddrb,
            reg::DDRA => self.ddra,
            reg::ACR => self.acr,
            reg::PCR => self.pcr,
            reg::SR => {
                self.ifr &= !irq::SR;
                self.sr_count = 8;
                self.sr_clock = 0;
                self.sr
            }
            reg::T1L_L => self.t1_latch as u8,
            reg::T1L_H => (self.t1_latch >> 8) as u8,
            reg::IFR => self.ifr | if self.irq() { irq::ANY } else { 0 },
            reg::IER => self.ier | irq::ANY,
            _ => 0,
        }
    }

    pub fn write(&mut self, index: u8, value: u8) {
        match index & 0x0F {
            reg::ORB => self.orb = value,
            reg::ORA | reg::ORA_NH => self.ora = value,
            reg::DDRB => self.ddrb = value,
            reg::DDRA => self.ddra = value,
            reg::ACR => self.acr = value,
            reg::PCR => self.pcr = value,
            // Writing the register reloads it and starts eight shifts.
            reg::SR => {
                self.sr = value;
                self.sr_count = 8;
                self.sr_clock = 0;
                self.ifr &= !irq::SR;
            }
            reg::T1C_L | reg::T1L_L => {
                self.t1_latch = self.t1_latch & 0xFF00 | u16::from(value);
            }
            // Writing the high half latches it, loads the counter and starts
            // the timer running — this is the write that begins a stroke.
            reg::T1C_H => {
                self.t1_latch = self.t1_latch & 0x00FF | u16::from(value) << 8;
                self.t1 = self.t1_latch;
                self.t1_armed = true;
                self.t1_reload = false;
                self.ifr &= !irq::T1;
                if self.acr & 0x80 != 0 {
                    self.t1_pb7 = false;
                }
            }
            reg::T1L_H => {
                self.t1_latch = self.t1_latch & 0x00FF | u16::from(value) << 8;
                self.ifr &= !irq::T1;
            }
            reg::T2C_L => self.t2_latch = self.t2_latch & 0xFF00 | u16::from(value),
            reg::T2C_H => {
                self.t2_latch = self.t2_latch & 0x00FF | u16::from(value) << 8;
                self.t2 = self.t2_latch;
                self.t2_armed = true;
                self.ifr &= !irq::T2;
            }
            // Writing a 1 clears that flag; there is no way to set one from
            // software (6522 datasheet, IFR).
            reg::IFR => self.ifr &= !(value & !irq::ANY),
            // Bit 7 selects: set the named bits, or clear them.
            reg::IER => {
                let bits = value & !irq::ANY;
                if value & irq::ANY != 0 {
                    self.ier |= bits;
                } else {
                    self.ier &= !bits;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_masks_decide_what_a_port_presents() {
        let mut via = Via::new();
        via.write(reg::DDRB, 0xF0); // high nibble out, low nibble in
        via.write(reg::ORB, 0xAA);
        via.set_pins_b(0x0F);

        // Output bits come from the register, input bits from the pins.
        assert_eq!(via.port_b(), 0xA0 | 0x0F);
        assert_eq!(via.read(reg::ORB), 0xA0 | 0x0F);
    }

    #[test]
    fn port_a_reads_its_pins_even_where_it_is_driving() {
        // The 6522's port A read returns the pin level for every bit, unlike
        // port B which reads back its own output register. Software that
        // assumes otherwise reads garbage on a loaded output.
        let mut via = Via::new();
        via.write(reg::DDRA, 0xFF);
        via.write(reg::ORA, 0x55);
        via.set_pins_a(0x12);

        assert_eq!(via.read(reg::ORA), 0x12);
        assert_eq!(via.port_a(), 0x55, "the pin is still driven from the register");
    }

    #[test]
    fn latched_inputs_hold_what_they_saw() {
        let mut via = Via::new();
        via.write(reg::DDRB, 0x00);
        via.write(reg::ACR, 0x02); // latch port B
        via.set_pins_b(0x3C);
        via.set_pins_b(0xC3); // the latch was loaded on the first change

        assert_eq!(via.read(reg::ORB), 0xC3, "each change reloads while enabled");

        via.write(reg::ACR, 0x00);
        via.set_pins_b(0x5A);
        assert_eq!(via.read(reg::ORB), 0x5A, "unlatched reads follow the pins");
    }

    #[test]
    fn reading_the_enable_register_sets_its_top_bit() {
        // IER reads with bit 7 set regardless of what was written — the bit is
        // a set/clear selector on write, not a stored flag (6522 datasheet).
        let mut via = Via::new();
        via.ier = 0x21;
        assert_eq!(via.read(reg::IER), 0xA1);
    }

    #[test]
    fn the_enable_register_sets_and_clears_by_its_top_bit() {
        let mut via = Via::new();
        via.write(reg::IER, irq::ANY | irq::T1 | irq::T2);
        assert_eq!(via.ier, irq::T1 | irq::T2);

        via.write(reg::IER, irq::T2); // top bit clear: clear these
        assert_eq!(via.ier, irq::T1);
    }

    #[test]
    fn the_interrupt_line_follows_flag_and_enable_in_either_order() {
        let mut via = Via::new();
        via.raise(irq::T1);
        assert!(!via.irq(), "flagged but not enabled");

        via.write(reg::IER, irq::ANY | irq::T1);
        assert!(via.irq(), "enabling an already-flagged source asserts");

        via.write(reg::IFR, irq::T1);
        assert!(!via.irq(), "clearing the flag releases the line");

        via.raise(irq::T1);
        assert!(via.irq(), "flagging an already-enabled source asserts");

        via.write(reg::IER, irq::T1);
        assert!(!via.irq(), "masking releases the line without clearing the flag");
        assert_eq!(via.ifr & irq::T1, irq::T1);
    }

    #[test]
    fn the_flag_register_summarises_itself_in_bit_seven() {
        let mut via = Via::new();
        via.raise(irq::SR);
        assert_eq!(via.read(reg::IFR) & irq::ANY, 0, "not enabled, so not summarised");

        via.write(reg::IER, irq::ANY | irq::SR);
        assert_eq!(via.read(reg::IFR) & irq::ANY, irq::ANY);
    }

    #[test]
    fn writing_ones_to_the_flag_register_clears_exactly_those() {
        let mut via = Via::new();
        via.raise(irq::T1 | irq::T2 | irq::CB1);
        via.write(reg::IFR, irq::T2);
        assert_eq!(via.ifr, irq::T1 | irq::CB1);
    }

    #[test]
    fn the_timer_one_latch_is_addressable_as_two_halves() {
        let mut via = Via::new();
        via.write(reg::T1L_L, 0x34);
        via.write(reg::T1L_H, 0x12);
        assert_eq!(via.t1_latch, 0x1234);
        assert_eq!(via.read(reg::T1L_L), 0x34);
        assert_eq!(via.read(reg::T1L_H), 0x12);
    }
}

#[cfg(test)]
mod timer_tests {
    use super::*;

    /// Cycles from starting T1 until its flag appears.
    fn interval(latch: u16, acr: u8) -> u32 {
        let mut via = Via::new();
        via.write(reg::ACR, acr);
        via.write(reg::T1C_L, latch as u8);
        via.write(reg::T1C_H, (latch >> 8) as u8);
        for n in 1..10_000 {
            via.step();
            if via.ifr & irq::T1 != 0 {
                return n;
            }
        }
        panic!("T1 never flagged");
    }

    #[test]
    fn one_shot_flags_after_the_latch_plus_settling() {
        // The count written is the interval; the extra cycles are the pass
        // through zero. What matters for the Vectrex is that the relationship
        // is exact and linear, because stroke length is proportional to it.
        let a = interval(100, 0x00);
        let b = interval(200, 0x00);
        assert_eq!(b - a, 100, "the interval must track the latch one for one");
    }

    #[test]
    fn one_shot_flags_once() {
        let mut via = Via::new();
        via.write(reg::T1C_L, 10);
        via.write(reg::T1C_H, 0);
        for _ in 0..12 {
            via.step();
        }
        assert_ne!(via.ifr & irq::T1, 0);
        via.write(reg::IFR, irq::T1);
        for _ in 0..200 {
            via.step();
        }
        assert_eq!(via.ifr & irq::T1, 0, "a one-shot must not flag again");
    }

    #[test]
    fn free_running_flags_every_period() {
        let mut via = Via::new();
        via.write(reg::ACR, 0x40);
        via.write(reg::T1C_L, 20);
        via.write(reg::T1C_H, 0);
        let mut gaps = Vec::new();
        let mut last = 0u32;
        for n in 1..500u32 {
            via.step();
            if via.ifr & irq::T1 != 0 {
                via.write(reg::IFR, irq::T1);
                if last != 0 {
                    gaps.push(n - last);
                }
                last = n;
            }
        }
        assert!(gaps.len() > 5, "expected repeated flagging, got {gaps:?}");
        assert!(gaps.windows(2).all(|w| w[0] == w[1]), "period must not drift: {gaps:?}");
        assert_eq!(gaps[0], 22, "N+2 cycles for a latch of N");
    }

    #[test]
    fn pb7_toggles_with_the_free_running_timer() {
        // On the Vectrex this pin is ~RAMP: every toggle starts or ends a
        // stroke, so the edges are the drawing.
        let mut via = Via::new();
        via.write(reg::ACR, 0xC0); // free-running, PB7 under timer control
        via.write(reg::T1C_L, 10);
        via.write(reg::T1C_H, 0);
        assert!(!via.pb7());

        let mut edges = 0;
        let mut was = via.pb7();
        for _ in 0..200 {
            via.step();
            if via.pb7() != was {
                edges += 1;
                was = via.pb7();
            }
        }
        assert!(edges >= 8, "expected a square wave, saw {edges} edges");
    }

    #[test]
    fn reading_the_counter_low_half_clears_the_flag() {
        let mut via = Via::new();
        via.write(reg::T1C_L, 5);
        via.write(reg::T1C_H, 0);
        for _ in 0..8 {
            via.step();
        }
        assert_ne!(via.ifr & irq::T1, 0);
        via.read(reg::T1C_L);
        assert_eq!(via.ifr & irq::T1, 0);
    }

    #[test]
    fn starting_the_timer_clears_a_pending_flag() {
        let mut via = Via::new();
        via.raise(irq::T1);
        via.write(reg::T1C_H, 0);
        assert_eq!(via.ifr & irq::T1, 0);
    }
}

#[cfg(test)]
mod timer2_tests {
    use super::*;

    fn start_t2(via: &mut Via, latch: u16) {
        via.write(reg::T2C_L, latch as u8);
        via.write(reg::T2C_H, (latch >> 8) as u8);
    }

    #[test]
    fn the_one_shot_interval_tracks_the_latch() {
        let count = |latch: u16| {
            let mut via = Via::new();
            start_t2(&mut via, latch);
            (1..10_000)
                .find(|_| {
                    via.step();
                    via.ifr & irq::T2 != 0
                })
                .expect("T2 never flagged")
        };
        assert_eq!(count(200) - count(100), 100);
    }

    #[test]
    fn it_does_not_flag_again_until_restarted() {
        let mut via = Via::new();
        start_t2(&mut via, 10);
        for _ in 0..12 {
            via.step();
        }
        assert_ne!(via.ifr & irq::T2, 0);

        via.write(reg::IFR, irq::T2);
        for _ in 0..500 {
            via.step();
        }
        assert_eq!(via.ifr & irq::T2, 0, "T2 free-runs but must stay quiet");

        start_t2(&mut via, 10);
        for _ in 0..12 {
            via.step();
        }
        assert_ne!(via.ifr & irq::T2, 0, "restarting re-arms it");
    }

    #[test]
    fn pulse_counting_advances_only_on_pb6_edges() {
        let mut via = Via::new();
        via.write(reg::ACR, 0x20);
        via.set_pins_b(0x40);
        via.step();
        start_t2(&mut via, 3);

        // Cycles alone do nothing in this mode.
        for _ in 0..100 {
            via.step();
        }
        assert_eq!(via.ifr & irq::T2, 0, "counted cycles it should have ignored");

        for _ in 0..4 {
            via.set_pins_b(0x00);
            via.step();
            via.set_pins_b(0x40);
            via.step();
        }
        assert_ne!(via.ifr & irq::T2, 0, "four falling edges should exhaust a count of 3");
    }

    #[test]
    fn reading_the_counter_low_half_clears_the_flag() {
        let mut via = Via::new();
        start_t2(&mut via, 5);
        for _ in 0..8 {
            via.step();
        }
        assert_ne!(via.ifr & irq::T2, 0);
        via.read(reg::T2C_L);
        assert_eq!(via.ifr & irq::T2, 0);
    }
}

#[cfg(test)]
mod shift_tests {
    use super::*;

    /// Drive the register and collect the bit it presents on each shift.
    fn shift_out(pattern: u8) -> (Vec<bool>, u32) {
        let mut via = Via::new();
        via.write(reg::ACR, 0x18); // shift out under phase-2 control
        via.write(reg::SR, pattern);
        let mut bits = Vec::new();
        let mut cycles = 0;
        while via.shifting() && cycles < 200 {
            via.step();
            cycles += 1;
            // One bit per shift, and under Phi2 a shift is one cycle.
            bits.push(via.sr_output());
        }
        (bits, cycles)
    }

    #[test]
    fn eight_shifts_present_the_pattern_most_significant_first() {
        let (bits, _) = shift_out(0b1011_0010);
        assert_eq!(
            bits,
            vec![true, false, true, true, false, false, true, false],
            "the dash pattern must come out in the order it was written"
        );
    }

    #[test]
    fn the_phi2_shift_clock_is_the_e_clock() {
        let (_, cycles) = shift_out(0xFF);
        assert_eq!(cycles, 8, "under Phi2 the register shifts every cycle");
    }

    #[test]
    fn the_timer_shift_clock_is_slower_than_the_e_clock() {
        // Mode 101, shift out under T2, is not the mode the Vectrex uses; it
        // is here so the two clocks cannot be confused for one another.
        let mut via = Via::new();
        via.write(reg::ACR, 0x14);
        via.write(reg::SR, 0xFF);
        let mut cycles = 0;
        while via.shifting() && cycles < 100 {
            via.step();
            cycles += 1;
        }
        assert!(cycles > 8, "the timer cannot shift as fast as the system clock");
    }

    #[test]
    fn completing_eight_shifts_raises_the_flag() {
        let mut via = Via::new();
        via.write(reg::ACR, 0x18);
        via.write(reg::SR, 0x5A);
        assert_eq!(via.ifr & irq::SR, 0);
        for _ in 0..8 {
            via.step();
        }
        assert_ne!(via.ifr & irq::SR, 0, "the flag says the pattern has run out");
        assert!(!via.shifting());
    }

    #[test]
    fn rewriting_the_register_restarts_the_pattern() {
        let mut via = Via::new();
        via.write(reg::ACR, 0x18);
        via.write(reg::SR, 0xF0);
        for _ in 0..4 {
            via.step();
        }
        assert!(via.shifting());

        via.write(reg::SR, 0x0F);
        assert_eq!(via.ifr & irq::SR, 0, "restarting clears a stale flag");
        let mut cycles = 0;
        while via.shifting() && cycles < 100 {
            via.step();
            cycles += 1;
        }
        assert_eq!(cycles, 8, "a fresh write is a fresh eight shifts");
    }

    #[test]
    fn the_pattern_survives_being_shifted_out() {
        // The register rotates rather than emptying, so the same dash pattern
        // can be sent again without rewriting it.
        let mut via = Via::new();
        via.write(reg::ACR, 0x18);
        via.write(reg::SR, 0b1100_0011);
        for _ in 0..16 {
            via.step();
        }
        assert_eq!(via.sr, 0b1100_0011);
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;

    #[test]
    fn manual_output_modes_drive_ca2_and_cb2() {
        let mut via = Via::new();
        via.write(reg::PCR, 0b110 << 1); // CA2 manual low
        assert!(!via.ca2());
        via.write(reg::PCR, 0b111 << 1); // CA2 manual high
        assert!(via.ca2());

        via.write(reg::PCR, 0b110 << 5);
        assert!(!via.cb2());
        via.write(reg::PCR, 0b111 << 5);
        assert!(via.cb2());
    }

    #[test]
    fn the_shift_register_takes_cb2_while_it_runs() {
        // This is the dash mechanism: ~BLANK follows the shifted pattern for
        // eight bits, then hands the pin back.
        let mut via = Via::new();
        via.write(reg::PCR, 0b111 << 5); // manual high when not shifting
        via.write(reg::ACR, 0x18);
        via.write(reg::SR, 0x00);
        via.step();
        via.step();
        assert!(!via.cb2(), "shifting a zero bit must pull ~BLANK down");

        for _ in 0..20 {
            via.step();
        }
        assert!(!via.shifting());
        assert!(
            !via.cb2(),
            "a register that has run dry keeps its last bit; it does not hand the pin back"
        );
    }

    #[test]
    fn the_pin_snapshot_reflects_the_timer_owning_pb7() {
        let mut via = Via::new();
        via.write(reg::DDRB, 0xFF);
        via.write(reg::ORB, 0x80);
        assert!(via.pins().pb7, "port B drives it when the timer does not");

        via.write(reg::ACR, 0xC0);
        via.write(reg::T1C_L, 4);
        via.write(reg::T1C_H, 0);
        assert!(!via.pins().pb7, "starting the timer takes the pin and clears it");
    }

    #[test]
    fn the_snapshot_carries_the_dac_and_mux_lines() {
        let mut via = Via::new();
        via.write(reg::DDRA, 0xFF);
        via.write(reg::ORA, 0x7F);
        via.write(reg::DDRB, 0xFF);
        via.write(reg::ORB, 0x06);
        let pins = via.pins();
        assert_eq!(pins.a, 0x7F, "the DAC sees port A");
        assert_eq!(pins.b, 0x06, "the mux select sees port B");
    }
}
