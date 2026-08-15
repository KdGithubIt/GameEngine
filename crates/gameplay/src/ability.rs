//! Data-driven ability timing helpers for project-local action-game code.
//!
//! The types in this module deliberately do not own ECS entities, animation
//! clips, hitboxes, audio, or input. Project systems keep those engine commands
//! explicit while sharing one deterministic startup/active/recovery/cooldown
//! state machine across attacks, dodges, skills, and interact actions.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Current persisted schema version for [`AbilityLibrary`].
pub const ABILITY_LIBRARY_SCHEMA_VERSION: u32 = 1;

/// Ordered phases of one ability activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbilityPhase {
    /// The ability can be activated.
    #[default]
    Ready,
    /// Wind-up before gameplay effects become active.
    Startup,
    /// Gameplay effects such as hitboxes or invulnerability are active.
    Active,
    /// The activation has ended but the actor has not fully recovered.
    Recovery,
    /// The actor recovered, but this ability is still unavailable.
    Cooldown,
}

/// Author-controlled timing for one reusable ability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbilityDefinition {
    /// Stable project-local identifier used by gameplay code and diagnostics.
    pub id: String,
    /// Seconds spent in [`AbilityPhase::Startup`].
    pub startup_seconds: f32,
    /// Seconds spent in [`AbilityPhase::Active`].
    pub active_seconds: f32,
    /// Seconds spent in [`AbilityPhase::Recovery`].
    pub recovery_seconds: f32,
    /// Seconds spent in [`AbilityPhase::Cooldown`] after recovery.
    pub cooldown_seconds: f32,
}

impl Default for AbilityDefinition {
    fn default() -> Self {
        Self {
            id: "new_ability".to_owned(),
            startup_seconds: 0.1,
            active_seconds: 0.2,
            recovery_seconds: 0.3,
            cooldown_seconds: 0.0,
        }
    }
}

impl AbilityDefinition {
    /// Creates and validates an ability timing definition.
    ///
    /// # Errors
    ///
    /// Returns [`AbilityDefinitionError`] when the identifier is blank or any
    /// duration is negative or non-finite.
    pub fn new(
        id: impl Into<String>,
        startup_seconds: f32,
        active_seconds: f32,
        recovery_seconds: f32,
        cooldown_seconds: f32,
    ) -> Result<Self, AbilityDefinitionError> {
        let definition = Self {
            id: id.into(),
            startup_seconds,
            active_seconds,
            recovery_seconds,
            cooldown_seconds,
        };
        definition.validate()?;
        Ok(definition)
    }

    /// Validates this definition without changing it.
    ///
    /// # Errors
    ///
    /// Returns [`AbilityDefinitionError`] when the identifier is blank or any
    /// duration is negative or non-finite.
    pub fn validate(&self) -> Result<(), AbilityDefinitionError> {
        if self.id.trim().is_empty() {
            return Err(AbilityDefinitionError::BlankId);
        }
        for (field, value) in [
            ("startup_seconds", self.startup_seconds),
            ("active_seconds", self.active_seconds),
            ("recovery_seconds", self.recovery_seconds),
            ("cooldown_seconds", self.cooldown_seconds),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(AbilityDefinitionError::InvalidDuration { field, value });
            }
        }
        Ok(())
    }

    /// Returns the total authored duration in seconds.
    pub fn total_seconds(&self) -> f32 {
        self.startup_seconds + self.active_seconds + self.recovery_seconds + self.cooldown_seconds
    }

    /// Returns the phase active at `seconds` from activation start.
    ///
    /// Values below zero clamp to startup. Values at or after the total duration
    /// return [`AbilityPhase::Ready`].
    pub fn phase_at(&self, seconds: f32) -> AbilityPhase {
        let mut remaining = seconds.max(0.0);
        for phase in [
            AbilityPhase::Startup,
            AbilityPhase::Active,
            AbilityPhase::Recovery,
            AbilityPhase::Cooldown,
        ] {
            let duration = self.duration(phase);
            if remaining < duration {
                return phase;
            }
            remaining -= duration;
        }
        AbilityPhase::Ready
    }

    fn duration(&self, phase: AbilityPhase) -> f32 {
        match phase {
            AbilityPhase::Ready => 0.0,
            AbilityPhase::Startup => self.startup_seconds,
            AbilityPhase::Active => self.active_seconds,
            AbilityPhase::Recovery => self.recovery_seconds,
            AbilityPhase::Cooldown => self.cooldown_seconds,
        }
    }
}

/// Validation failure for an [`AbilityDefinition`].
#[derive(Debug, Clone, PartialEq)]
pub enum AbilityDefinitionError {
    /// The stable ability identifier was empty or whitespace-only.
    BlankId,
    /// A timing field was negative or non-finite.
    InvalidDuration {
        /// Name of the invalid field.
        field: &'static str,
        /// Rejected value.
        value: f32,
    },
}

impl fmt::Display for AbilityDefinitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlankId => write!(formatter, "ability ID must not be blank"),
            Self::InvalidDuration { field, value } => write!(
                formatter,
                "ability {field} must be finite and non-negative, found {value}"
            ),
        }
    }
}

impl std::error::Error for AbilityDefinitionError {}

/// Persisted collection edited by the Ability Designer GUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbilityLibrary {
    /// File format version.
    pub schema_version: u32,
    /// Reusable project ability definitions.
    #[serde(default)]
    pub abilities: Vec<AbilityDefinition>,
}

impl Default for AbilityLibrary {
    fn default() -> Self {
        Self {
            schema_version: ABILITY_LIBRARY_SCHEMA_VERSION,
            abilities: vec![AbilityDefinition::default()],
        }
    }
}

impl AbilityLibrary {
    /// Parses and validates a library from JSON.
    ///
    /// # Errors
    ///
    /// Returns [`AbilityLibraryError`] for malformed JSON, an unsupported schema,
    /// duplicate IDs, or an invalid ability definition.
    pub fn from_json_str(json: &str) -> Result<Self, AbilityLibraryError> {
        let library: Self = serde_json::from_str(json).map_err(AbilityLibraryError::Json)?;
        library.validate()?;
        Ok(library)
    }

    /// Serializes this library as pretty JSON after validation.
    ///
    /// # Errors
    ///
    /// Returns [`AbilityLibraryError`] when validation or serialization fails.
    pub fn to_json_string(&self) -> Result<String, AbilityLibraryError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(AbilityLibraryError::Json)
    }

    /// Validates schema, IDs, and every timing definition.
    ///
    /// # Errors
    ///
    /// Returns the first deterministic validation error.
    pub fn validate(&self) -> Result<(), AbilityLibraryError> {
        if self.schema_version != ABILITY_LIBRARY_SCHEMA_VERSION {
            return Err(AbilityLibraryError::UnsupportedSchema {
                found: self.schema_version,
            });
        }
        let mut ids = std::collections::BTreeSet::new();
        for (index, ability) in self.abilities.iter().enumerate() {
            ability
                .validate()
                .map_err(|source| AbilityLibraryError::InvalidAbility { index, source })?;
            if !ids.insert(ability.id.as_str()) {
                return Err(AbilityLibraryError::DuplicateId(ability.id.clone()));
            }
        }
        Ok(())
    }

    /// Finds an ability by its stable ID.
    pub fn find(&self, id: &str) -> Option<&AbilityDefinition> {
        self.abilities.iter().find(|ability| ability.id == id)
    }
}

/// Loading, serialization, or validation failure for [`AbilityLibrary`].
#[derive(Debug)]
pub enum AbilityLibraryError {
    /// JSON parsing or serialization failed.
    Json(serde_json::Error),
    /// The file declares a schema newer or older than this engine understands.
    UnsupportedSchema {
        /// Rejected schema version.
        found: u32,
    },
    /// One ability failed validation.
    InvalidAbility {
        /// Zero-based ability index.
        index: usize,
        /// Definition error.
        source: AbilityDefinitionError,
    },
    /// Two abilities use the same stable ID.
    DuplicateId(String),
}

impl fmt::Display for AbilityLibraryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "ability library JSON error: {error}"),
            Self::UnsupportedSchema { found } => write!(
                formatter,
                "unsupported ability library schema {found}; expected {ABILITY_LIBRARY_SCHEMA_VERSION}"
            ),
            Self::InvalidAbility { index, source } => {
                write!(formatter, "ability {index} is invalid: {source}")
            }
            Self::DuplicateId(id) => write!(formatter, "ability ID `{id}` is duplicated"),
        }
    }
}

impl std::error::Error for AbilityLibraryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::InvalidAbility { source, .. } => Some(source),
            Self::UnsupportedSchema { .. } | Self::DuplicateId(_) => None,
        }
    }
}

/// Failure to start an ability activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbilityActivationError {
    /// The requested definition was invalid.
    InvalidDefinition(String),
    /// Another activation or cooldown is still in progress.
    Busy {
        /// Ability currently owning the machine.
        active_id: String,
        /// Current phase of that ability.
        phase: AbilityPhase,
    },
}

impl fmt::Display for AbilityActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDefinition(reason) => write!(formatter, "invalid ability: {reason}"),
            Self::Busy { active_id, phase } => {
                write!(
                    formatter,
                    "ability `{active_id}` is still in phase {phase:?}"
                )
            }
        }
    }
}

impl std::error::Error for AbilityActivationError {}

/// One deterministic state transition produced by [`AbilityMachine`].
#[derive(Debug, Clone, PartialEq)]
pub struct AbilityEvent {
    /// Monotonic activation number assigned when the ability starts.
    pub activation: u64,
    /// Stable ID copied from the activated definition.
    pub ability_id: String,
    /// Phase left by this transition.
    pub from: AbilityPhase,
    /// Phase entered by this transition.
    pub to: AbilityPhase,
}

/// Runtime-only state machine shared by data-driven abilities.
#[derive(Debug, Clone, Default)]
pub struct AbilityMachine {
    phase: AbilityPhase,
    phase_elapsed: f32,
    definition: Option<AbilityDefinition>,
    activation: u64,
}

impl AbilityMachine {
    /// Creates an idle ability machine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the current phase.
    pub fn phase(&self) -> AbilityPhase {
        self.phase
    }

    /// Returns the current ability identifier while an activation or cooldown exists.
    pub fn ability_id(&self) -> Option<&str> {
        self.definition
            .as_ref()
            .map(|definition| definition.id.as_str())
    }

    /// Returns seconds elapsed in the current phase.
    pub fn phase_elapsed(&self) -> f32 {
        self.phase_elapsed
    }

    /// Returns the latest activation sequence number.
    pub fn activation(&self) -> u64 {
        self.activation
    }

    /// Returns whether a new ability may be activated immediately.
    pub fn is_ready(&self) -> bool {
        self.phase == AbilityPhase::Ready
    }

    /// Starts an ability and returns every transition caused by zero-duration phases.
    ///
    /// # Errors
    ///
    /// Returns [`AbilityActivationError::Busy`] until the current ability reaches
    /// [`AbilityPhase::Ready`], or `InvalidDefinition` for malformed timing data.
    pub fn activate(
        &mut self,
        definition: AbilityDefinition,
    ) -> Result<Vec<AbilityEvent>, AbilityActivationError> {
        definition
            .validate()
            .map_err(|error| AbilityActivationError::InvalidDefinition(error.to_string()))?;
        if let Some(active) = self.definition.as_ref() {
            return Err(AbilityActivationError::Busy {
                active_id: active.id.clone(),
                phase: self.phase,
            });
        }
        self.activation = self.activation.wrapping_add(1);
        self.definition = Some(definition);
        self.phase = AbilityPhase::Startup;
        self.phase_elapsed = 0.0;
        let mut events = vec![self.event(AbilityPhase::Ready, AbilityPhase::Startup)];
        self.advance_zero_duration_phases(&mut events);
        Ok(events)
    }

    /// Advances the active ability by `delta_seconds`.
    ///
    /// One call may cross several short phases. Events are returned in exact
    /// phase order, making the result independent of render frame rate when the
    /// caller supplies a fixed simulation delta.
    ///
    /// Invalid, negative, or zero deltas do not change the machine.
    pub fn tick(&mut self, delta_seconds: f32) -> Vec<AbilityEvent> {
        if self.definition.is_none() || !delta_seconds.is_finite() || delta_seconds <= 0.0 {
            return Vec::new();
        }

        let mut remaining = delta_seconds;
        let mut events = Vec::new();
        while self.definition.is_some() && remaining > 0.0 {
            let duration = self
                .definition
                .as_ref()
                .map(|definition| definition.duration(self.phase))
                .unwrap_or(0.0);
            let until_boundary = (duration - self.phase_elapsed).max(0.0);
            if remaining < until_boundary {
                self.phase_elapsed += remaining;
                break;
            }
            remaining -= until_boundary;
            self.phase_elapsed = duration;
            self.advance_phase(&mut events);
            self.advance_zero_duration_phases(&mut events);
        }
        events
    }

    /// Cancels the current activation and becomes ready immediately.
    pub fn cancel(&mut self) -> Option<AbilityEvent> {
        let definition = self.definition.take()?;
        let from = self.phase;
        self.phase = AbilityPhase::Ready;
        self.phase_elapsed = 0.0;
        Some(AbilityEvent {
            activation: self.activation,
            ability_id: definition.id,
            from,
            to: AbilityPhase::Ready,
        })
    }

    fn advance_zero_duration_phases(&mut self, events: &mut Vec<AbilityEvent>) {
        while let Some(definition) = self.definition.as_ref() {
            if definition.duration(self.phase) > 0.0 {
                break;
            }
            self.advance_phase(events);
        }
    }

    fn advance_phase(&mut self, events: &mut Vec<AbilityEvent>) {
        let from = self.phase;
        let to = match self.phase {
            AbilityPhase::Ready => return,
            AbilityPhase::Startup => AbilityPhase::Active,
            AbilityPhase::Active => AbilityPhase::Recovery,
            AbilityPhase::Recovery => AbilityPhase::Cooldown,
            AbilityPhase::Cooldown => AbilityPhase::Ready,
        };
        events.push(self.event(from, to));
        self.phase = to;
        self.phase_elapsed = 0.0;
        if to == AbilityPhase::Ready {
            self.definition = None;
        }
    }

    fn event(&self, from: AbilityPhase, to: AbilityPhase) -> AbilityEvent {
        AbilityEvent {
            activation: self.activation,
            ability_id: self
                .definition
                .as_ref()
                .map(|definition| definition.id.clone())
                .unwrap_or_default(),
            from,
            to,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attack() -> AbilityDefinition {
        AbilityDefinition::new("attack.light", 0.1, 0.2, 0.3, 0.4)
            .expect("test definition must be valid")
    }

    #[test]
    fn activation_rejects_overlapping_ability() {
        let mut machine = AbilityMachine::new();
        machine
            .activate(attack())
            .expect("first activation must start");

        let error = machine
            .activate(attack())
            .expect_err("second activation must remain blocked");

        assert!(matches!(
            error,
            AbilityActivationError::Busy {
                phase: AbilityPhase::Startup,
                ..
            }
        ));
    }

    #[test]
    fn fixed_delta_crosses_phases_in_order() {
        let mut machine = AbilityMachine::new();
        machine.activate(attack()).expect("activation must start");

        let events = machine.tick(0.65);

        assert_eq!(
            events.iter().map(|event| event.to).collect::<Vec<_>>(),
            vec![
                AbilityPhase::Active,
                AbilityPhase::Recovery,
                AbilityPhase::Cooldown,
            ]
        );
        assert_eq!(machine.phase(), AbilityPhase::Cooldown);
        assert!((machine.phase_elapsed() - 0.05).abs() < f32::EPSILON * 8.0);
    }

    #[test]
    fn zero_duration_definition_completes_without_sticking() {
        let mut machine = AbilityMachine::new();
        let definition = AbilityDefinition::new("instant", 0.0, 0.0, 0.0, 0.0)
            .expect("zero durations are valid");

        let events = machine.activate(definition).expect("activation must start");

        assert_eq!(events.len(), 5);
        assert!(machine.is_ready());
        assert_eq!(machine.ability_id(), None);
    }

    #[test]
    fn cancel_returns_to_ready_and_keeps_activation_sequence() {
        let mut machine = AbilityMachine::new();
        machine.activate(attack()).expect("activation must start");
        let activation = machine.activation();

        let event = machine.cancel().expect("active ability must cancel");

        assert_eq!(event.activation, activation);
        assert_eq!(event.to, AbilityPhase::Ready);
        assert!(machine.is_ready());
    }

    #[test]
    fn invalid_duration_is_rejected() {
        let error = AbilityDefinition::new("attack.invalid", -0.1, 0.0, 0.0, 0.0)
            .expect_err("negative duration must be invalid");

        assert!(matches!(
            error,
            AbilityDefinitionError::InvalidDuration {
                field: "startup_seconds",
                ..
            }
        ));
    }

    #[test]
    fn library_round_trip_preserves_definitions() {
        let library = AbilityLibrary {
            schema_version: ABILITY_LIBRARY_SCHEMA_VERSION,
            abilities: vec![attack()],
        };

        let json = library.to_json_string().expect("library must serialize");
        let loaded = AbilityLibrary::from_json_str(&json).expect("library must load");

        assert_eq!(loaded, library);
    }

    #[test]
    fn library_rejects_duplicate_ids() {
        let library = AbilityLibrary {
            schema_version: ABILITY_LIBRARY_SCHEMA_VERSION,
            abilities: vec![attack(), attack()],
        };

        assert!(matches!(
            library.validate(),
            Err(AbilityLibraryError::DuplicateId(_))
        ));
    }
}
