//! What the processor talks to, one call per machine cycle.

/// Memory as the CPU sees it.
///
/// **Exactly one call per machine cycle**, dead cycles included. On the 6809 a
/// dead cycle is not idle: the bus is still driven, and the part reads
/// something it will discard — either `0xFFFF` (a VMA cycle) or the
/// instruction stream at PC. The verification corpus records those reads as
/// ordinary ones, so a core that computes an instruction's result and then
/// synthesises a plausible cycle count cannot be verified against it at all
/// (ARCHITECTURE.md §5). That constraint is why this trait exists at machine-
/// cycle granularity rather than per instruction.
///
/// A real machine also needs it: the VIA is clocked by the same E cycle, so
/// when the CPU touches the bus decides what the timers have counted.
pub trait Bus {
    fn read(&mut self, address: u16) -> u8;
    fn write(&mut self, address: u16, value: u8);

    /// A cycle the part spends on internal work with the bus not driven at
    /// all. On the MC6809 only the undocumented `0x38` produces one —
    /// everything else that looks idle is a real read, as above. A machine has
    /// nothing to do for it, so the default is to do nothing; the recording
    /// bus overrides it, because the cycle still has to be counted.
    fn internal(&mut self) {}
}

/// One bus call, as recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cycle {
    pub write: bool,
    pub address: u16,
    pub value: u8,
    /// The bus was not driven this cycle, so address and value mean nothing.
    pub internal: bool,
}

/// 64 KiB of RAM that remembers every cycle, for checking a core against the
/// corpus: the comparison is against the traffic, not just the outcome.
#[derive(Clone)]
pub struct RecordingBus {
    memory: Box<[u8; 0x1_0000]>,
    pub cycles: Vec<Cycle>,
}

impl Default for RecordingBus {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingBus {
    pub fn new() -> Self {
        Self {
            memory: Box::new([0; 0x1_0000]),
            cycles: Vec::new(),
        }
    }

    /// Place a byte without recording a cycle — for seeding a test's initial
    /// memory, which the machine never saw happen.
    pub fn poke(&mut self, address: u16, value: u8) {
        self.memory[address as usize] = value;
    }

    /// Read a byte without recording a cycle, for checking final state.
    pub fn peek(&self, address: u16) -> u8 {
        self.memory[address as usize]
    }

    pub fn clear_cycles(&mut self) {
        self.cycles.clear();
    }
}

impl Bus for RecordingBus {
    fn read(&mut self, address: u16) -> u8 {
        let value = self.memory[address as usize];
        self.cycles.push(Cycle { write: false, address, value, internal: false });
        value
    }

    fn write(&mut self, address: u16, value: u8) {
        self.memory[address as usize] = value;
        self.cycles.push(Cycle { write: true, address, value, internal: false });
    }

    fn internal(&mut self) {
        self.cycles.push(Cycle { write: false, address: 0, value: 0, internal: true });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_call_is_recorded_in_order_with_what_crossed_the_bus() {
        let mut bus = RecordingBus::new();
        bus.poke(0x1234, 0x5A);

        let got = bus.read(0x1234);
        bus.write(0x8000, 0x99);
        let back = bus.read(0x8000);
        // A dead cycle: the part reads 0xFFFF and throws the value away, but
        // the cycle is on the bus and the corpus records it.
        bus.read(0xFFFF);

        assert_eq!((got, back), (0x5A, 0x99));
        assert_eq!(
            bus.cycles,
            vec![
                Cycle { write: false, address: 0x1234, value: 0x5A, internal: false },
                Cycle { write: true, address: 0x8000, value: 0x99, internal: false },
                Cycle { write: false, address: 0x8000, value: 0x99, internal: false },
                Cycle { write: false, address: 0xFFFF, value: 0x00, internal: false },
            ]
        );
    }

    #[test]
    fn seeding_and_inspecting_do_not_appear_on_the_bus() {
        // Initial memory was never written by the machine, and checking final
        // memory is not something the machine did — neither may pollute the
        // cycle log the corpus is compared against.
        let mut bus = RecordingBus::new();
        bus.poke(0x0010, 0x11);
        assert_eq!(bus.peek(0x0010), 0x11);
        assert!(bus.cycles.is_empty());

        bus.read(0x0010);
        assert_eq!(bus.cycles.len(), 1);
        bus.clear_cycles();
        assert!(bus.cycles.is_empty());
        assert_eq!(bus.peek(0x0010), 0x11, "clearing the log must not clear memory");
    }

    #[test]
    fn memory_wraps_the_whole_address_space() {
        let mut bus = RecordingBus::new();
        bus.write(0xFFFF, 0x7E);
        assert_eq!(bus.read(0xFFFF), 0x7E);
        assert_eq!(bus.read(0x0000), 0x00, "the top of memory is not the bottom");
    }
}
