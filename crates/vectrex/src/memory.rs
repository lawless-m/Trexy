//! The Vectrex address space, as the 6809 sees it.
//!
//! ```text
//!   $0000-$7FFF  cartridge ROM (absent reads as open bus)
//!   $8000-$C7FF  unmapped
//!   $C800-$CFFF  1 KiB RAM, mirrored within the window
//!   $D000-$D7FF  6522 VIA, register on the low four bits, mirrored
//!   $E000-$FFFF  BIOS ROM — 8 KiB here, or 4 KiB at $F000
//! ```
//!
//! ROM images are read from wherever the user put them and never copied into
//! this repository: they are copyrighted and supplied by whoever runs this
//! (ARCHITECTURE.md §5).

use std::path::{Path, PathBuf};

use m6809::Bus;

use crate::psg::Psg;
use crate::via::{Pins, Via};

/// Where extracted ROM images are looked for.
pub fn rom_dir() -> PathBuf {
    std::env::var_os("VECTREX_ROMS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".cache/trexy/roms")
        })
}

/// Load a ROM image, with a message that says where to put one if it is
/// missing rather than just failing to open a path.
pub fn load(path: impl AsRef<Path>) -> Result<Vec<u8>, String> {
    let path = path.as_ref();
    std::fs::read(path).map_err(|e| {
        format!(
            "{}: {e}\nVectrex ROMs are not distributed with this project. \
             Extract your own into {} , or set VECTREX_ROMS.",
            path.display(),
            rom_dir().display()
        )
    })
}

pub const RAM_SIZE: usize = 1024;

/// What an undriven bus reads as. Nothing pulls it down, so it floats high.
const IDLE_BUS: u8 = 0xFF;

/// Everything the CPU can address.
pub struct Memory {
    pub cartridge: Vec<u8>,
    /// The top of memory. 8 KiB maps at $E000, 4 KiB at $F000.
    pub rom: Vec<u8>,
    pub ram: [u8; RAM_SIZE],
    pub via: Via,
    pub psg: Psg,
    /// Machine cycles since the machine was built. One per bus call, which is
    /// what the [`Bus`] contract guarantees.
    pub cycles: u64,
    /// What the VIA's pins did, cycle by cycle, since the log was last
    /// cleared. The picture is made of these — the analogue front end reads
    /// them — so they have to be sampled every cycle rather than at
    /// instruction boundaries, where a whole stroke could pass unseen.
    pub pin_log: Vec<(u64, Pins)>,
}

impl Memory {
    pub fn new(rom: Vec<u8>, cartridge: Vec<u8>) -> Self {
        Self {
            cartridge,
            rom,
            ram: [0; RAM_SIZE],
            via: Via::new(),
            psg: Psg::new(),
            cycles: 0,
            pin_log: Vec::new(),
        }
    }

    /// Where the top ROM starts, from its size.
    fn rom_base(&self) -> u32 {
        0x1_0000 - self.rom.len() as u32
    }

    /// The reset vector, read without spending machine cycles: the caller
    /// wants to know where the part will start, not to pretend it has.
    pub fn reset_vector(&self) -> u16 {
        u16::from(self.load(0xFFFE)) << 8 | u16::from(self.load(0xFFFF))
    }

    /// One machine cycle of everything that is not the CPU.
    fn tick(&mut self) {
        // The sound chip hangs off port A and drives it back while the machine
        // is reading it — which is how the buttons are scanned. Without this
        // the port floats to zero and every button reads as held down.
        let pins = self.via.pins();
        let driven = self.psg.cycle(pins.b, pins.a).unwrap_or(IDLE_BUS);
        self.via.set_pins_a(driven);

        self.via.step();
        self.pin_log.push((self.cycles, self.via.pins()));
        self.cycles += 1;
    }

    /// Read without advancing the clock.
    fn load(&self, address: u16) -> u8 {
        let at = u32::from(address);
        match address {
            0x0000..=0x7FFF => self.cartridge.get(at as usize).copied().unwrap_or(0xFF),
            0xC800..=0xCFFF => self.ram[at as usize % RAM_SIZE],
            // A VIA register read is not free of side effects, so this cannot
            // serve it; the bus impl below does.
            0xD000..=0xD7FF => 0xFF,
            _ if at >= self.rom_base() => {
                let index = (at - self.rom_base()) as usize;
                self.rom.get(index).copied().unwrap_or(0xFF)
            }
            // Nothing is driving the bus here.
            _ => 0xFF,
        }
    }
}

impl Bus for Memory {
    fn read(&mut self, address: u16) -> u8 {
        let value = match address {
            0xD000..=0xD7FF => self.via.read(address as u8 & 0x0F),
            _ => self.load(address),
        };
        self.tick();
        value
    }

    fn write(&mut self, address: u16, value: u8) {
        match address {
            0xC800..=0xCFFF => self.ram[u32::from(address) as usize % RAM_SIZE] = value,
            0xD000..=0xD7FF => self.via.write(address as u8 & 0x0F, value),
            // ROM ignores writes; so does open bus.
            _ => {}
        }
        self.tick();
    }

    fn internal(&mut self) {
        self.tick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bios() -> Vec<u8> {
        load(rom_dir().join("Mine Storm (1982).vec")).expect("the 8K BIOS image")
    }

    #[test]
    fn the_ram_window_mirrors_within_itself() {
        let mut mem = Memory::new(vec![0; 0x2000], Vec::new());
        mem.write(0xC800, 0x5A);
        assert_eq!(mem.read(0xC800), 0x5A);
        // 1 KiB repeated across a 2 KiB window.
        assert_eq!(mem.read(0xCC00), 0x5A, "the same cell, seen through the mirror");
        mem.write(0xCC01, 0x7E);
        assert_eq!(mem.read(0xC801), 0x7E);
    }

    #[test]
    fn the_via_decodes_on_four_bits_and_mirrors_across_its_window() {
        let mut mem = Memory::new(vec![0; 0x2000], Vec::new());
        mem.write(0xD003, 0xFF); // DDRA
        mem.write(0xD001, 0x42); // ORA
        assert_eq!(mem.via.ora, 0x42);
        // Same registers, further up the window.
        mem.write(0xD011, 0x24);
        assert_eq!(mem.via.ora, 0x24);
        mem.write(0xD7F1, 0x11);
        assert_eq!(mem.via.ora, 0x11);
    }

    #[test]
    fn an_eight_kilobyte_image_maps_at_e000() {
        let rom = bios();
        assert_eq!(rom.len(), 8192);
        let mut mem = Memory::new(rom.clone(), Vec::new());
        assert_eq!(mem.read(0xE000), rom[0]);
        assert_eq!(mem.read(0xFFFF), rom[8191]);
    }

    #[test]
    fn a_four_kilobyte_image_maps_at_f000() {
        let rom = load(rom_dir().join("Vectrex BIOS (1982).vec")).expect("the 4K BIOS image");
        assert_eq!(rom.len(), 4096);
        let mut mem = Memory::new(rom.clone(), Vec::new());
        assert_eq!(mem.read(0xF000), rom[0]);
        assert_eq!(mem.read(0xFFFF), rom[4095]);
    }

    #[test]
    fn the_reset_vector_points_into_rom() {
        let mem = Memory::new(bios(), Vec::new());
        let vector = mem.reset_vector();
        assert!(vector >= 0xE000, "reset vector {vector:04x} is outside ROM");
    }

    #[test]
    fn every_bus_call_is_one_machine_cycle() {
        let mut mem = Memory::new(bios(), Vec::new());
        mem.read(0xE000);
        mem.write(0xC800, 0);
        // An internal cycle touches nothing, but the VIA still counted it.
        mem.internal();
        assert_eq!(mem.cycles, 3);
        assert_eq!(mem.pin_log.len(), 3, "the pins are sampled every cycle");
        assert_eq!(mem.pin_log[2].0, 2, "each sample carries its own cycle");
    }

    #[test]
    fn reading_the_reset_vector_costs_nothing() {
        let mem = Memory::new(bios(), Vec::new());
        mem.reset_vector();
        assert_eq!(mem.cycles, 0, "asking where the part starts is not the part running");
    }

    #[test]
    fn writes_to_rom_are_ignored() {
        let rom = bios();
        let was = rom[0];
        let mut mem = Memory::new(rom, Vec::new());
        mem.write(0xE000, !was);
        assert_eq!(mem.read(0xE000), was);
    }

    #[test]
    fn an_absent_cartridge_reads_as_open_bus() {
        let mut mem = Memory::new(bios(), Vec::new());
        assert_eq!(mem.read(0x0000), 0xFF);
        assert_eq!(mem.read(0x9000), 0xFF, "and so does the unmapped gap");
    }
}
