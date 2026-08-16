//! Platform-facing runtime adapters for GameEngine.
//!
//! Default features enable desktop window-input, audio, and gamepad backends.
//! Consumers that only need virtual-input contracts may disable default
//! features and avoid compiling those operating-system integrations.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

/// Runtime audio assets, authored playback, and native output backend.
#[cfg(feature = "audio")]
pub mod audio;
/// Backend-neutral spatial-audio math and runtime voice contracts.
pub mod spatial_audio;
/// Desktop gamepad event ingestion.
#[cfg(feature = "gamepad")]
pub mod gamepad;
/// Keyboard, pointer, controller, and virtual-input ingestion.
pub mod input;
/// Stable parsers for persisted physical-input names.
#[doc(hidden)]
pub mod input_names;
