//! Layer 3 — the tube model: wgpu pipelines, WGSL shaders, tube profiles,
//! parameter registry.
//!
//! Specified in RENDERER.md.

pub mod deposit;

pub use deposit::{Deposit, DepositMode, DepositParams, DepositShaders, SUPERSAMPLE, TubeProfile};
