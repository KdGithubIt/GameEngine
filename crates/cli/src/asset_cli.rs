use super::{input_error_json, to_json, CliError, CliRunResult};
use engine_assets::asset::AssetManifest;
use engine_assets::catalog::{AssetCatalogError, AssetCatalogService};
use engine_authoring::{
    AssetId, AuthoringPermissions, ProjectRoot, StableId,
};
use std::fs;
use std::path::Path;

pub(super) fn dispatch(args: &[String]) -> Option<Result<CliRunResult, CliError>> {
    match args {
        [domain, command, project] if domain == "asset" && command == "search" => {
            Some(asset_search(Path::new(project), ""))
        }
        [domain, command, project, query] if domain == "asset" && command == "search" => {
            Some(asset_search(Path::new(project), query))
        }
        [domain, command, project, asset_id] if domain == "asset" && command == "inspect" => {
            Some(asset_inspect(Path::new(project), asset_id))
        }
        _ => None,
    }
}

fn asset_search(project_path: &Path, query: &str) -> Result<CliRunResult, CliError> {
    let (project, manifest) = match load_catalog(project_path) {
        Ok(catalog) => catalog,
        Err(result) => return Ok(result),
    };
    match AssetCatalogService::new().search(
        &project,
        &manifest,
        &AuthoringPermissions::read_only(),
        query,
    ) {
        Ok(output) => Ok(CliRunResult::success(to_json(&output)?)),
        Err(error) => catalog_error(project_path, error),
    }
}

fn asset_inspect(project_path: &Path, asset_id: &str) -> Result<CliRunResult, CliError> {
    let asset_id = match AssetId::from_stable_id(StableId::new(asset_id)) {
        Ok(asset_id) => asset_id,
        Err(error) => {
            return Ok(input_error(
                "invalid_asset_id",
                Path::new(asset_id),
                &error.to_string(),
            ));
        }
    };
    let (project, manifest) = match load_catalog(project_path) {
        Ok(catalog) => catalog,
        Err(result) => return Ok(result),
    };
    match AssetCatalogService::new().inspect(
        &project,
        &manifest,
        &AuthoringPermissions::read_only(),
        &asset_id,
    ) {
        Ok(output) => Ok(CliRunResult::success(to_json(&output)?)),
        Err(error) => catalog_error(project_path, error),
    }
}

fn load_catalog(project_path: &Path) -> Result<(ProjectRoot, AssetManifest), CliRunResult> {
    let project = ProjectRoot::open(project_path).map_err(|error| {
        input_error("project_error", project_path, &error.to_string())
    })?;
    let manifest_path = project.path().join("asset_manifest.json");
    let json = match fs::read_to_string(&manifest_path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((project, AssetManifest::default()));
        }
        Err(error) => {
            return Err(input_error(
                "io_error",
                &manifest_path,
                &error.to_string(),
            ));
        }
    };
    let manifest = AssetManifest::from_json(&json).map_err(|error| {
        input_error("invalid_asset_manifest", &manifest_path, &error.to_string())
    })?;
    Ok((project, manifest))
}

fn catalog_error(path: &Path, error: AssetCatalogError) -> Result<CliRunResult, CliError> {
    Ok(input_error(error.code(), path, &error.to_string()))
}

fn input_error(kind: &str, path: &Path, message: &str) -> CliRunResult {
    CliRunResult::input_error(input_error_json(kind, path, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_assets::asset::{ImportSettings, ManifestEntry};
    use engine_authoring::{ProjectConfig, PROJECT_SCHEMA_VERSION};
    use serde_json::Value;
    use std::path::PathBuf;

    fn project() -> (PathBuf, ProjectRoot) {
        let path = std::env::temp_dir().join(format!(
            "gameengine-cli-asset-{}",
            AssetId::generate().as_str()
        ));
        fs::create_dir_all(&path).expect("temporary project directory");
        let root = ProjectRoot::create(
            &path,
            ProjectConfig {
                name: "CliAssetTest".into(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project fixture");
        (path, root)
    }

    fn write_manifest(root: &ProjectRoot, manifest: &AssetManifest) {
        let json = manifest
            .to_canonical_json()
            .expect("manifest fixture serialization");
        fs::write(root.path().join("asset_manifest.json"), json)
            .expect("manifest fixture write");
    }

    #[test]
    fn asset_search_uses_shared_catalog_output() {
        let (path, root) = project();
        let id = AssetId::generate();
        let mut manifest = AssetManifest::default();
        manifest.insert(
            id.clone(),
            ManifestEntry {
                path: "textures/hero.png".into(),
                name: Some("hero_texture".into()),
                import_settings: ImportSettings::default(),
            },
        );
        write_manifest(&root, &manifest);
        let direct = AssetCatalogService::new()
            .search(
                &root,
                &manifest,
                &AuthoringPermissions::read_only(),
                "hero",
            )
            .expect("direct catalog query");

        let output = crate::run_cli([
            "asset",
            "search",
            path.to_str().expect("UTF-8 test path"),
            "hero",
        ])
        .expect("asset search CLI");
        let cli: Value = serde_json::from_str(&output).expect("CLI JSON");
        let direct = serde_json::to_value(direct).expect("direct JSON");

        assert_eq!(cli, direct);
        assert_eq!(cli["assets"][0]["id"], id.as_str());
        fs::remove_dir_all(path).expect("temporary project cleanup");
    }

    #[test]
    fn asset_inspect_rejects_invalid_stable_id_as_input_error() {
        let (path, _root) = project();
        let result = crate::run_cli_with_status([
            "asset",
            "inspect",
            path.to_str().expect("UTF-8 test path"),
            "asset_not-valid",
        ])
        .expect("invalid ID must be structured CLI output");

        assert_eq!(result.exit_code, 2);
        let output: Value = serde_json::from_str(&result.output).expect("error JSON");
        assert_eq!(output["error"]["kind"], "invalid_asset_id");
        fs::remove_dir_all(path).expect("temporary project cleanup");
    }

    #[test]
    fn missing_manifest_is_an_empty_catalog() {
        let (path, _root) = project();
        let output = crate::run_cli([
            "asset",
            "search",
            path.to_str().expect("UTF-8 test path"),
        ])
        .expect("missing manifest must behave as empty catalog");
        let output: Value = serde_json::from_str(&output).expect("catalog JSON");

        assert_eq!(output["assets"].as_array().map(Vec::len), Some(0));
        fs::remove_dir_all(path).expect("temporary project cleanup");
    }
}
