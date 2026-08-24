//! The machine: a 6809, a 6522, a sound chip and 1 KiB of RAM, on one clock.
//!
//! Nothing here decides what the screen looks like. The CPU writes to the VIA,
//! the VIA's pins move, and the analogue front end downstream reads those pins
//! cycle by cycle — so all this layer has to get right is *when* each of those
//! things happens relative to the others (ARCHITECTURE.md §5).

use m6809::Cpu;

use crate::frontend::CLOCK_HZ;
use crate::memory::Memory;

pub struct Machine {
    pub cpu: Cpu,
    pub memory: Memory,
}

impl Machine {
    /// `rom` maps at the top of memory — 8 KiB at `$E000`, 4 KiB at `$F000`.
    pub fn new(rom: Vec<u8>, cartridge: Vec<u8>) -> Self {
        let memory = Memory::new(rom, cartridge);
        let mut cpu = Cpu::new();
        cpu.regs.pc = memory.reset_vector();
        Self { cpu, memory }
    }

    /// Run one instruction, clocking the VIA through every cycle it takes.
    ///
    /// The pin log is cleared first, so after this call it holds exactly what
    /// the analogue side saw during this instruction and nothing else.
    pub fn step(&mut self) {
        // The 6809 samples IRQ between instructions, so this is the moment it
        // can act on: a device holding the line low is served here or not at
        // all until the next one finishes.
        self.cpu.irq = self.memory.via.irq();
        self.memory.pin_log.clear();
        self.cpu.step(&mut self.memory);
    }

    pub fn cycles(&self) -> u64 {
        self.memory.cycles
    }

    /// Elapsed time, which is just the cycle count at the E clock's rate.
    pub fn seconds(&self) -> f64 {
        self.memory.cycles as f64 / CLOCK_HZ
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{load, rom_dir};

    fn bios() -> Vec<u8> {
        load(rom_dir().join("Mine Storm (1982).vec")).expect("the 8K BIOS image")
    }

    #[test]
    fn the_clock_converts_to_seconds() {
        let mut machine = Machine::new(bios(), Vec::new());
        while machine.cycles() < 1_500_000 {
            machine.step();
        }
        // The step that crossed the second overshot it by a few cycles.
        assert!((machine.seconds() - 1.0).abs() < 1e-4, "{}", machine.seconds());
    }

    #[test]
    fn the_bios_runs_from_its_reset_vector_and_drives_the_via() {
        let mut machine = Machine::new(bios(), Vec::new());
        assert!(machine.cpu.regs.pc >= 0xE000, "reset vector is outside ROM");

        // The pin log holds one instruction, so what the run did has to be
        // gathered as it goes.
        let mut drove_the_dac = false;
        while machine.cycles() < 100_000 {
            machine.step();
            drove_the_dac |= machine.memory.pin_log.iter().any(|(_, p)| p.a != 0);
        }

        // Something has to have moved, or the CPU is running but the machine
        // is not: the BIOS sets the DAC up long before it draws anything.
        assert_ne!(machine.memory.via.ddra, 0, "the BIOS never configured port A");
        assert!(drove_the_dac, "nothing was ever put on the DAC");
    }

    #[test]
    fn the_pin_log_holds_one_instruction_at_a_time() {
        let mut machine = Machine::new(bios(), Vec::new());
        machine.step();
        let first = machine.memory.pin_log.len();
        assert!(first > 0);
        machine.step();
        assert_eq!(
            machine.memory.pin_log.len() + first,
            machine.cycles() as usize,
            "the log should hold the last instruction, not both"
        );
    }
}
