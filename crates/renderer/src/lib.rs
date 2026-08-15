//! Low-level GPU context and optional window surface helpers.
//!
//! The default `surface` feature enables winit-backed window presentation.
//! Headless GPU consumers may disable default features and use [`GpuContext`]
//! without compiling the windowing backend.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

/// GPU device and queue initialization.
pub mod context;
/// Window surface configuration.
#[cfg(feature = "surface")]
pub mod surface;

pub use context::{GpuContext, GpuContextDescriptor, GpuContextError};
#[cfg(feature = "surface")]
pub use surface::{
    UnconfiguredWindowSurface, WindowSurface, WindowSurfaceDescriptor, WindowSurfaceError,
};
