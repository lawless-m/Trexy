//! An MC6809 core — layer 1's processor (ARCHITECTURE.md §6 step 4).
//!
//! Verified against single-step vectors that record every machine cycle, so
//! the core is built cycle-by-cycle rather than instruction-by-instruction.
//! Nothing here knows what a Vectrex is; the machine around it is a separate
//! crate.

pub mod bus;
pub mod cpu;
pub mod moo;

pub use bus::{Bus, Cycle, RecordingBus};
pub use cpu::Cpu;

/// The condition-code bits, in their hardware positions.
///
/// The verification corpus stores CC as a byte, so the packing is part of the
/// contract rather than an implementation detail.
pub mod cc {
    /// Entire state was stacked (as opposed to a fast interrupt's subset).
    pub const E: u8 = 0x80;
    /// FIRQ mask.
    pub const F: u8 = 0x40;
    /// Half carry, out of bit 3.
    pub const H: u8 = 0x20;
    /// IRQ mask.
    pub const I: u8 = 0x10;
    pub const N: u8 = 0x08;
    pub const Z: u8 = 0x04;
    pub const V: u8 = 0x02;
    pub const C: u8 = 0x01;
}

/// The programmer-visible state.
///
/// A and B are held separately and paired on demand rather than stored as D:
/// most instructions touch one accumulator, and a stored D would need
/// splitting far more often than the pair needs joining.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    pub a: u8,
    pub b: u8,
    pub x: u16,
    pub y: u16,
    /// Hardware stack, used by interrupts and subroutine calls.
    pub s: u16,
    /// User stack.
    pub u: u16,
    pub pc: u16,
    /// Direct page: the high byte of every direct-mode address.
    pub dp: u8,
    pub cc: u8,
}

impl Registers {
    /// A:B as one 16-bit accumulator, A high.
    pub fn d(&self) -> u16 {
        u16::from(self.a) << 8 | u16::from(self.b)
    }

    pub fn set_d(&mut self, value: u16) {
        self.a = (value >> 8) as u8;
        self.b = value as u8;
    }

    pub fn flag(&self, bit: u8) -> bool {
        self.cc & bit != 0
    }

    pub fn set_flag(&mut self, bit: u8, on: bool) {
        if on {
            self.cc |= bit;
        } else {
            self.cc &= !bit;
        }
    }

    /// Set N and Z from an 8-bit result, leaving the other bits alone. The
    /// commonest pair of side effects in the instruction set.
    pub fn set_nz8(&mut self, value: u8) {
        self.set_flag(cc::N, value & 0x80 != 0);
        self.set_flag(cc::Z, value == 0);
    }

    /// Set N and Z from a 16-bit result.
    pub fn set_nz16(&mut self, value: u16) {
        self.set_flag(cc::N, value & 0x8000 != 0);
        self.set_flag(cc::Z, value == 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d_is_a_and_b_joined() {
        let mut r = Registers { a: 0x12, b: 0x34, ..Default::default() };
        assert_eq!(r.d(), 0x1234);

        r.set_d(0xBEEF);
        assert_eq!((r.a, r.b), (0xBE, 0xEF));
        assert_eq!(r.d(), 0xBEEF);
    }

    #[test]
    fn every_condition_bit_round_trips() {
        let mut r = Registers::default();
        for bit in [cc::E, cc::F, cc::H, cc::I, cc::N, cc::Z, cc::V, cc::C] {
            r.set_flag(bit, true);
            assert!(r.flag(bit), "setting {bit:#04x} did not take");
            r.set_flag(bit, false);
            assert!(!r.flag(bit), "clearing {bit:#04x} did not take");
        }
    }

    #[test]
    fn condition_bits_sit_where_the_hardware_puts_them() {
        // The corpus stores CC as a byte, so these positions are load-bearing.
        let mut r = Registers { cc: 0xFF, ..Default::default() };
        for bit in [cc::E, cc::F, cc::H, cc::I, cc::N, cc::Z, cc::V, cc::C] {
            assert!(r.flag(bit));
        }
        assert_eq!(
            cc::E | cc::F | cc::H | cc::I | cc::N | cc::Z | cc::V | cc::C,
            0xFF,
            "the eight bits must tile the byte with no gaps or overlaps"
        );

        r.cc = 0;
        r.set_flag(cc::H, true);
        assert_eq!(r.cc, 0x20);
    }

    #[test]
    fn n_and_z_follow_a_result() {
        let mut r = Registers::default();
        r.set_nz8(0);
        assert!(r.flag(cc::Z) && !r.flag(cc::N));
        r.set_nz8(0x80);
        assert!(!r.flag(cc::Z) && r.flag(cc::N));
        r.set_nz16(0);
        assert!(r.flag(cc::Z) && !r.flag(cc::N));
        r.set_nz16(0x8000);
        assert!(!r.flag(cc::Z) && r.flag(cc::N));
    }

    #[test]
    fn setting_one_flag_leaves_the_others_alone() {
        let mut r = Registers { cc: cc::C | cc::I, ..Default::default() };
        r.set_flag(cc::Z, true);
        assert_eq!(r.cc, cc::C | cc::I | cc::Z);
        r.set_flag(cc::C, false);
        assert_eq!(r.cc, cc::I | cc::Z);
    }
}
