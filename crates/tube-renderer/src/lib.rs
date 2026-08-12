//! Layer 3 — the tube model: wgpu pipelines, WGSL shaders, tube profiles,
//! parameter registry.
//!
//! Specified in RENDERER.md.

pub mod deposit;
pub mod frame;
pub mod phosphor;
mod readback;
pub mod readout;
pub mod substep;
pub mod timing;

pub use deposit::{Deposit, DepositMode, DepositParams, DepositShaders, SUPERSAMPLE, TubeProfile};
pub use frame::{Field, FieldShaders, TubeParams};
pub use phosphor::{Component, Phosphor, PhosphorParams};
pub use readout::{Readout, ReadoutParams, ReadoutShaders, View};
pub use substep::{SUBSTEP_SECONDS, Substep, SubstepClock, clip_spans};
pub use timing::{READOUT_PASSES, Timings};
