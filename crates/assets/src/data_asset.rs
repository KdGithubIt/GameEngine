//! Generic author-owned data assets and stable component references.

use crate::asset::AssetManifest;
use engine_authoring::id::AssetId;
use engine_authoring::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Current persisted schema version for `*.data.json` documents.
pub const DATA_ASSET_SCHEMA_VERSION: u32 = 1;
/// File suffix reserved for generic data assets.
pub const DATA_ASSET_FILE_SUFFIX: &str = ".data.json";
const DATA_ASSET_REF_MARKER: &str = "data_asset_ref";

/// Self-describing reusable values stored as one project asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataAssetDocument {
    /// Persisted document schema version.
    pub schema_version: u32,
    /// Human-readable name shown by editor tooling.
    pub display_name: String,
    /// Deterministically ordered reusable values keyed by author-facing names.
    pub fields: BTreeMap<String, Value>,
}

impl DataAssetDocument {
    /// Creates an empty version-one data asset.
    pub fn new(display_name: impl Into<String>) -> Self {
        Self {
            schema_version: DATA_ASSET_SCHEMA_VERSION,
            display_name: display_name.into(),
            fields: BTreeMap::new(),
        }
    }

    /// Parses and validates one data asset document.
    ///
    /// # Errors
    ///
    /// Returns an error when the JSON is malformed, a current-format field is
    /// missing, or the document contract is invalid.
    pub fn from_json(json: &str) -> Result<Self, DataAssetError> {
        let document = serde_json::from_str::<Self>(json).map_err(DataAssetError::Json)?;
        document.validate()?;
        Ok(document)
    }

    /// Serializes this document as deterministic pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns an error when validation or JSON serialization fails.
    pub fn to_canonical_json(&self) -> Result<String, DataAssetError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(DataAssetError::Json)
    }

    /// Validates the persisted document contract.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported versions, blank names, invalid field
    /// identifiers, or non-finite floating-point values.
    pub fn validate(&self) -> Result<(), DataAssetError> {
        if self.schema_version != DATA_ASSET_SCHEMA_VERSION {
            return Err(DataAssetError::UnsupportedVersion {
                found: self.schema_version,
            });
        }
        if self.display_name.trim().is_empty() {
            return Err(DataAssetError::EmptyDisplayName);
        }
        for (name, value) in &self.fields {
            if !is_valid_field_name(name) {
                return Err(DataAssetError::InvalidFieldName { name: name.clone() });
            }
            validate_finite_values(name, value)?;
        }
        Ok(())
    }

    /// Loads the document referenced by a registered manifest asset.
    ///
    /// # Errors
    ///
    /// Returns an error when the reference is missing, is not a data asset,
    /// the file cannot be read, or its contents are invalid.
    pub fn load_registered(
        manifest: &AssetManifest,
        assets_root: &Path,
        asset: &AssetId,
    ) -> Result<Self, DataAssetError> {
        let entry = manifest
            .get(asset)
            .ok_or_else(|| DataAssetError::MissingAsset(asset.clone()))?;
        if !is_data_asset_path(Path::new(&entry.path)) {
            return Err(DataAssetError::WrongAssetKind {
                asset: asset.clone(),
                path: PathBuf::from(&entry.path),
            });
        }
        let path = assets_root.join(&entry.path);
        let json = std::fs::read_to_string(&path).map_err(|source| DataAssetError::Io {
            path: path.clone(),
            source,
        })?;
        Self::from_json(&json)
    }
}

impl Default for DataAssetDocument {
    fn default() -> Self {
        Self::new("New Data Asset")
    }
}

/// Optional stable reference stored inside a project-local component.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataAssetRef {
    asset: Option<AssetId>,
}

impl DataAssetRef {
    /// Creates an assigned reference.
    pub fn new(asset: AssetId) -> Self {
        Self { asset: Some(asset) }
    }
    /// Creates an unassigned reference suitable for component defaults.
    pub const fn unassigned() -> Self {
        Self { asset: None }
    }
    /// Returns the assigned stable asset identifier.
    pub fn asset_id(&self) -> Option<&AssetId> {
        self.asset.as_ref()
    }
    /// Returns whether this reference currently names an asset.
    pub fn is_assigned(&self) -> bool {
        self.asset.is_some()
    }
    /// Replaces the assigned asset while preserving the reference wrapper.
    pub fn set(&mut self, asset: Option<AssetId>) {
        self.asset = asset;
    }
    /// Resolves and loads the referenced document.
    ///
    /// # Errors
    ///
    /// Returns an error when the assigned manifest entry or document is invalid.
    pub fn load(
        &self,
        manifest: &AssetManifest,
        assets_root: &Path,
    ) -> Result<Option<DataAssetDocument>, DataAssetError> {
        self.asset
            .as_ref()
            .map(|asset| DataAssetDocument::load_registered(manifest, assets_root, asset))
            .transpose()
    }

    /// Converts this reference into its explicit authoring representation.
    pub fn to_authoring_value(&self) -> Value {
        let asset = match &self.asset {
            Some(asset) => Value::AssetRef(asset.clone()),
            None => Value::Null,
        };
        Value::Object(BTreeMap::from([
            (
                "$type".to_owned(),
                Value::String(DATA_ASSET_REF_MARKER.to_owned()),
            ),
            ("asset".to_owned(), asset),
        ]))
    }

    /// Decodes the explicit authoring representation used by components.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is not a data-asset reference object.
    pub fn from_authoring_value(value: &Value) -> Result<Self, String> {
        let Value::Object(fields) = value else {
            return Err("expected a data asset reference object".to_owned());
        };
        if fields.len() != 2
            || !matches!(
                fields.get("$type"),
                Some(Value::String(marker)) if marker == DATA_ASSET_REF_MARKER
            )
        {
            return Err("expected a data asset reference marker".to_owned());
        }
        match fields.get("asset") {
            Some(Value::AssetRef(asset)) => Ok(Self::new(asset.clone())),
            Some(Value::Null) => Ok(Self::unassigned()),
            Some(_) => Err("data asset reference `asset` must be an asset reference or null".to_owned()),
            None => Err("data asset reference is missing `asset`".to_owned()),
        }
    }
}

/// Reports an invalid or unavailable data asset.
#[derive(Debug)]
pub enum DataAssetError {
    /// The JSON document could not be decoded or encoded.
    Json(serde_json::Error),
    /// The persisted schema version is not supported.
    UnsupportedVersion {
        /// Version found in the document.
        found: u32,
    },
    /// The human-readable document name was blank.
    EmptyDisplayName,
    /// One field name was not a valid Rust-style identifier.
    InvalidFieldName {
        /// Rejected field name.
        name: String,
    },
    /// One nested floating-point value was NaN or infinite.
    NonFiniteValue {
        /// Human-readable property path.
        path: String,
    },
    /// The requested stable asset ID was not registered.
    MissingAsset(AssetId),
    /// The registered asset did not use the data-asset suffix.
    WrongAssetKind {
        /// Requested stable asset identifier.
        asset: AssetId,
        /// Registered project-relative path.
        path: PathBuf,
    },
    /// The registered file could not be read.
    Io {
        /// File path that failed.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
}

impl fmt::Display for DataAssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid data asset JSON: {error}"),
            Self::UnsupportedVersion { found } => write!(
                formatter,
                "unsupported data asset schema version {found}; expected {DATA_ASSET_SCHEMA_VERSION}"
            ),
            Self::EmptyDisplayName => {
                formatter.write_str("data asset display name must not be blank")
            }
            Self::InvalidFieldName { name } => write!(
                formatter,
                "data asset field `{name}` must be a Rust-style identifier"
            ),
            Self::NonFiniteValue { path } => {
                write!(formatter, "data asset value `{path}` must be finite")
            }
            Self::MissingAsset(asset) => {
                write!(formatter, "data asset `{}` is not registered", asset.as_str())
            }
            Self::WrongAssetKind { asset, path } => write!(
                formatter,
                "asset `{}` at `{}` is not a data asset",
                asset.as_str(),
                path.display()
            ),
            Self::Io { path, source } => {
                write!(formatter, "failed to read data asset {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for DataAssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Returns whether a project-relative path uses the generic data-asset suffix.
pub fn is_data_asset_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(DATA_ASSET_FILE_SUFFIX))
}

fn is_valid_field_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn validate_finite_values(path: &str, value: &Value) -> Result<(), DataAssetError> {
    match value {
        Value::F64(number) if !number.is_finite() => Err(DataAssetError::NonFiniteValue {
            path: path.to_owned(),
        }),
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                validate_finite_values(&format!("{path}[{index}]"), value)?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for (name, value) in fields {
                validate_finite_values(&format!("{path}.{name}"), value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::{ImportSettings, ManifestEntry};

    #[test]
    fn document_roundtrip_preserves_typed_values() {
        let mut document = DataAssetDocument::new("Enemy Stats");
        document.fields.insert("health".into(), Value::I64(100));
        document.fields.insert("speed".into(), Value::F64(4.5));
        let json = document
            .to_canonical_json()
            .expect("document must serialize");
        let loaded = DataAssetDocument::from_json(&json).expect("document must parse");
        assert_eq!(loaded, document);
    }

    #[test]
    fn missing_current_document_fields_are_rejected() {
        for json in [
            r#"{"display_name":"Enemy","fields":{}}"#,
            r#"{"schema_version":1,"display_name":"Enemy"}"#,
        ] {
            assert!(matches!(
                DataAssetDocument::from_json(json),
                Err(DataAssetError::Json(_))
            ));
        }
    }

    #[test]
    fn registered_document_loads_by_stable_id() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("enemy.data.json");
        let document = DataAssetDocument::new("Enemy");
        std::fs::write(
            &path,
            document.to_canonical_json().expect("serialize"),
        )
        .expect("document must be written");
        let asset = AssetId::generate();
        let mut manifest = AssetManifest::default();
        manifest.insert(
            asset.clone(),
            ManifestEntry {
                path: "enemy.data.json".into(),
                name: Some("Enemy".into()),
                import_settings: ImportSettings::default(),
            },
        );
        assert_eq!(
            DataAssetDocument::load_registered(&manifest, temporary.path(), &asset)
                .expect("registered document must load"),
            document
        );
    }
}
