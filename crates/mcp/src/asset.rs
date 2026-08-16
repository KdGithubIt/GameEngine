//! MCP adapters for shared project asset discovery and inspection.

use crate::{McpToolDescriptor, McpToolError};
use engine_assets::asset::AssetManifest;
use engine_assets::catalog::{AssetCatalogSearch, AssetCatalogService, AssetInspection};
use engine_authoring::{AssetId, AuthoringPermissions, ProjectRoot};
use serde::Deserialize;
use serde_json::json;

/// Input for `asset.search`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AssetSearchInput {
    /// Case-insensitive search text. Empty text returns the complete catalog.
    #[serde(default)]
    pub query: String,
}

/// Input for `asset.inspect`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AssetInspectInput {
    /// Stable asset identity selected from search or another authoring reference.
    pub asset_id: AssetId,
}

/// Asset MCP tool handler collection backed by the shared catalog service.
pub struct AssetMcpTools {
    service: AssetCatalogService,
}

impl AssetMcpTools {
    /// Creates asset tool handlers backed by the shared catalog service.
    pub fn new() -> Self {
        Self {
            service: AssetCatalogService::new(),
        }
    }

    /// Returns tool descriptors for registration by the MCP transport layer.
    pub fn tool_descriptors(&self) -> Vec<McpToolDescriptor> {
        vec![
            McpToolDescriptor {
                name: "asset.search".into(),
                description: "Search project assets and imported sub-assets by stable ID, name, path, or imported kind."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"}
                    },
                    "additionalProperties": false
                }),
            },
            McpToolDescriptor {
                name: "asset.inspect".into(),
                description: "Inspect one project asset by stable AssetId, including source import metadata and file state."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "required": ["asset_id"],
                    "properties": {
                        "asset_id": {"type": "string"}
                    },
                    "additionalProperties": false
                }),
            },
        ]
    }

    /// Searches project assets through the shared asset catalog service.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when shared read permission is denied or the
    /// manifest contains malformed imported asset identity.
    pub fn asset_search(
        &self,
        project: &ProjectRoot,
        manifest: &AssetManifest,
        permissions: &AuthoringPermissions,
        input: AssetSearchInput,
    ) -> Result<AssetCatalogSearch, McpToolError> {
        self.service
            .search(project, manifest, permissions, &input.query)
            .map_err(McpToolError::from)
    }

    /// Inspects one stable project asset through the shared catalog service.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when shared read permission is denied, the
    /// asset is unknown, or its source path violates the project boundary.
    pub fn asset_inspect(
        &self,
        project: &ProjectRoot,
        manifest: &AssetManifest,
        permissions: &AuthoringPermissions,
        input: AssetInspectInput,
    ) -> Result<AssetInspection, McpToolError> {
        self.service
            .inspect(project, manifest, permissions, &input.asset_id)
            .map_err(McpToolError::from)
    }
}

impl Default for AssetMcpTools {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_assets::asset::{ImportSettings, ManifestEntry};
    use engine_authoring::{ProjectConfig, PROJECT_SCHEMA_VERSION};
    use std::fs;
    use std::path::PathBuf;

    fn project() -> (PathBuf, ProjectRoot) {
        let path = std::env::temp_dir().join(format!(
            "gameengine-mcp-asset-{}",
            AssetId::generate().as_str()
        ));
        fs::create_dir_all(&path).expect("temporary project directory");
        let root = ProjectRoot::create(
            &path,
            ProjectConfig {
                name: "McpAssetTest".into(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project fixture");
        (path, root)
    }

    #[test]
    fn descriptors_expose_asset_search_and_inspect() {
        let names = AssetMcpTools::new()
            .tool_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["asset.search", "asset.inspect"]);
    }

    #[test]
    fn mcp_search_matches_shared_catalog_result() {
        let (path, root) = project();
        let id = AssetId::generate();
        let mut manifest = AssetManifest::default();
        manifest.insert(
            id,
            ManifestEntry {
                path: "textures/hero.png".into(),
                name: Some("hero_texture".into()),
                import_settings: ImportSettings::default(),
            },
        );
        let permissions = AuthoringPermissions::read_only();
        let direct = AssetCatalogService::new()
            .search(&root, &manifest, &permissions, "hero")
            .expect("direct catalog query");
        let mcp = AssetMcpTools::new()
            .asset_search(
                &root,
                &manifest,
                &permissions,
                AssetSearchInput {
                    query: "hero".into(),
                },
            )
            .expect("MCP catalog query");

        assert_eq!(mcp, direct);
        fs::remove_dir_all(path).expect("temporary project cleanup");
    }
}
