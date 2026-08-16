//! Stable persistent identifiers for authoring objects.
//!
//! All persisted authoring objects carry a [`StableId`] in the format
//! `<prefix>_<ULID>`. Identifiers are opaque, generated once, and never
//! changed when an object is renamed.
//!
//! See `AI_FRIENDLY_AUTHORING_SPEC.md` §7.1 and ADR 0004 for the full policy.

use serde::de;
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

/// A persistent opaque identifier in the format `<prefix>_<ULID>`.
///
/// `StableId` is the common string carrier shared by all typed identifier
/// wrappers ([`EntityId`], [`AssetId`], etc.). Consumers MUST treat the
/// string form as opaque; the internal format is an implementation detail,
/// not a parsing contract.
///
/// Identifiers remain valid across editor sessions, renames, and runtime
/// rebuilds. They MUST NOT be derived from memory addresses, array offsets,
/// or display positions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StableId(String);

impl StableId {
    /// Creates a `StableId` from any string.
    ///
    /// No format validation is performed here. Prefix and ULID suffix
    /// validation is enforced by typed wrappers such as
    /// [`EntityId::from_stable_id`].
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Returns the string representation of this identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StableId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Reports why a typed identifier could not be constructed from a [`StableId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    /// The stable ID string does not begin with the required prefix.
    WrongPrefix {
        /// The expected prefix without the trailing underscore separator.
        expected: &'static str,
        /// The actual string that was provided.
        actual: String,
    },
    /// The suffix after `<prefix>_` is not a valid 26-character Crockford
    /// base32 ULID string.
    ///
    /// Valid characters are the digits `0`–`9` and the uppercase letters
    /// `A`–`Z` excluding `I`, `L`, `O`, and `U`.
    InvalidSuffix {
        /// The full identifier string that was rejected.
        actual: String,
    },
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPrefix { expected, actual } => {
                write!(f, "expected prefix `{expected}_`, got `{actual}`")
            }
            Self::InvalidSuffix { actual } => write!(
                f,
                "stable ID `{actual}` has an invalid or missing ULID suffix \
                 (expected 26 uppercase Crockford base32 characters)"
            ),
        }
    }
}

impl std::error::Error for IdError {}

/// Returns `true` when `s` is a valid 26-character Crockford base32 string
/// as produced by [`ulid::Ulid`].
///
/// The Crockford base32 alphabet uses the digits `0`–`9` and the uppercase
/// letters `A`–`Z` excluding `I`, `L`, `O`, and `U`. Lowercase characters
/// are not accepted.
fn is_valid_ulid_suffix(s: &str) -> bool {
    s.len() == 26
        && s.as_bytes()
            .first()
            .is_some_and(|byte| matches!(byte, b'0'..=b'7'))
        && s.bytes().all(|b| {
            matches!(
                b,
                b'0'..=b'9'
                    | b'A'..=b'H'
                    | b'J'
                    | b'K'
                    | b'M'
                    | b'N'
                    | b'P'..=b'T'
                    | b'V'..=b'Z'
            )
        })
}

/// Generates typed stable identifier wrappers that share a common prefix.
///
/// Each generated type:
/// - Wraps a [`StableId`] and validates the required prefix and ULID suffix.
/// - Provides `generate()` for creating new identifiers using ULID.
/// - Serializes transparently as a plain string.
macro_rules! define_id {
    ($(#[$attr:meta])* $name:ident, $prefix:literal) => {
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(StableId);

        impl $name {
            const PREFIX: &'static str = $prefix;

            /// Generates a new identifier using the current time and random data.
            ///
            /// The returned identifier is unique within the same process with
            /// very high probability. It uses the format `<prefix>_<ULID>`.
            pub fn generate() -> Self {
                let ulid = ulid::Ulid::new();
                Self(StableId::new(format!(concat!($prefix, "_{}"), ulid)))
            }

            /// Wraps a [`StableId`] as this identifier type after validating
            /// the required prefix and ULID suffix.
            ///
            /// # Errors
            ///
            /// Returns [`IdError::WrongPrefix`] when the stable ID does not
            /// begin with `<prefix>_`.
            ///
            /// Returns [`IdError::InvalidSuffix`] when the part after
            /// `<prefix>_` is not a valid 26-character Crockford base32 ULID
            /// string.
            pub fn from_stable_id(id: StableId) -> Result<Self, IdError> {
                let s = id.as_str();
                let prefix_with_sep = concat!($prefix, "_");
                if !s.starts_with(prefix_with_sep) {
                    return Err(IdError::WrongPrefix {
                        expected: Self::PREFIX,
                        actual: s.to_owned(),
                    });
                }
                let suffix = &s[prefix_with_sep.len()..];
                if !is_valid_ulid_suffix(suffix) {
                    return Err(IdError::InvalidSuffix {
                        actual: s.to_owned(),
                    });
                }
                Ok(Self(id))
            }

            /// Returns a reference to the underlying [`StableId`].
            pub fn as_stable_id(&self) -> &StableId {
                &self.0
            }

            /// Returns the string representation of this identifier.
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let id = StableId::deserialize(deserializer)?;
                Self::from_stable_id(id).map_err(de::Error::custom)
            }
        }
    };
}

define_id!(
    /// A stable logical identifier for one project.
    ///
    /// Format: `project_<ULID>`. It survives project directory moves and is
    /// distinct from the canonical directory used for editor-process locking.
    ProjectId,
    "project"
);

define_id!(
    /// A project-wide stable identifier for an authoring entity.
    ///
    /// Format: `entity_<ULID>`. Never changes when the entity is renamed.
    EntityId,
    "entity"
);

define_id!(
    /// A project-wide stable identifier for an authoring asset.
    ///
    /// Format: `asset_<ULID>`. Never changes when the asset is renamed.
    AssetId,
    "asset"
);

define_id!(
    /// A project-wide stable identifier for a graph.
    ///
    /// Format: `graph_<ULID>`. Never changes when the graph is renamed.
    GraphId,
    "graph"
);

define_id!(
    /// A graph-local stable identifier for a graph node.
    ///
    /// Format: `node_<ULID>`. Unique within the containing graph.
    NodeId,
    "node"
);

define_id!(
    /// A graph-local stable identifier for a graph edge.
    ///
    /// Format: `edge_<ULID>`. Unique within the containing graph.
    EdgeId,
    "edge"
);

define_id!(
    /// A node-schema-local stable identifier for a port.
    ///
    /// Format: `port_<ULID>`. Unique within the containing node schema.
    PortId,
    "port"
);

define_id!(
    /// A graph-local stable identifier for a group.
    ///
    /// Format: `group_<ULID>`. Unique within the containing graph.
    GroupId,
    "group"
);

define_id!(
    /// A stable animation-graph motion slot identifier.
    ///
    /// Format: `motion_<ULID>`. Animation states persist this identifier and
    /// animation sets bind it to imported animation-clip assets. The ID does
    /// not change when the slot's display name or bound clip changes.
    MotionSlotId,
    "motion"
);

define_id!(
    /// A stable identity for one Timeline authoring document.
    ///
    /// Format: `timeline_<ULID>`. It survives file renames and Editor reopen.
    TimelineId,
    "timeline"
);

define_id!(
    /// A Timeline-local stable track identifier.
    ///
    /// Format: `track_<ULID>`. Moving or renaming a track never changes it.
    TimelineTrackId,
    "track"
);

define_id!(
    /// A Timeline-local stable interval-clip identifier.
    ///
    /// Format: `clip_<ULID>`. Trimming or moving a clip never changes it.
    TimelineClipId,
    "clip"
);

define_id!(
    /// A Timeline-local stable marker/event identifier.
    ///
    /// Format: `marker_<ULID>`. Moving a marker never changes it.
    TimelineMarkerId,
    "marker"
);

// ---------------------------------------------------------------------------
// Deterministic sub-asset ID derivation (ADR 0032)
// ---------------------------------------------------------------------------

impl AssetId {
    /// Creates a deterministic sub-asset ID from `parent` and `discriminant`.
    ///
    /// The returned ID is stable: re-calling with the same arguments always
    /// produces the same `AssetId`.  This is used by the glTF import pipeline
    /// (Phase 36) to ensure that mesh, material, and texture IDs remain stable
    /// across reimports.
    ///
    /// Internally uses two rounds of FNV-1a-64 to produce a 128-bit value that
    /// is encoded as a ULID string.
    pub fn derive(parent: &AssetId, discriminant: &str) -> AssetId {
        let input = format!("{}:{}", parent.as_str(), discriminant);
        let hash = derive_u128(input.as_bytes());
        let ulid = ulid::Ulid::from(hash);
        AssetId(StableId::new(format!("asset_{ulid}")))
    }
}

fn fnv1a_u64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn derive_u128(data: &[u8]) -> u128 {
    let lo = fnv1a_u64(data);
    let hi_seed = lo ^ 0xdead_beef_cafe_1234_u64;
    let hi_input: Vec<u8> = hi_seed.to_le_bytes().iter().chain(data).copied().collect();
    let hi = fnv1a_u64(&hi_input);
    ((hi as u128) << 64) | lo as u128
}

/// Identifies a component type in the authoring model.
///
/// Component type IDs are stable dotted strings such as `"gameplay.health"`.
/// They MUST NOT change after the component type is used in project files.
/// Unlike object identifiers, component type IDs are defined by code and are
/// not ULID-based.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ComponentTypeId(String);

impl ComponentTypeId {
    /// Creates a `ComponentTypeId` from a lowercase dotted namespace string.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is empty, lacks a namespace separator,
    /// contains an empty segment, or contains characters other than lowercase
    /// ASCII letters, digits, and underscores.
    pub fn try_new(s: impl Into<String>) -> Result<Self, ComponentTypeIdError> {
        let s = s.into();
        validate_component_type_id(&s)?;
        Ok(Self(s))
    }

    /// Creates a `ComponentTypeId` from a known-valid lowercase dotted string.
    ///
    /// # Panics
    ///
    /// Panics when `s` is not a valid component type identifier. Use
    /// [`ComponentTypeId::try_new`] for untrusted input.
    pub fn new(s: impl Into<String>) -> Self {
        Self::try_new(s).expect("component type ID must be a valid lowercase dotted namespace")
    }

    /// Returns the string representation of this component type identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ComponentTypeId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(de::Error::custom)
    }
}

impl fmt::Display for ComponentTypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Reports why a component type identifier was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentTypeIdError {
    /// The identifier was empty.
    Empty,
    /// The identifier did not contain a namespace separator.
    MissingNamespace,
    /// One of the dot-separated namespace segments was empty.
    EmptySegment,
    /// A segment contained an unsupported character or did not start with a
    /// lowercase ASCII letter.
    InvalidSegment {
        /// The rejected segment.
        segment: String,
    },
}

impl fmt::Display for ComponentTypeIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("component type ID must not be empty"),
            Self::MissingNamespace => {
                formatter.write_str("component type ID must use a dotted namespace")
            }
            Self::EmptySegment => {
                formatter.write_str("component type ID must not contain empty segments")
            }
            Self::InvalidSegment { segment } => write!(
                formatter,
                "component type ID segment `{segment}` must start with a lowercase ASCII letter and contain only lowercase ASCII letters, digits, or underscores"
            ),
        }
    }
}

impl std::error::Error for ComponentTypeIdError {}

fn validate_component_type_id(value: &str) -> Result<(), ComponentTypeIdError> {
    if value.is_empty() {
        return Err(ComponentTypeIdError::Empty);
    }
    if !value.contains('.') {
        return Err(ComponentTypeIdError::MissingNamespace);
    }
    for segment in value.split('.') {
        if segment.is_empty() {
            return Err(ComponentTypeIdError::EmptySegment);
        }
        let mut bytes = segment.bytes();
        let starts_with_lowercase = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
        let remaining_are_valid =
            bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if !starts_with_lowercase || !remaining_are_valid {
            return Err(ComponentTypeIdError::InvalidSegment {
                segment: segment.to_owned(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 26-character Crockford base32 string used in tests that require a
    // syntactically valid ULID suffix. All characters are drawn from the
    // valid set (0-9, A-H, J, K, M, N, P-T, V-Z).
    const VALID_ULID_SUFFIX: &str = "01234567890ABCDEFGHJKMNPQR";

    #[test]
    fn entity_id_has_entity_prefix() {
        let id = EntityId::generate();
        assert!(
            id.as_str().starts_with("entity_"),
            "expected entity_ prefix, got: {id}"
        );
    }

    #[test]
    fn asset_id_has_asset_prefix() {
        let id = AssetId::generate();
        assert!(id.as_str().starts_with("asset_"), "got: {id}");
    }

    #[test]
    fn graph_id_has_graph_prefix() {
        let id = GraphId::generate();
        assert!(id.as_str().starts_with("graph_"), "got: {id}");
    }

    #[test]
    fn node_id_has_node_prefix() {
        let id = NodeId::generate();
        assert!(id.as_str().starts_with("node_"), "got: {id}");
    }

    #[test]
    fn entity_id_from_wrong_prefix_returns_wrong_prefix_error() {
        let stable = StableId::new(format!("asset_{VALID_ULID_SUFFIX}"));
        let result = EntityId::from_stable_id(stable);
        assert!(
            matches!(
                result,
                Err(IdError::WrongPrefix {
                    expected: "entity",
                    ..
                })
            ),
            "unexpected result: {result:?}"
        );
    }

    #[test]
    fn asset_id_from_entity_prefix_returns_wrong_prefix_error() {
        let stable = StableId::new(format!("entity_{VALID_ULID_SUFFIX}"));
        assert!(matches!(
            AssetId::from_stable_id(stable),
            Err(IdError::WrongPrefix { .. })
        ));
    }

    #[test]
    fn entity_id_from_valid_prefix_and_ulid_succeeds() {
        let stable = StableId::new(format!("entity_{VALID_ULID_SUFFIX}"));
        assert!(
            EntityId::from_stable_id(stable).is_ok(),
            "valid prefix and ULID suffix must be accepted"
        );
    }

    #[test]
    fn entity_id_from_stable_id_rejects_empty_suffix() {
        let stable = StableId::new("entity_");
        assert!(
            matches!(
                EntityId::from_stable_id(stable),
                Err(IdError::InvalidSuffix { .. })
            ),
            "empty suffix must be rejected"
        );
    }

    #[test]
    fn entity_id_from_stable_id_rejects_short_suffix() {
        let stable = StableId::new("entity_SHORT");
        assert!(
            matches!(
                EntityId::from_stable_id(stable),
                Err(IdError::InvalidSuffix { .. })
            ),
            "short suffix must be rejected"
        );
    }

    #[test]
    fn entity_id_from_stable_id_rejects_invalid_crockford_chars() {
        // 'I', 'L', 'O', 'U' are excluded from the Crockford base32 alphabet
        // because they look like digits and cause transcription errors.
        let stable = StableId::new("entity_IIIIIIIIIIIIIIIIIIIIIIIIII");
        assert!(
            matches!(
                EntityId::from_stable_id(stable),
                Err(IdError::InvalidSuffix { .. })
            ),
            "excluded Crockford chars I/L/O/U must be rejected"
        );
    }

    #[test]
    fn entity_id_from_stable_id_rejects_lowercase_ulid() {
        // ULIDs generated by this engine are uppercase. Lowercase input is
        // rejected to prevent silent case-normalization surprises.
        let lower = format!("entity_{}", VALID_ULID_SUFFIX.to_lowercase());
        let stable = StableId::new(lower);
        assert!(
            matches!(
                EntityId::from_stable_id(stable),
                Err(IdError::InvalidSuffix { .. })
            ),
            "lowercase ULID suffix must be rejected"
        );
    }

    #[test]
    fn entity_id_from_stable_id_rejects_out_of_range_ulid() {
        let stable = StableId::new("entity_ZZZZZZZZZZZZZZZZZZZZZZZZZZ");
        assert!(matches!(
            EntityId::from_stable_id(stable),
            Err(IdError::InvalidSuffix { .. })
        ));
    }

    #[test]
    fn entity_id_roundtrip_via_from_stable_id() {
        let original = EntityId::generate();
        let stable = original.as_stable_id().clone();
        let recovered = EntityId::from_stable_id(stable).expect("generated id must round-trip");
        assert_eq!(original, recovered);
    }

    #[test]
    fn stable_id_serializes_as_plain_string() {
        let id = EntityId::generate();
        let json = serde_json::to_string(&id).unwrap();
        assert!(json.starts_with('"'));
        assert!(json.ends_with('"'));
    }

    #[test]
    fn entity_id_survives_json_roundtrip() {
        let id = EntityId::generate();
        let json = serde_json::to_string(&id).unwrap();
        let loaded: EntityId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, loaded);
    }

    #[test]
    fn typed_id_deserialization_rejects_wrong_prefix() {
        let json = format!(r#""asset_{VALID_ULID_SUFFIX}""#);
        assert!(serde_json::from_str::<EntityId>(&json).is_err());
    }

    #[test]
    fn typed_id_deserialization_rejects_invalid_ulid_suffix() {
        assert!(serde_json::from_str::<EntityId>(r#""entity_not_a_ulid""#).is_err());
    }

    #[test]
    fn two_generated_entity_ids_are_distinct() {
        let a = EntityId::generate();
        let b = EntityId::generate();
        assert_ne!(a, b);
    }

    #[test]
    fn component_type_id_preserves_dotted_path() {
        let id = ComponentTypeId::new("gameplay.health");
        assert_eq!(id.as_str(), "gameplay.health");
    }

    #[test]
    fn component_type_id_rejects_empty_and_invalid_formats() {
        assert!(matches!(
            ComponentTypeId::try_new(""),
            Err(ComponentTypeIdError::Empty)
        ));
        assert!(matches!(
            ComponentTypeId::try_new("health"),
            Err(ComponentTypeIdError::MissingNamespace)
        ));
        assert!(matches!(
            ComponentTypeId::try_new("gameplay..health"),
            Err(ComponentTypeIdError::EmptySegment)
        ));
        assert!(matches!(
            ComponentTypeId::try_new("Gameplay.health"),
            Err(ComponentTypeIdError::InvalidSegment { .. })
        ));
    }

    #[test]
    fn component_type_id_deserialization_rejects_invalid_format() {
        assert!(serde_json::from_str::<ComponentTypeId>(r#""""#).is_err());
        assert!(serde_json::from_str::<ComponentTypeId>(r#""not_dotted""#).is_err());
    }

    #[test]
    fn asset_id_derive_is_deterministic() {
        let parent = AssetId::generate();
        let a = AssetId::derive(&parent, "mesh:0");
        let b = AssetId::derive(&parent, "mesh:0");
        assert_eq!(a, b, "derive must be deterministic for equal inputs");
    }

    #[test]
    fn asset_id_derive_differs_for_different_discriminants() {
        let parent = AssetId::generate();
        let a = AssetId::derive(&parent, "mesh:0");
        let b = AssetId::derive(&parent, "mesh:1");
        assert_ne!(a, b, "different discriminants must produce different IDs");
    }

    #[test]
    fn asset_id_derive_has_asset_prefix() {
        let parent = AssetId::generate();
        let derived = AssetId::derive(&parent, "texture:0");
        assert!(
            derived.as_str().starts_with("asset_"),
            "derived ID must have asset_ prefix"
        );
    }

    #[test]
    fn asset_id_derive_differs_for_different_parents() {
        let parent_a = AssetId::generate();
        let parent_b = AssetId::generate();
        let a = AssetId::derive(&parent_a, "mesh:0");
        let b = AssetId::derive(&parent_b, "mesh:0");
        assert_ne!(a, b, "different parents must produce different derived IDs");
    }
}
