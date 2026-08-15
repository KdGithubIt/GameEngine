//! Asset manifest I/O and the shared asset path and name helpers.
//!
//! Every submodule that mutates the manifest reads and writes it through this
//! module so the on-disk file is produced by exactly one code path.

use crate::ui::*;

impl EditorApp {
    pub(super) fn manifest_entry_for(&self, relative: &Path) -> Option<(AssetId, String)> {
        let relative = relative.to_string_lossy().replace('\\', "/");
        self.asset_manifest
            .iter()
            .find(|(_, entry)| entry.path == relative)
            .map(|(id, entry)| (id.clone(), entry.path.clone()))
    }

}

fn asset_manifest_path(project_root: &ProjectRoot) -> PathBuf {
    project_root.path().join("asset_manifest.json")
}

pub(super) fn save_asset_manifest(
    project_root: &ProjectRoot,
    manifest: &engine::AssetManifest,
) -> Result<(), String> {
    let json = manifest
        .to_canonical_json()
        .map_err(|error| format!("failed to serialize asset manifest: {error}"))?;
    let path = asset_manifest_path(project_root);
    replace_file_contents(&path, &json)
        .map_err(|error| format!("failed to save {}: {error}", path.display()))
}

pub(in crate::ui) fn load_asset_manifest(
    project_root: &ProjectRoot,
) -> (engine::AssetManifest, Option<engine_authoring::Diagnostic>) {
    let path = asset_manifest_path(project_root);
    let json = match fs::read_to_string(&path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (engine::AssetManifest::default(), None);
        }
        Err(error) => {
            return (
                engine::AssetManifest::default(),
                Some(engine_authoring::Diagnostic::error(
                    "editor.asset_manifest_load_failed",
                    format!("failed to read {}: {error}", path.display()),
                )),
            );
        }
    };

    match engine::AssetManifest::from_json(&json) {
        Ok(manifest) => (manifest, None),
        Err(error) => (
            engine::AssetManifest::default(),
            Some(engine_authoring::Diagnostic::error(
                "editor.asset_manifest_load_failed",
                format!("failed to parse {}: {error}", path.display()),
            )),
        ),
    }
}

/// Returns `true` for a `*.ui.json` declarative UI document path (Phase 54).
///
/// This test-only compatibility helper checks the same shared category filter
/// used by registration and Inspector pickers.
#[cfg(test)]
pub(in crate::ui) fn is_registerable_ui_document(path: &Path) -> bool {
    manifest_path_matches_asset_kind(engine::AssetKind::UiDocument, path, None)
}

pub(in crate::ui) fn asset_relative_path_string(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let std::path::Component::Normal(part) = component else {
            return None;
        };
        parts.push(part.to_str()?);
    }
    Some(parts.join("/"))
}

pub(super) fn normalize_manifest_path(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

/// Returns whether `path` names a source the background importer can catalog:
/// a model document or a `.vmd` motion (ADR 0097 §3).
pub(in crate::ui) fn is_importable_source_path(path: &Path) -> bool {
    engine::asset_path_matches_kind(engine::AssetKind::GltfSource, path)
        || engine::asset_path_matches_kind(engine::AssetKind::MotionSource, path)
}

/// Returns the registered `.pmx` model source when the project holds exactly
/// one, so a newly registered motion can be paired without asking.
///
/// Deliberately `None` for zero or several: pairing a motion with the wrong
/// rig bakes a clip that looks plausible and is wrong (ADR 0097 §3's
/// `vmd.rest_pose_mismatch` is a warning, not a hard stop), so the ambiguous
/// case belongs to the author.
pub(in crate::ui) fn sole_pmx_model_source(manifest: &engine::AssetManifest) -> Option<engine_authoring::AssetId> {
    let mut models = manifest.iter().filter(|(_, entry)| {
        Path::new(&entry.path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
    });
    let (id, _) = models.next()?;
    models.next().is_none().then(|| id.clone())
}

pub(in crate::ui) fn unique_asset_name(display_name: &str, manifest: &engine::AssetManifest) -> String {
    let base = asset_name_slug(display_name);
    if !manifest
        .iter()
        .any(|(_, entry)| entry.name.as_deref() == Some(base.as_str()))
    {
        return base;
    }

    for suffix in 2.. {
        let candidate = format!("{base}_{suffix}");
        if !manifest
            .iter()
            .any(|(_, entry)| entry.name.as_deref() == Some(candidate.as_str()))
        {
            return candidate;
        }
    }
    unreachable!("usize suffix space must not be exhausted")
}

pub(in crate::ui) fn asset_name_slug(display_name: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_separator = false;
    for character in display_name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator && !slug.is_empty() {
            slug.push('_');
            previous_was_separator = true;
        }
    }
    while slug.ends_with('_') {
        slug.pop();
    }
    if slug.is_empty() {
        "asset".into()
    } else {
        slug
    }
}

pub(in crate::ui) fn is_registerable_asset(path: &Path) -> bool {
    crate::asset_management::is_registerable_asset_path(path)
}
