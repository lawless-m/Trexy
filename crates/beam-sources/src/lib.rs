//! Layer 1 — signal sources: the synthetic generator and the XY audio player.
//!
//! Sources emit beam traces directly in the first slice; the deflection model
//! that will eventually sit between them and the renderer is layer 2, and does
//! not exist yet (ARCHITECTURE.md §2). The sampling contract applies to them
//! all the same, so the renderer is never tuned against traces no real
//! producer would emit.

pub mod lissajous;
pub mod patterns;
pub mod pen;

pub use lissajous::Lissajous;
pub use patterns::{EPSILON, PATTERNS, Pattern};
pub use pen::Pen;
