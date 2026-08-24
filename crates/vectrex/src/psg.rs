//! AY-3-8912 registers, enough to keep the BIOS happy.
//!
//! No synthesis: this exists because the BIOS scans the buttons through the
//! sound chip's port A, and a chip that never answers can stall the input scan
//! before the attract mode gets going. Sound is not part of the renderer's
//! problem (ARCHITECTURE.md §1).

/// Bus control, decoded from the VIA's port B on the Vectrex: BDIR is PB4 and
/// BC1 is PB3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Neither line asserted: the chip ignores the bus.
    Inactive,
    /// BC1 alone: the chip is presenting the selected register.
    Read,
    /// BDIR alone: the data lines carry a value for the selected register.
    Write,
    /// Both: the data lines carry a register number.
    Latch,
}

impl Mode {
    /// Decode BDIR and BC1 as the Vectrex wires them.
    pub fn of(port_b: u8) -> Self {
        match (port_b & 0x10 != 0, port_b & 0x08 != 0) {
            (false, false) => Mode::Inactive,
            (false, true) => Mode::Read,
            (true, false) => Mode::Write,
            (true, true) => Mode::Latch,
        }
    }
}

/// Register 14 is the chip's I/O port A, which the Vectrex wires to the
/// controller buttons — active low, so nothing pressed reads as all ones.
pub const IO_PORT_A: usize = 14;

#[derive(Clone, Debug)]
pub struct Psg {
    registers: [u8; 16],
    selected: usize,
    /// Button lines, active low. All ones means nothing pressed.
    pub buttons: u8,
}

impl Default for Psg {
    fn default() -> Self {
        Self::new()
    }
}

impl Psg {
    pub fn new() -> Self {
        Self { registers: [0; 16], selected: 0, buttons: 0xFF }
    }

    /// Present one cycle of bus state. `data` is the value on port A, and the
    /// return is what the chip drives back when it is being read.
    pub fn cycle(&mut self, port_b: u8, data: u8) -> Option<u8> {
        match Mode::of(port_b) {
            Mode::Inactive => None,
            Mode::Latch => {
                self.selected = usize::from(data & 0x0F);
                None
            }
            Mode::Write => {
                // The port register is an input here; writing it changes
                // nothing the machine can read back.
                if self.selected != IO_PORT_A {
                    self.registers[self.selected] = data;
                }
                None
            }
            Mode::Read => Some(self.read()),
        }
    }

    pub fn read(&self) -> u8 {
        if self.selected == IO_PORT_A {
            self.buttons
        } else {
            self.registers[self.selected]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LATCH: u8 = 0x18;
    const WRITE: u8 = 0x10;
    const READ: u8 = 0x08;

    #[test]
    fn a_register_written_reads_back() {
        let mut psg = Psg::new();
        psg.cycle(LATCH, 7);
        psg.cycle(WRITE, 0x3F);
        psg.cycle(LATCH, 7);
        assert_eq!(psg.cycle(READ, 0), Some(0x3F));
    }

    #[test]
    fn the_button_port_always_answers_even_though_nothing_is_pressed() {
        // The point of the stub: an unanswered scan can stall the BIOS before
        // the attract mode starts.
        let mut psg = Psg::new();
        psg.cycle(LATCH, IO_PORT_A as u8);
        assert_eq!(psg.cycle(READ, 0), Some(0xFF));

        psg.cycle(WRITE, 0x00);
        assert_eq!(psg.cycle(READ, 0), Some(0xFF), "the port is an input");
    }

    #[test]
    fn a_pressed_button_pulls_its_line_low() {
        let mut psg = Psg::new();
        psg.buttons = 0xFE;
        psg.cycle(LATCH, IO_PORT_A as u8);
        assert_eq!(psg.cycle(READ, 0), Some(0xFE));
    }

    #[test]
    fn an_idle_bus_changes_nothing() {
        let mut psg = Psg::new();
        psg.cycle(LATCH, 3);
        psg.cycle(WRITE, 0x55);
        assert_eq!(psg.cycle(0x00, 0xFF), None, "inactive means inactive");
        psg.cycle(LATCH, 3);
        assert_eq!(psg.cycle(READ, 0), Some(0x55));
    }

    #[test]
    fn only_four_bits_select_a_register() {
        let mut psg = Psg::new();
        psg.cycle(LATCH, 0xF3); // high nibble is not part of the address
        psg.cycle(WRITE, 0x11);
        psg.cycle(LATCH, 0x03);
        assert_eq!(psg.cycle(READ, 0), Some(0x11));
    }
}
