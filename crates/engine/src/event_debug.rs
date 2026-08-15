//! Bounded fixed-step timeline for animation and combat event inspection.

use std::collections::VecDeque;

use engine_ecs::{Entity, Res, ResMut};
use serde::{Deserialize, Serialize};

use crate::animation::AnimationEvents;
use crate::combat::HitResults;
use crate::time::FixedTime;

/// Environment variable used by the Event Timeline Viewer to request live JSON output.
pub const RUNTIME_EVENT_TRACE_PATH_ENV: &str = "GAMEENGINE_EVENT_TRACE_PATH";

/// Current persisted trace schema version.
pub const RUNTIME_EVENT_TRACE_SCHEMA_VERSION: u32 = 1;

/// One event category retained by the runtime debug timeline.
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeEventDebugKind {
    /// A named marker fired by an animator.
    Animation {
        /// Entity whose animator fired the marker.
        entity: Entity,
        /// Authored event name.
        name: String,
        /// Target clip time after the fixed step.
        clip_time: f32,
    },
    /// One accepted combat hit.
    Hit {
        /// Entity credited as the attacker.
        attacker: Entity,
        /// Runtime hitbox entity that produced the contact.
        hitbox: Entity,
        /// Entity whose health changed.
        target: Entity,
        /// Damage applied by the contact.
        damage: f32,
        /// Target health after the hit.
        remaining_health: f32,
        /// Hitbox activation generation.
        activation: u64,
    },
}

/// Immutable entry retained by [`RuntimeEventTimeline`].
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeEventDebugEntry {
    /// Monotonic sequence local to this Play session.
    pub sequence: u64,
    /// Fixed simulation step in which the event was observed.
    pub fixed_step: u64,
    /// Event-specific payload.
    pub kind: RuntimeEventDebugKind,
}

/// Serializable entity identity used by persisted debugger traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeEventTraceEntity {
    /// Numeric runtime entity ID.
    pub id: u32,
    /// Entity generation used to reject stale handles.
    pub generation: u32,
}

impl From<Entity> for RuntimeEventTraceEntity {
    fn from(entity: Entity) -> Self {
        Self {
            id: entity.id(),
            generation: entity.generation(),
        }
    }
}

/// Serializable event payload displayed by the Event Timeline Viewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEventTraceKind {
    /// A named animation marker.
    Animation {
        /// Firing entity.
        entity: RuntimeEventTraceEntity,
        /// Authored event name.
        name: String,
        /// Clip time after the firing step.
        clip_time: f32,
    },
    /// One accepted combat hit.
    Hit {
        /// Attacker entity.
        attacker: RuntimeEventTraceEntity,
        /// Hitbox entity.
        hitbox: RuntimeEventTraceEntity,
        /// Damaged entity.
        target: RuntimeEventTraceEntity,
        /// Applied damage.
        damage: f32,
        /// Health after damage.
        remaining_health: f32,
        /// Hitbox activation generation.
        activation: u64,
    },
}

/// Serializable timeline entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeEventTraceEntry {
    /// Monotonic event sequence.
    pub sequence: u64,
    /// Fixed simulation step.
    pub fixed_step: u64,
    /// Event payload.
    pub kind: RuntimeEventTraceKind,
}

/// Persisted snapshot consumed by the live Event Timeline Viewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeEventTrace {
    /// File format version.
    pub schema_version: u32,
    /// Latest fixed step observed by the producer.
    pub latest_fixed_step: u64,
    /// Retained events ordered from oldest to newest.
    pub entries: Vec<RuntimeEventTraceEntry>,
}

impl RuntimeEventTrace {
    /// Parses a trace from JSON.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeEventTraceError`] for malformed JSON or an unsupported schema.
    pub fn from_json_str(json: &str) -> Result<Self, RuntimeEventTraceError> {
        let trace: Self = serde_json::from_str(json).map_err(RuntimeEventTraceError::Json)?;
        if trace.schema_version != RUNTIME_EVENT_TRACE_SCHEMA_VERSION {
            return Err(RuntimeEventTraceError::UnsupportedSchema {
                found: trace.schema_version,
            });
        }
        Ok(trace)
    }

    /// Serializes this trace as pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeEventTraceError`] when serialization fails.
    pub fn to_json_string(&self) -> Result<String, RuntimeEventTraceError> {
        serde_json::to_string_pretty(self).map_err(RuntimeEventTraceError::Json)
    }
}

/// Trace parsing or serialization failure.
#[derive(Debug)]
pub enum RuntimeEventTraceError {
    /// JSON parsing or serialization failed.
    Json(serde_json::Error),
    /// The trace schema is unsupported.
    UnsupportedSchema {
        /// Rejected schema version.
        found: u32,
    },
}

impl std::fmt::Display for RuntimeEventTraceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "runtime event trace JSON error: {error}"),
            Self::UnsupportedSchema { found } => write!(
                formatter,
                "unsupported runtime event trace schema {found}; expected {RUNTIME_EVENT_TRACE_SCHEMA_VERSION}"
            ),
        }
    }
}

impl std::error::Error for RuntimeEventTraceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::UnsupportedSchema { .. } => None,
        }
    }
}

/// Bounded runtime resource used by editor and player diagnostics.
#[derive(Debug, Clone)]
pub struct RuntimeEventTimeline {
    entries: VecDeque<RuntimeEventDebugEntry>,
    capacity: usize,
    next_sequence: u64,
    last_animation_generation: Option<u64>,
    last_hit_generation: Option<u64>,
    latest_fixed_step: u64,
    revision: u64,
    last_persisted_revision: u64,
}

impl RuntimeEventTimeline {
    /// Default maximum retained event count.
    pub const DEFAULT_CAPACITY: usize = 256;

    /// Creates a timeline retaining at most `capacity` entries.
    ///
    /// A zero capacity is promoted to one so a configured timeline always
    /// remains observable.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            next_sequence: 0,
            last_animation_generation: None,
            last_hit_generation: None,
            latest_fixed_step: 0,
            revision: 0,
            last_persisted_revision: 0,
        }
    }

    /// Iterates retained entries from oldest to newest.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &RuntimeEventDebugEntry> {
        self.entries.iter()
    }

    /// Returns the number of retained entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether no entries are retained.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Removes every retained entry and producer cursor.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.last_animation_generation = None;
        self.last_hit_generation = None;
        self.revision = self.revision.wrapping_add(1);
    }

    /// Creates a serializable snapshot of the retained timeline.
    pub fn trace(&self) -> RuntimeEventTrace {
        let entries = self
            .entries
            .iter()
            .map(|entry| RuntimeEventTraceEntry {
                sequence: entry.sequence,
                fixed_step: entry.fixed_step,
                kind: match &entry.kind {
                    RuntimeEventDebugKind::Animation {
                        entity,
                        name,
                        clip_time,
                    } => RuntimeEventTraceKind::Animation {
                        entity: (*entity).into(),
                        name: name.clone(),
                        clip_time: *clip_time,
                    },
                    RuntimeEventDebugKind::Hit {
                        attacker,
                        hitbox,
                        target,
                        damage,
                        remaining_health,
                        activation,
                    } => RuntimeEventTraceKind::Hit {
                        attacker: (*attacker).into(),
                        hitbox: (*hitbox).into(),
                        target: (*target).into(),
                        damage: *damage,
                        remaining_health: *remaining_health,
                        activation: *activation,
                    },
                },
            })
            .collect();
        RuntimeEventTrace {
            schema_version: RUNTIME_EVENT_TRACE_SCHEMA_VERSION,
            latest_fixed_step: self.latest_fixed_step,
            entries,
        }
    }

    /// Serializes the retained timeline as pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeEventTraceError`] when serialization fails.
    pub fn to_json_string(&self) -> Result<String, RuntimeEventTraceError> {
        self.trace().to_json_string()
    }

    fn push(&mut self, fixed_step: u64, kind: RuntimeEventDebugKind) {
        let entry = RuntimeEventDebugEntry {
            sequence: self.next_sequence,
            fixed_step,
            kind,
        };
        self.next_sequence = self.next_sequence.wrapping_add(1);
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
        self.revision = self.revision.wrapping_add(1);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn persist_live_trace_if_requested(&mut self) {
        if self.revision == self.last_persisted_revision {
            return;
        }
        let Some(path) = std::env::var_os(RUNTIME_EVENT_TRACE_PATH_ENV) else {
            return;
        };
        let path = std::path::PathBuf::from(path);
        let Ok(json) = self.to_json_string() else {
            return;
        };
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err() {
                return;
            }
        if std::fs::write(path, json).is_ok() {
            self.last_persisted_revision = self.revision;
        }
    }
}

impl Default for RuntimeEventTimeline {
    fn default() -> Self {
        Self::with_capacity(Self::DEFAULT_CAPACITY)
    }
}

/// Copies newly produced animation markers and combat hits into one timeline.
///
/// Register this after both animation sampling and combat-contact processing in
/// the fixed schedule. Producer generations prevent stale resources from being
/// appended again when the debug system is disabled and later re-enabled.
///
/// On desktop, setting [`RUNTIME_EVENT_TRACE_PATH_ENV`] writes updated snapshots
/// for the Event Timeline Viewer without changing gameplay event delivery.
pub fn runtime_event_timeline_system(
    time: Res<FixedTime>,
    animation_events: Option<Res<AnimationEvents>>,
    hit_results: Option<Res<HitResults>>,
    mut timeline: ResMut<RuntimeEventTimeline>,
) {
    let fixed_step = time.step_count;
    timeline.latest_fixed_step = fixed_step;

    if let Some(events) = animation_events {
        let generation = events.generation();
        if timeline.last_animation_generation != Some(generation) {
            for event in events.iter() {
                timeline.push(
                    fixed_step,
                    RuntimeEventDebugKind::Animation {
                        entity: event.entity,
                        name: event.name.clone(),
                        clip_time: event.clip_time,
                    },
                );
            }
            timeline.last_animation_generation = Some(generation);
        }
    }

    if let Some(results) = hit_results {
        let generation = results.generation();
        if timeline.last_hit_generation != Some(generation) {
            for hit in results.iter() {
                timeline.push(
                    fixed_step,
                    RuntimeEventDebugKind::Hit {
                        attacker: hit.attacker,
                        hitbox: hit.hitbox,
                        target: hit.target,
                        damage: hit.damage,
                        remaining_health: hit.remaining_health,
                        activation: hit.activation,
                    },
                );
            }
            timeline.last_hit_generation = Some(generation);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    timeline.persist_live_trace_if_requested();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_drops_oldest_entries_at_capacity() {
        let mut timeline = RuntimeEventTimeline::with_capacity(2);
        for fixed_step in 1..=3 {
            timeline.push(
                fixed_step,
                RuntimeEventDebugKind::Animation {
                    entity: Entity::from_raw(1, 0),
                    name: format!("event_{fixed_step}"),
                    clip_time: fixed_step as f32,
                },
            );
        }

        let retained = timeline
            .iter()
            .map(|entry| entry.fixed_step)
            .collect::<Vec<_>>();
        assert_eq!(retained, vec![2, 3]);
    }

    #[test]
    fn clear_removes_entries_and_producer_cursors() {
        let mut timeline = RuntimeEventTimeline {
            last_animation_generation: Some(4),
            last_hit_generation: Some(8),
            ..Default::default()
        };
        timeline.push(
            1,
            RuntimeEventDebugKind::Animation {
                entity: Entity::from_raw(1, 0),
                name: "attack_active".to_owned(),
                clip_time: 0.2,
            },
        );

        timeline.clear();

        assert!(timeline.is_empty());
        assert_eq!(timeline.last_animation_generation, None);
        assert_eq!(timeline.last_hit_generation, None);
    }

    #[test]
    fn trace_round_trip_preserves_entity_generations() {
        let mut timeline = RuntimeEventTimeline::default();
        timeline.push(
            7,
            RuntimeEventDebugKind::Animation {
                entity: Entity::from_raw(4, 2),
                name: "attack_active".to_owned(),
                clip_time: 0.2,
            },
        );
        let json = timeline.to_json_string().expect("trace must serialize");
        let trace = RuntimeEventTrace::from_json_str(&json).expect("trace must load");

        assert!(matches!(
            &trace.entries[0].kind,
            RuntimeEventTraceKind::Animation {
                entity: RuntimeEventTraceEntity {
                    id: 4,
                    generation: 2
                },
                ..
            }
        ));
    }
}
