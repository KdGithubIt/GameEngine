//! Project scripting ABI data contracts and host-independent gameplay API types.

#![warn(missing_docs)]

#[doc(hidden)]
pub mod animation_commands;

/// Rust gameplay field, component, resource, and schedule contracts.
pub mod game_contracts;
/// Host-independent typed gameplay system API.
pub mod game_api;
/// Host-independent typed gameplay convenience helpers.
pub mod game_convenience;
/// Per-entity parameters used by `#[game_system(each)]` callbacks.
pub mod game_each;
/// Versioned query input, deferred output, command, and event ABI contracts.
pub mod game_io;
/// Native project-module registration and C ABI export contracts.
pub mod game_module;
