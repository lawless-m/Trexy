//! The native shell, as a library so the regression suite can drive the same
//! rendering path the binary does rather than a parallel one.

pub mod app;
pub mod gpu;
pub mod headless;
pub mod regression;
pub mod render;
pub mod shaders;
pub mod source;
