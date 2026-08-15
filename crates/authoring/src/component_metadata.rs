//! Sidecar metadata for project-local Rust components.
//!
//! Component identity is editor-owned data stored beside the Rust source. The
//! source path and Rust type remain mutable presentation details, while the
//! sidecar's component ID is generated once and persists in scenes and
//! prefabs for the lifetime of the logical component type.

use crate::id::ComponentTypeId;
use crate::persist::{replace_file_contents, PersistError};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Schema version written into new Rust component sidecars.
pub const COMPONENT_METADATA_SCHEMA_VERSION: u32 = 1;

/// Persisted metadata paired with one project Rust component source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentMetadata {
    /// Version of this sidecar document.
    pub schema_version: u32,
    /// Stable component type ID stored by scenes and prefabs.
    pub component_id: String,
}

impl ComponentMetadata {
    /// Creates version-one metadata after validating the supplied ID.
    ///
    /// # Errors
    ///
    /// Returns [`ComponentMetadataError::InvalidComponentId`] when the ID is
    /// not a valid project component ID.
    pub fn new(component_id: impl Into<String>) -> Result<Self, ComponentMetadataError> {
        let component_id = component_id.into();
        validate_project_component_id(&component_id)?;
        Ok(Self {
            schema_version: COMPONENT_METADATA_SCHEMA_VERSION,
            component_id,
        })
    }
}

/// Returns the sidecar path paired with a Rust source path.
///
/// `player.rs` maps to `player.rs.meta.json`; the function never embeds or
/// serializes the source path into the metadata document.
pub fn component_metadata_path(source_path: &Path) -> PathBuf {
    let mut file_name = source_path.file_name().unwrap_or_default().to_os_string();
    file_name.push(".meta.json");
    source_path.with_file_name(file_name)
}

/// Loads and validates one component sidecar.
///
/// # Errors
///
/// Returns a typed error for missing/unreadable files, malformed JSON,
/// unsupported schema versions, and invalid stable component IDs.
pub fn load_component_metadata(path: &Path) -> Result<ComponentMetadata, ComponentMetadataError> {
    let json = fs::read_to_string(path).map_err(|source| ComponentMetadataError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata: ComponentMetadata =
        serde_json::from_str(&json).map_err(|source| ComponentMetadataError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.schema_version != COMPONENT_METADATA_SCHEMA_VERSION {
        return Err(ComponentMetadataError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: metadata.schema_version,
            supported: COMPONENT_METADATA_SCHEMA_VERSION,
        });
    }
    validate_project_component_id(&metadata.component_id)?;
    Ok(metadata)
}

/// Atomically writes canonical component sidecar JSON.
///
/// # Errors
///
/// Returns a validation error before writing, or a persistence error when the
/// atomic replacement cannot be completed.
pub fn write_component_metadata(
    path: &Path,
    metadata: &ComponentMetadata,
) -> Result<(), ComponentMetadataError> {
    if metadata.schema_version != COMPONENT_METADATA_SCHEMA_VERSION {
        return Err(ComponentMetadataError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: metadata.schema_version,
            supported: COMPONENT_METADATA_SCHEMA_VERSION,
        });
    }
    validate_project_component_id(&metadata.component_id)?;
    let mut json =
        serde_json::to_string_pretty(metadata).map_err(|source| ComponentMetadataError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    json.push('\n');
    replace_file_contents(path, &json).map_err(ComponentMetadataError::Persist)
}

/// Validates IDs accepted in project component metadata.
///
/// Current project components use opaque `game.c_<lowercase ULID>` IDs. The
/// source path, Rust type name, and display name are intentionally not encoded
/// into persisted component identity.
pub fn validate_project_component_id(component_id: &str) -> Result<(), ComponentMetadataError> {
    ComponentTypeId::try_new(component_id.to_owned()).map_err(|source| {
        ComponentMetadataError::InvalidComponentId {
            component_id: component_id.to_owned(),
            reason: source.to_string(),
        }
    })?;
    let Some(suffix) = component_id.strip_prefix("game.c_") else {
        return Err(ComponentMetadataError::InvalidComponentId {
            component_id: component_id.to_owned(),
            reason: "project component IDs must use `game.c_` followed by a 26-character lowercase Crockford ULID".to_owned(),
        });
    };
    let is_valid = suffix.len() == 26
        && suffix
            .as_bytes()
            .first()
            .is_some_and(|byte| matches!(byte, b'0'..=b'7'))
        && suffix.bytes().all(|byte| {
            matches!(
                byte,
                b'0'..=b'9'
                    | b'a'..=b'h'
                    | b'j'
                    | b'k'
                    | b'm'
                    | b'n'
                    | b'p'..=b't'
                    | b'v'..=b'z'
            )
        });
    if !is_valid {
        return Err(ComponentMetadataError::InvalidComponentId {
            component_id: component_id.to_owned(),
            reason: "project component IDs must use `game.c_` followed by a 26-character lowercase Crockford ULID".to_owned(),
        });
    }
    Ok(())
}

/// Failures while reading, validating, or writing component metadata.
#[derive(Debug)]
pub enum ComponentMetadataError {
    /// The sidecar could not be read.
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying filesystem failure.
        source: io::Error,
    },
    /// The sidecar was not valid JSON for the current schema.
    Json {
        /// Sidecar path being parsed.
        path: PathBuf,
        /// Underlying JSON failure.
        source: serde_json::Error,
    },
    /// The sidecar schema version is unsupported.
    UnsupportedVersion {
        /// Sidecar path carrying the unsupported version.
        path: PathBuf,
        /// Version found in the file.
        found: u32,
        /// Version supported by this build.
        supported: u32,
    },
    /// The stable component ID is malformed or outside the current project ID format.
    InvalidComponentId {
        /// Rejected ID text.
        component_id: String,
        /// Actionable validation reason.
        reason: String,
    },
    /// Atomic sidecar replacement failed.
    Persist(PersistError),
}

impl fmt::Display for ComponentMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "could not read {}: {source}", path.display())
            }
            Self::Json { path, source } => {
                write!(formatter, "invalid component metadata {}: {source}", path.display())
            }
            Self::UnsupportedVersion {
                path,
                found,
                supported,
            } => write!(
                formatter,
                "component metadata {} uses schema version {found}, but this build expects {supported}",
                path.display()
            ),
            Self::InvalidComponentId {
                component_id,
                reason,
            } => write!(formatter, "invalid component ID `{component_id}`: {reason}"),
            Self::Persist(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for ComponentMetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Persist(source) => Some(source),
            Self::UnsupportedVersion { .. } | Self::InvalidComponentId { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_path_appends_meta_json_to_complete_source_name() {
        assert_eq!(
            component_metadata_path(Path::new("game/src/components/player.rs")),
            Path::new("game/src/components/player.rs.meta.json")
        );
    }

    #[test]
    fn metadata_roundtrip_is_canonical_and_contains_no_source_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("player.rs.meta.json");
        let metadata = ComponentMetadata::new("game.c_01kxtq56q3qxhqnqh86mmp758j").unwrap();

        write_component_metadata(&path, &metadata).unwrap();

        assert_eq!(load_component_metadata(&path).unwrap(), metadata);
        let json = fs::read_to_string(path).unwrap();
        assert!(!json.contains("source"));
        assert!(json.ends_with('\n'));
    }

    #[test]
    fn malformed_json_and_invalid_opaque_ids_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broken.rs.meta.json");
        fs::write(&path, "{").unwrap();
        assert!(matches!(
            load_component_metadata(&path),
            Err(ComponentMetadataError::Json { .. })
        ));
        assert!(matches!(
            ComponentMetadata::new("game.c_not_a_ulid"),
            Err(ComponentMetadataError::InvalidComponentId { .. })
        ));
    }

    #[test]
    fn dotted_project_component_ids_are_rejected() {
        assert!(matches!(
            ComponentMetadata::new("game.player_controller"),
            Err(ComponentMetadataError::InvalidComponentId { .. })
        ));
    }
}
