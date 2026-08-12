//! Beam trace: sample types, byte layout, ring buffer, file I/O, validation.
//!
//! The single interface between signal producers and the renderer, specified
//! normatively in TRACE-FORMAT.md. Same layout in memory, on disk, and in the
//! GPU storage buffer — little-endian throughout, no repacking anywhere.
//!
//! A trace is a time-ordered, piecewise-linear beam trajectory with per-sample
//! drive. Blanked travel is present as zero-drive samples; a true position
//! discontinuity is a flag bit, not a gap.

mod file;
mod ring;
mod sample;
mod validate;

pub use file::{
    DEFAULT_EPSILON, HEADER_LEN, MAGIC, PRODUCER_ID_LEN, Trace, TraceHeader, VERSION, decode,
    encode, read_file, write_file,
};
pub use ring::{DEFAULT_CAPACITY, RingBuffer};
pub use sample::{Sample, flags};
pub use validate::{TraceError, validate};
