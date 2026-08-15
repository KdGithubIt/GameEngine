//! Runtime archetype-based Entity Component System.
//!
//! This crate owns ephemeral runtime entities and optimized component storage.
//! It intentionally does not contain authoring data, serialized project IDs,
//! editor behavior, or MCP integration.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

mod access;
mod app;
mod archetype;
mod commands;
mod component;
mod entity;
mod error;
mod query;
mod registry;
mod resource;
mod schedule;
mod storage;
mod system;
mod system_descriptor;
mod world;
mod world_cell;

pub use access::{AccessError, AccessKind, AccessTargetKind, SystemAccess};
pub use app::App;
pub use commands::{Commands, EntityCommands, RuntimeCommand};
pub use component::{Component, ComponentId};
pub use entity::Entity;
pub use error::{
    QueryError, ScheduleError, SystemBuildError, SystemExecutionError, SystemParamError,
    SystemRunError, WorldError,
};
pub use query::{Query, QueryData, QueryFilter, QueryIter, ReadOnlyQueryData, With, Without};
pub use registry::{TypeRegistration, TypeRegistry};
pub use resource::{Res, ResMut};
pub use schedule::Schedule;
pub use system::{FunctionSystem, IntoSystem, System, SystemParam};
pub use system_descriptor::{
    ScheduleConfiguration, ScheduleDiagnostic, ScheduleEditError, ScheduleEntryInfo,
    SystemDescriptor, SystemId, SystemIdError, SystemOrigin, SystemRegistrationError,
};
pub use world::World;
