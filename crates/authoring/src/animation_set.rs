//! Author-owned animation-set assets.
//!
//! An animation set binds stable motion slots from one animation graph to
//! imported animation-clip sub-assets. The graph therefore describes reusable
//! behavior while the set chooses content, including clips imported from
//! different glTF, GLB, or FBX source files.

use crate::id::{AssetId, MotionSlotId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Current persisted animation-set schema version.
pub const ANIMATION_SET_SCHEMA_VERSION: u32 = 1;

/// File-name suffix used by persisted animation-set documents.
pub const ANIMATION_SET_FILE_SUFFIX: &str = ".animset.json";

/// A reviewable mapping from graph motion slots to imported animation clips.
///
/// The [`Self::graph`] reference identifies the graph whose slot contract this
/// set implements. Each binding key is a stable [`MotionSlotId`], while the
/// human-readable binding name can change without breaking graph references.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimationSet {
    /// Persisted format version. Version 1 is currently the only accepted
    /// value.
    pub schema_version: u32,
    /// Animation graph whose motion-slot contract this set implements.
    ///
    /// `None` is a valid authoring state for a newly-created empty set. The
    /// current serialized shape still includes this field explicitly as null.
    #[serde(deserialize_with = "explicit_optional_graph")]
    pub graph: Option<AssetId>,
    /// Clip bindings keyed by stable graph motion-slot ID.
    pub bindings: BTreeMap<MotionSlotId, AnimationBinding>,
}

/// Reads the `graph` field, which the current writer always emits.
///
/// serde treats a plain `Option` field as absent-is-`None`, which would accept
/// a document that never wrote the field at all. Routing the field through
/// `deserialize_with` keeps that implicit default from applying, so an
/// unwritten `graph` is reported as a missing field while an explicit `null`
/// still loads as `None`.
fn explicit_optional_graph<'de, D>(deserializer: D) -> Result<Option<AssetId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<AssetId>::deserialize(deserializer)
}

impl AnimationSet {
    /// Creates an empty animation set for `graph`.
    ///
    /// Empty sets are valid while being edited. Scene validation rejects a
    /// controller whose graph requires a slot that the selected set does not
    /// bind.
    pub fn new(graph: AssetId) -> Self {
        Self {
            schema_version: ANIMATION_SET_SCHEMA_VERSION,
            graph: Some(graph),
            bindings: BTreeMap::new(),
        }
    }

    /// Creates an empty set with no target graph.
    pub fn empty() -> Self {
        Self {
            schema_version: ANIMATION_SET_SCHEMA_VERSION,
            graph: None,
            bindings: BTreeMap::new(),
        }
    }

    /// Parses and validates an animation-set JSON document.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationSetError::Json`] for malformed JSON or missing
    /// current-format fields and a semantic error for unsupported versions,
    /// blank names, duplicate names, or invalid event times.
    pub fn from_json(json: &str) -> Result<Self, AnimationSetError> {
        let set: Self = serde_json::from_str(json).map_err(AnimationSetError::Json)?;
        set.validate()?;
        Ok(set)
    }

    /// Serializes this set as deterministic, pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns a semantic validation error before serialization, or a JSON
    /// error if serialization unexpectedly fails.
    pub fn to_canonical_json(&self) -> Result<String, AnimationSetError> {
        self.validate()?;
        let mut json = serde_json::to_string_pretty(self).map_err(AnimationSetError::Json)?;
        json.push('\n');
        Ok(json)
    }

    /// Validates the persisted animation-set contract.
    ///
    /// # Errors
    ///
    /// Returns the first semantic violation. Clip category and graph-slot
    /// reachability require the project manifest and are validated by the
    /// engine integration layer.
    pub fn validate(&self) -> Result<(), AnimationSetError> {
        if self.schema_version != ANIMATION_SET_SCHEMA_VERSION {
            return Err(AnimationSetError::UnsupportedVersion {
                found: self.schema_version,
            });
        }

        let mut names = BTreeSet::new();
        for (slot, binding) in &self.bindings {
            let trimmed = binding.name.trim();
            if trimmed.is_empty() {
                return Err(AnimationSetError::BlankBindingName { slot: slot.clone() });
            }
            if !names.insert(trimmed.to_owned()) {
                return Err(AnimationSetError::DuplicateBindingName {
                    name: trimmed.to_owned(),
                });
            }
            let mut layers = BTreeSet::from([binding.clip.clone()]);
            for overlay in &binding.overlays {
                if !layers.insert(overlay.clone()) {
                    return Err(AnimationSetError::DuplicateLayer {
                        slot: slot.clone(),
                        clip: overlay.clone(),
                    });
                }
            }
            for event in &binding.events {
                if !event.time.is_finite() || event.time < 0.0 {
                    return Err(AnimationSetError::InvalidEventTime {
                        slot: slot.clone(),
                        event: event.name.clone(),
                    });
                }
                if event.name.trim().is_empty() {
                    return Err(AnimationSetError::BlankEventName { slot: slot.clone() });
                }
            }
        }
        Ok(())
    }
}

/// One graph motion-slot binding in an [`AnimationSet`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimationBinding {
    /// Human-readable slot label used by editors and diagnostics.
    pub name: String,
    /// Imported animation-clip sub-asset selected for this slot.
    pub clip: AssetId,
    /// Ordered supplemental clips composed with [`Self::clip`]. Later
    /// entries override earlier entries when they drive the same channel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<AssetId>,
    /// Author-owned timeline events layered over the imported clip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AnimationSetEvent>,
}

/// One author-owned event marker attached to an animation-set binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimationSetEvent {
    /// Clip-local event time in seconds.
    pub time: f32,
    /// Gameplay event name emitted when playback crosses [`Self::time`].
    pub name: String,
}

/// Reports malformed or semantically invalid animation-set content.
#[derive(Debug)]
pub enum AnimationSetError {
    /// The document is not valid JSON or contains an invalid typed ID.
    Json(serde_json::Error),
    /// The document uses a schema version unsupported by this build.
    UnsupportedVersion {
        /// Version found in the document.
        found: u32,
    },
    /// A binding has no human-readable name.
    BlankBindingName {
        /// Slot whose binding name is blank.
        slot: MotionSlotId,
    },
    /// Two bindings use the same human-readable name.
    DuplicateBindingName {
        /// Duplicated binding name.
        name: String,
    },
    /// One binding references the same clip more than once.
    DuplicateLayer {
        /// Slot containing the duplicate layer.
        slot: MotionSlotId,
        /// Repeated clip reference.
        clip: AssetId,
    },
    /// An event time is negative or non-finite.
    InvalidEventTime {
        /// Slot containing the event.
        slot: MotionSlotId,
        /// Event name used for the diagnostic.
        event: String,
    },
    /// An event name is blank.
    BlankEventName {
        /// Slot containing the event.
        slot: MotionSlotId,
    },
}

impl fmt::Display for AnimationSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid animation-set JSON: {error}"),
            Self::UnsupportedVersion { found } => write!(
                formatter,
                "unsupported animation-set schema version {found}; expected {ANIMATION_SET_SCHEMA_VERSION}"
            ),
            Self::BlankBindingName { slot } => {
                write!(formatter, "animation slot `{slot}` has a blank binding name")
            }
            Self::DuplicateBindingName { name } => {
                write!(formatter, "animation binding name `{name}` is duplicated")
            }
            Self::DuplicateLayer { slot, clip } => write!(
                formatter,
                "animation slot `{slot}` references clip `{clip}` more than once"
            ),
            Self::InvalidEventTime { slot, event } => write!(
                formatter,
                "animation slot `{slot}` event `{event}` has an invalid time"
            ),
            Self::BlankEventName { slot } => {
                write!(formatter, "animation slot `{slot}` has an event with a blank name")
            }
        }
    }
}

impl std::error::Error for AnimationSetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_set_round_trip_preserves_cross_source_clip_bindings() {
        let graph = AssetId::generate();
        let idle_slot = MotionSlotId::generate();
        let attack_slot = MotionSlotId::generate();
        let mut set = AnimationSet::new(graph);
        set.bindings.insert(
            idle_slot,
            AnimationBinding {
                name: "idle".to_owned(),
                clip: AssetId::generate(),
                overlays: Vec::new(),
                events: Vec::new(),
            },
        );
        set.bindings.insert(
            attack_slot,
            AnimationBinding {
                name: "attack".to_owned(),
                clip: AssetId::generate(),
                overlays: Vec::new(),
                events: vec![AnimationSetEvent {
                    time: 0.25,
                    name: "attack.hit".to_owned(),
                }],
            },
        );

        let json = set
            .to_canonical_json()
            .expect("valid animation set must serialize");
        let loaded = AnimationSet::from_json(&json).expect("serialized set must load");
        assert_eq!(loaded, set);
    }

    #[test]
    fn animation_set_without_graph_round_trips_with_explicit_null() {
        let set = AnimationSet::empty();

        let json = set
            .to_canonical_json()
            .expect("graphless set must serialize");
        assert!(json.contains("\"graph\": null"));
        let loaded = AnimationSet::from_json(&json).expect("graphless set must load");

        assert_eq!(loaded, set);
    }

    #[test]
    fn animation_set_missing_graph_field_is_rejected() {
        assert!(matches!(
            AnimationSet::from_json(r#"{"schema_version":1,"bindings":{}}"#),
            Err(AnimationSetError::Json(_))
        ));
    }

    #[test]
    fn animation_set_rejects_duplicate_display_names() {
        let mut set = AnimationSet::new(AssetId::generate());
        for _ in 0..2 {
            set.bindings.insert(
                MotionSlotId::generate(),
                AnimationBinding {
                    name: "idle".to_owned(),
                    clip: AssetId::generate(),
                    overlays: Vec::new(),
                    events: Vec::new(),
                },
            );
        }

        assert!(matches!(
            set.validate(),
            Err(AnimationSetError::DuplicateBindingName { .. })
        ));
    }

    #[test]
    fn animation_set_rejects_negative_event_time() {
        let mut set = AnimationSet::new(AssetId::generate());
        set.bindings.insert(
            MotionSlotId::generate(),
            AnimationBinding {
                name: "attack".to_owned(),
                clip: AssetId::generate(),
                overlays: Vec::new(),
                events: vec![AnimationSetEvent {
                    time: -0.1,
                    name: "attack.hit".to_owned(),
                }],
            },
        );

        assert!(matches!(
            set.validate(),
            Err(AnimationSetError::InvalidEventTime { .. })
        ));
    }

    #[test]
    fn empty_overlays_remain_current_writer_omission() {
        let graph = AssetId::generate();
        let slot = MotionSlotId::generate();
        let clip = AssetId::generate();
        let json = format!(
            "{{\"schema_version\":1,\"graph\":\"{}\",\"bindings\":{{\"{}\":{{\"name\":\"idle\",\"clip\":\"{}\",\"events\":[]}}}}}}",
            graph.as_str(),
            slot.as_str(),
            clip.as_str()
        );
        let loaded = AnimationSet::from_json(&json).expect("current omitted overlays must load");
        assert!(loaded.bindings[&slot].overlays.is_empty());
    }

    #[test]
    fn duplicate_primary_or_overlay_clip_is_rejected() {
        let slot = MotionSlotId::generate();
        let clip = AssetId::generate();
        let mut set = AnimationSet::new(AssetId::generate());
        set.bindings.insert(
            slot,
            AnimationBinding {
                name: "motion".to_owned(),
                clip: clip.clone(),
                overlays: vec![clip],
                events: Vec::new(),
            },
        );
        assert!(matches!(
            set.validate(),
            Err(AnimationSetError::DuplicateLayer { .. })
        ));
    }
}
