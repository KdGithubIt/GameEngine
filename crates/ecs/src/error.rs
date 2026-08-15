use crate::access::AccessError;
use crate::component::ComponentId;
use crate::entity::Entity;
use std::fmt;

/// Reports a failed runtime world operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldError {
    /// No live entity exists with the requested numeric ID.
    EntityNotFound(Entity),
    /// The numeric ID exists, but the entity generation is no longer current.
    StaleEntity(Entity),
    /// The allocator attempted to create an entity whose numeric ID is alive.
    EntityIdAlreadyInUse(Entity),
    /// The entity already contains the requested component type.
    ComponentAlreadyExists {
        /// The entity that was being modified.
        entity: Entity,
        /// The component that was already present.
        component: ComponentId,
    },
    /// The entity does not contain the requested component type.
    ComponentNotFound {
        /// The entity that was being modified.
        entity: Entity,
        /// The component that was missing.
        component: ComponentId,
    },
    /// No additional runtime entity IDs can be allocated.
    EntityIdExhausted,
    /// The entity allocator mutex was poisoned by a previous panic.
    EntityAllocatorUnavailable,
    /// A deferred command could not be sent because its world no longer exists.
    CommandQueueDisconnected,
    /// The deferred command queue mutex was poisoned by a previous panic.
    CommandQueueUnavailable,
    /// An operation failed and its required cleanup also failed.
    OperationAndCleanupFailed {
        /// The original operation failure.
        operation: Box<WorldError>,
        /// The failure encountered while cleaning up partial runtime state.
        cleanup: Box<WorldError>,
    },
    /// An internal ECS invariant was violated.
    InternalInvariant(&'static str),
}

impl WorldError {
    pub(crate) fn with_cleanup(operation: Self, cleanup: Result<(), Self>) -> Self {
        match cleanup {
            Ok(()) => operation,
            Err(cleanup) => Self::OperationAndCleanupFailed {
                operation: Box::new(operation),
                cleanup: Box::new(cleanup),
            },
        }
    }
}

impl fmt::Display for WorldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EntityNotFound(entity) => write!(formatter, "entity {entity} was not found"),
            Self::StaleEntity(entity) => write!(formatter, "entity {entity} is stale"),
            Self::EntityIdAlreadyInUse(entity) => {
                write!(formatter, "entity ID for {entity} is already in use")
            }
            Self::ComponentAlreadyExists { entity, component } => {
                write!(
                    formatter,
                    "entity {entity} already has component {component}"
                )
            }
            Self::ComponentNotFound { entity, component } => {
                write!(
                    formatter,
                    "entity {entity} does not have component {component}"
                )
            }
            Self::EntityIdExhausted => formatter.write_str("runtime entity IDs are exhausted"),
            Self::EntityAllocatorUnavailable => {
                formatter.write_str("entity allocator is unavailable after a panic")
            }
            Self::CommandQueueDisconnected => {
                formatter.write_str("runtime command queue is disconnected")
            }
            Self::CommandQueueUnavailable => {
                formatter.write_str("runtime command queue is unavailable after a panic")
            }
            Self::OperationAndCleanupFailed { operation, cleanup } => {
                write!(formatter, "{operation}; cleanup also failed: {cleanup}")
            }
            Self::InternalInvariant(message) => {
                write!(formatter, "internal ECS invariant violated: {message}")
            }
        }
    }
}

impl std::error::Error for WorldError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OperationAndCleanupFailed { operation, .. } => Some(operation),
            _ => None,
        }
    }
}

/// Reports a failed direct query construction.
#[derive(Debug)]
pub enum QueryError {
    /// The query requests conflicting component access.
    Access(AccessError),
    /// The world storage is internally inconsistent.
    World(WorldError),
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Access(error) => error.fmt(formatter),
            Self::World(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for QueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Access(error) => Some(error),
            Self::World(error) => Some(error),
        }
    }
}

impl From<AccessError> for QueryError {
    fn from(value: AccessError) -> Self {
        Self::Access(value)
    }
}

impl From<WorldError> for QueryError {
    fn from(value: WorldError) -> Self {
        Self::World(value)
    }
}

/// Reports an invalid system parameter access declaration.
#[derive(Debug)]
pub struct SystemBuildError {
    system_name: &'static str,
    source: AccessError,
}

impl SystemBuildError {
    pub(crate) fn new(system_name: &'static str, source: AccessError) -> Self {
        Self {
            system_name,
            source,
        }
    }

    /// Returns the Rust type name of the system that could not be built.
    pub fn system_name(&self) -> &'static str {
        self.system_name
    }

    /// Returns the access conflict that made the system invalid.
    pub fn access_error(&self) -> &AccessError {
        &self.source
    }
}

impl fmt::Display for SystemBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to build system {}: {}",
            self.system_name, self.source
        )
    }
}

impl std::error::Error for SystemBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Reports a failure while fetching a system parameter.
#[derive(Debug)]
pub enum SystemParamError {
    /// A required resource was not present in the world.
    MissingResource {
        /// The Rust type name of the missing resource.
        type_name: &'static str,
    },
    /// A query could not be prepared from the world.
    Query(QueryError),
}

impl fmt::Display for SystemParamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingResource { type_name } => {
                write!(formatter, "required resource {type_name} is missing")
            }
            Self::Query(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SystemParamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MissingResource { .. } => None,
            Self::Query(error) => Some(error),
        }
    }
}

impl From<QueryError> for SystemParamError {
    fn from(value: QueryError) -> Self {
        Self::Query(value)
    }
}

/// Underlying reason a validated system could not complete.
#[derive(Debug)]
pub enum SystemExecutionError {
    /// One of the declared parameters could not be fetched.
    Parameter(SystemParamError),
    /// The system function returned an application error.
    Callback(Box<dyn std::error::Error + Send + Sync>),
}

impl fmt::Display for SystemExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parameter(error) => error.fmt(formatter),
            Self::Callback(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SystemExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parameter(error) => Some(error),
            Self::Callback(error) => Some(error.as_ref()),
        }
    }
}

impl From<SystemParamError> for SystemExecutionError {
    fn from(value: SystemParamError) -> Self {
        Self::Parameter(value)
    }
}

/// Reports a failure while running one system.
#[derive(Debug)]
pub struct SystemRunError {
    system_name: &'static str,
    source: SystemExecutionError,
}

impl SystemRunError {
    pub(crate) fn new(system_name: &'static str, source: SystemExecutionError) -> Self {
        Self {
            system_name,
            source,
        }
    }

    /// Returns the Rust type name of the failed system.
    pub fn system_name(&self) -> &'static str {
        self.system_name
    }
}

impl fmt::Display for SystemRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "system {} could not run: {}",
            self.system_name, self.source
        )
    }
}

impl std::error::Error for SystemRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Reports a failure while running a schedule.
#[derive(Debug)]
pub enum ScheduleError {
    /// A system could not fetch its parameters.
    System(SystemRunError),
    /// A system failed and queued commands could not be fully discarded.
    SystemAndCommandDiscard {
        /// The original system failure.
        system: SystemRunError,
        /// Failures encountered while discarding queued commands.
        discard_errors: Vec<WorldError>,
    },
    /// One or more deferred runtime commands failed.
    Commands(Vec<WorldError>),
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System(error) => error.fmt(formatter),
            Self::SystemAndCommandDiscard {
                system,
                discard_errors,
            } => write!(
                formatter,
                "{system}; {} queued command(s) also failed to discard",
                discard_errors.len()
            ),
            Self::Commands(errors) => {
                write!(
                    formatter,
                    "{} deferred runtime command(s) failed",
                    errors.len()
                )
            }
        }
    }
}

impl std::error::Error for ScheduleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::System(error) => Some(error),
            Self::SystemAndCommandDiscard { system, .. } => Some(system),
            Self::Commands(_) => None,
        }
    }
}
