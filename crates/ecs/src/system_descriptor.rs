//! Stable runtime system identity and scheduling metadata.

use std::fmt;

/// Stable identifier used by project settings and schedule constraints.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SystemId(String);

impl SystemId {
    /// Validates and creates a system identifier.
    ///
    /// IDs are code-definition identifiers rather than runtime instance IDs.
    /// They contain at least two dot-separated lowercase ASCII segments. Each
    /// segment starts with a letter and continues with letters, digits, or
    /// underscores, keeping persisted IDs portable and unambiguous.
    pub fn try_new(value: impl Into<String>) -> Result<Self, SystemIdError> {
        let value = value.into();
        let segments: Vec<_> = value.split('.').collect();
        let is_valid = segments.len() >= 2
            && segments.iter().all(|segment| {
                let mut bytes = segment.bytes();
                bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
                    && bytes.all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            });
        if !is_valid {
            return Err(SystemIdError { value });
        }
        Ok(Self(value))
    }

    /// Returns the persisted string form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SystemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reports an invalid stable system identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemIdError {
    value: String,
}

impl SystemIdError {
    /// Returns the rejected identifier text.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for SystemIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "system ID `{}` must contain at least two dot-separated lowercase ASCII segments",
            self.value
        )
    }
}

impl std::error::Error for SystemIdError {}

/// Identifies which layer registered a runtime system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemOrigin {
    /// A built-in or host-provided engine system.
    Engine,
    /// A project-local native Rust game system.
    Game,
    /// A registration made without an explicit stable identifier.
    Unnamed,
}

/// Metadata attached to one registered runtime system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemDescriptor {
    id: SystemId,
    display_name: String,
    description: String,
    origin: SystemOrigin,
    before: Vec<SystemId>,
    after: Vec<SystemId>,
    aliases: Vec<SystemId>,
    is_persistent: bool,
}

impl SystemDescriptor {
    /// Creates an explicitly identified descriptor suitable for persistence.
    pub fn new(
        id: impl Into<String>,
        display_name: impl Into<String>,
        origin: SystemOrigin,
    ) -> Result<Self, SystemIdError> {
        Ok(Self {
            id: SystemId::try_new(id)?,
            display_name: display_name.into(),
            description: String::new(),
            origin,
            before: Vec::new(),
            after: Vec::new(),
            aliases: Vec::new(),
            is_persistent: true,
        })
    }

    pub(crate) fn unnamed(system_name: &str, registration_index: usize) -> Self {
        // FNV-1a gives unnamed registrations a deterministic, compact ID while
        // making the type-name dependency visible through `is_persistent`.
        // Official engine and game registrations never use this fallback.
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in system_name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        Self {
            id: SystemId(format!("unnamed.{hash:016x}.{registration_index}")),
            display_name: system_name.to_owned(),
            description: "Registered without a stable ID; use an explicit descriptor before persisting this system's order.".to_owned(),
            origin: SystemOrigin::Unnamed,
            before: Vec::new(),
            after: Vec::new(),
            aliases: Vec::new(),
            is_persistent: false,
        }
    }

    /// Sets the optional human-readable description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Adds a constraint requiring this system to run before `id`.
    pub fn try_before(mut self, id: impl Into<String>) -> Result<Self, SystemIdError> {
        self.before.push(SystemId::try_new(id)?);
        Ok(self)
    }

    /// Adds a constraint requiring this system to run after `id`.
    pub fn try_after(mut self, id: impl Into<String>) -> Result<Self, SystemIdError> {
        self.after.push(SystemId::try_new(id)?);
        Ok(self)
    }

    /// Adds a previous stable ID that should migrate to this descriptor's ID.
    pub fn try_alias(mut self, id: impl Into<String>) -> Result<Self, SystemIdError> {
        self.aliases.push(SystemId::try_new(id)?);
        Ok(self)
    }

    /// Returns the stable identifier.
    pub fn id(&self) -> &SystemId {
        &self.id
    }

    /// Returns the editor-facing label.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Returns the optional editor-facing description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the registration origin.
    pub fn origin(&self) -> SystemOrigin {
        self.origin
    }

    /// Returns IDs that must execute after this system.
    pub fn before(&self) -> &[SystemId] {
        &self.before
    }

    /// Returns IDs that must execute before this system.
    pub fn after(&self) -> &[SystemId] {
        &self.after
    }

    /// Returns previous IDs accepted during settings migration.
    pub fn aliases(&self) -> &[SystemId] {
        &self.aliases
    }

    /// Returns whether this ID is safe to write into project settings.
    pub fn is_persistent(&self) -> bool {
        self.is_persistent
    }
}

/// Public snapshot of one entry in its current execution order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleEntryInfo {
    /// System metadata used by runtime and editor clients.
    pub descriptor: SystemDescriptor,
    /// Zero-based current execution position.
    pub order: usize,
    /// Whether schedule execution currently runs this entry.
    pub is_enabled: bool,
}

/// Preferred persisted order and disabled IDs for one schedule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScheduleConfiguration {
    /// Preferred stable ID order.
    pub order: Vec<SystemId>,
    /// Stable IDs that remain registered but are skipped during execution.
    pub disabled: Vec<SystemId>,
}

/// Non-fatal issue found while merging metadata and project settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleDiagnostic {
    /// A saved ID no longer exists in the current runtime catalog.
    UnknownConfiguredSystem(SystemId),
    /// A constraint references a system absent from this schedule.
    MissingConstraintTarget {
        /// System declaring the constraint.
        system: SystemId,
        /// Missing target ID.
        target: SystemId,
    },
    /// A previous ID was migrated to its current canonical ID.
    MigratedAlias {
        /// Saved previous ID.
        from: SystemId,
        /// Current canonical ID.
        to: SystemId,
    },
    /// Saved order was corrected to satisfy declared constraints.
    ConstraintAdjusted,
}

/// Reports an invalid schedule registration or ordering operation.
#[derive(Debug)]
pub enum ScheduleEditError {
    /// A stable ID is already registered or conflicts with an alias.
    DuplicateId(SystemId),
    /// No entry has the requested ID.
    UnknownSystem(SystemId),
    /// The requested target position is outside the schedule.
    InvalidPosition {
        /// Requested zero-based position.
        position: usize,
        /// Current number of systems.
        len: usize,
    },
    /// The requested exact order conflicts with before/after constraints.
    ConstraintViolation,
    /// Declared constraints contain a cycle.
    ConstraintCycle {
        /// IDs that could not be ordered after cycle detection.
        systems: Vec<SystemId>,
    },
}

impl fmt::Display for ScheduleEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(formatter, "duplicate system ID `{id}`"),
            Self::UnknownSystem(id) => write!(formatter, "system ID `{id}` is not registered"),
            Self::InvalidPosition { position, len } => {
                write!(formatter, "system position {position} is outside 0..{len}")
            }
            Self::ConstraintViolation => {
                formatter.write_str("requested order violates a before/after constraint")
            }
            Self::ConstraintCycle { systems } => write!(
                formatter,
                "system constraints contain a cycle involving {}",
                systems
                    .iter()
                    .map(SystemId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl std::error::Error for ScheduleEditError {}

/// Reports failure while building or identifying a named system entry.
#[derive(Debug)]
pub enum SystemRegistrationError {
    /// System parameter access validation failed.
    Build(crate::SystemBuildError),
    /// Stable ID or alias registration failed.
    Schedule(ScheduleEditError),
}

impl fmt::Display for SystemRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(error) => error.fmt(formatter),
            Self::Schedule(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SystemRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::Schedule(error) => Some(error),
        }
    }
}

impl From<crate::SystemBuildError> for SystemRegistrationError {
    fn from(value: crate::SystemBuildError) -> Self {
        Self::Build(value)
    }
}

impl From<ScheduleEditError> for SystemRegistrationError {
    fn from(value: ScheduleEditError) -> Self {
        Self::Schedule(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_id_accepts_dotted_lowercase_segments() {
        for value in [
            "engine.transform_propagation",
            "game.combat2",
            "game.query.nearby_enemies",
        ] {
            assert_eq!(
                SystemId::try_new(value)
                    .expect("documented system ID must be valid")
                    .as_str(),
                value
            );
        }
    }

    #[test]
    fn system_id_rejects_ambiguous_or_non_portable_forms() {
        for value in [
            "",
            "engine",
            ".engine",
            "engine.",
            "engine..camera",
            "Engine.camera",
            "engine.camera-aspect",
            "engine.2camera",
            "engine camera",
            "!!!",
        ] {
            assert!(
                SystemId::try_new(value).is_err(),
                "`{value}` must not become a persisted system ID"
            );
        }
    }
}
