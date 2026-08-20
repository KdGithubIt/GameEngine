//! Track type registry and seek capability (ADR 0126 §5, §7).
//!
//! The registry is the one place that answers what a track type is called, how
//! it behaves under discontinuous time, and how the Editor should present it.
//! Applying a track's output belongs to the composition layer; nothing here
//! reaches into a runtime domain.

use engine_authoring::TimelineTrackKind;
use std::collections::BTreeMap;

/// How one track type behaves when time jumps.
///
/// The Sequencer honours these policies rather than fabricating a result. It
/// never runs physics or VFX with a negative delta to imitate reverse playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackSeekPolicy {
    /// The result is a pure function of the target tick.
    Stateless,
    /// The target domain exposes a deterministic sample or seek operation.
    Seekable,
    /// State must be reconstructed by simulating forward from a checkpoint.
    ReplayRequired,
    /// The domain cannot answer for an arbitrary tick, and the Editor says so.
    NonSeekable,
}

impl TrackSeekPolicy {
    /// Whether scrubbing can present an exact result without reconstruction.
    pub fn is_exact_on_scrub(self) -> bool {
        matches!(self, Self::Stateless | Self::Seekable)
    }

    /// Short label the Editor shows beside a track during scrub.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stateless => "stateless",
            Self::Seekable => "seekable",
            Self::ReplayRequired => "replay required",
            Self::NonSeekable => "not seekable",
        }
    }
}

/// Registry entry for one track type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackDescriptor {
    /// Track kind this descriptor belongs to.
    pub kind: TimelineTrackKind,
    /// Stable registry identifier persisted in track records.
    pub type_id: &'static str,
    /// Editor presentation label.
    pub label: &'static str,
    /// Seek behaviour under discontinuous time.
    pub seek_policy: TrackSeekPolicy,
    /// Whether the type requires a bound entity to evaluate.
    pub requires_entity_binding: bool,
}

/// Registry of supported track types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRegistry {
    descriptors: BTreeMap<&'static str, TrackDescriptor>,
}

impl Default for TrackRegistry {
    fn default() -> Self {
        Self::with_builtin_tracks()
    }
}

impl TrackRegistry {
    /// Builds a registry containing the built-in track types.
    pub fn with_builtin_tracks() -> Self {
        let mut registry = Self {
            descriptors: BTreeMap::new(),
        };
        for descriptor in BUILTIN_TRACKS {
            registry.descriptors.insert(descriptor.type_id, descriptor);
        }
        registry
    }

    /// Registers or replaces one descriptor.
    pub fn register(&mut self, descriptor: TrackDescriptor) {
        self.descriptors.insert(descriptor.type_id, descriptor);
    }

    /// Resolves one descriptor by stable registry identifier.
    pub fn descriptor(&self, type_id: &str) -> Option<&TrackDescriptor> {
        self.descriptors.get(type_id)
    }

    /// Resolves one descriptor by track kind.
    pub fn for_kind(&self, kind: TimelineTrackKind) -> Option<&TrackDescriptor> {
        self.descriptors.get(kind.type_id())
    }

    /// Every registered descriptor in stable identifier order.
    pub fn descriptors(&self) -> impl Iterator<Item = &TrackDescriptor> {
        self.descriptors.values()
    }
}

const BUILTIN_TRACKS: [TrackDescriptor; 6] = [
    TrackDescriptor {
        kind: TimelineTrackKind::Event,
        type_id: "engine.timeline.event",
        label: "Event",
        // An event is defined by its crossing, not by the tick it is sampled
        // at, so a scrub can present it exactly without replaying anything.
        seek_policy: TrackSeekPolicy::Stateless,
        requires_entity_binding: false,
    },
    TrackDescriptor {
        kind: TimelineTrackKind::CameraCut,
        type_id: "engine.timeline.camera_cut",
        label: "Camera Cut",
        seek_policy: TrackSeekPolicy::Stateless,
        requires_entity_binding: false,
    },
    TrackDescriptor {
        kind: TimelineTrackKind::Animation,
        type_id: "engine.timeline.animation",
        label: "Animation",
        // Animation sampling is deterministic for a given time, so seeking asks
        // the animation domain for that time instead of replaying to it.
        seek_policy: TrackSeekPolicy::Seekable,
        requires_entity_binding: true,
    },
    TrackDescriptor {
        kind: TimelineTrackKind::Property,
        type_id: "engine.timeline.property",
        label: "Property",
        seek_policy: TrackSeekPolicy::Stateless,
        requires_entity_binding: true,
    },
    TrackDescriptor {
        kind: TimelineTrackKind::Audio,
        type_id: "engine.timeline.audio",
        label: "Audio",
        // ADR 0122 exposes cursor-aware managed voice startup, so the
        // composition adapter can restore a cue at its exact clip-local offset.
        seek_policy: TrackSeekPolicy::Seekable,
        requires_entity_binding: false,
    },
    TrackDescriptor {
        kind: TimelineTrackKind::Vfx,
        type_id: "engine.timeline.vfx",
        label: "VFX",
        // Particle state at a tick depends on the simulation that reached it.
        seek_policy: TrackSeekPolicy::ReplayRequired,
        requires_entity_binding: true,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_authored_track_kind_has_a_registry_descriptor() {
        let registry = TrackRegistry::default();
        for kind in TimelineTrackKind::ALL {
            let descriptor = registry
                .for_kind(kind)
                .unwrap_or_else(|| panic!("{} must be registered", kind.label()));
            assert_eq!(descriptor.type_id, kind.type_id());
            assert_eq!(descriptor.kind, kind);
        }
    }

    #[test]
    fn scrub_exactness_follows_the_declared_seek_policy() {
        let registry = TrackRegistry::default();
        assert!(
            registry
                .for_kind(TimelineTrackKind::Property)
                .expect("property")
                .seek_policy
                .is_exact_on_scrub()
        );
        assert!(
            !registry
                .for_kind(TimelineTrackKind::Vfx)
                .expect("vfx")
                .seek_policy
                .is_exact_on_scrub()
        );
        let audio = registry
            .for_kind(TimelineTrackKind::Audio)
            .expect("audio")
            .seek_policy;
        assert!(audio.is_exact_on_scrub());
        assert_eq!(audio.label(), "seekable");
    }

    #[test]
    fn a_new_track_type_can_be_registered_without_touching_the_core() {
        let mut registry = TrackRegistry::default();
        registry.register(TrackDescriptor {
            kind: TimelineTrackKind::Property,
            type_id: "project.timeline.custom",
            label: "Custom",
            seek_policy: TrackSeekPolicy::ReplayRequired,
            requires_entity_binding: true,
        });
        let descriptor = registry
            .descriptor("project.timeline.custom")
            .expect("custom descriptor");
        assert_eq!(descriptor.seek_policy, TrackSeekPolicy::ReplayRequired);
        assert_eq!(registry.descriptors().count(), 7);
    }
}
