//! Low-dependency runtime primitives shared by GameEngine domains.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

/// Platform-independent input state and controller primitives.
pub mod input;
/// Engine-owned physical keyboard and mouse-button identities.
pub mod physical_input;
/// Authored gameplay metadata retained in the runtime world.
pub mod runtime_metadata;
/// Frame and fixed-step timing resources.
pub mod time;
