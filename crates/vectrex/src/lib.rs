//! The Vectrex machine — layer 1 (ARCHITECTURE.md §5, §6 step 4).
//!
//! There is no display processor: the 6809 bit-bangs a 6522 VIA, whose port A
//! feeds a DAC and whose control lines gate the integrators and the beam. So
//! the picture is a consequence of *when* the CPU writes, which is why the VIA
//! is modelled cycle by cycle.

pub mod analogue;
pub mod cartridge;
pub mod frontend;
pub mod machine;
pub mod memory;
pub mod psg;
pub mod via;
