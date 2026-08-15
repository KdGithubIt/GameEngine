//! Humanoid import catalog derived from canonical model Native clips (ADR 0110).
//!
//! The canonical glTF/model import result remains the owner of source-bound Native
//! animation data. This module derives model-owned Humanoid profiles and portable
//! HumanoidMotion variants without making either one runtime playback state.

use crate::asset::{
    imported_humanoid_motion_sub_asset_id, HumanoidProfile, ImportedSubAsset,
    ImportedSubAssetKind,
};
use crate::model_import::GltfImportResult;
use engine_animation::humanoid::detect_humanoid_profile;
use engine_animation::humanoid_motion::{build_humanoid_motion, HumanoidMotion};
use engine_authoring::diagnostic::Diagnostic;
use engine_authoring::id::AssetId;
use hashbrown::{HashMap, HashSet};

/// One skeleton-independent Humanoid variant derived from a Native animation.
#[derive(Clone)]
pub struct GltfHumanoidMotionData {
    /// Original animation selector shared with the Native variant.
    pub source_index: usize,
    /// Stable ID nested under the Native clip stable ID.
    pub id: AssetId,
    /// Human-readable logical animation name.
    pub name: String,
    /// Index into the source model's skin table.
    pub skin_index: usize,
    /// Portable skeleton-independent motion.
    pub motion: HumanoidMotion,
}

/// Derived Humanoid authoring data available from one imported model.
#[derive(Clone, Default)]
pub struct HumanoidImportCatalog {
    /// One valid model-owned profile per resolved skeleton identity.
    pub profiles: Vec<HumanoidProfile>,
    /// Portable motion variants whose source skeleton has a valid profile.
    pub motions: Vec<GltfHumanoidMotionData>,
    /// Non-blocking mapping or conversion diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Builds model-owned Humanoid profiles and portable motion variants.
///
/// Detection is conservative and happens only during import/authoring. Native
/// clips remain authoritative and usable when a profile or conversion is
/// unavailable.
pub fn build_humanoid_import_catalog(imported: &GltfImportResult) -> HumanoidImportCatalog {
    let mut profiles = Vec::new();
    let mut diagnostics = Vec::new();
    let mut seen_skeletons = HashSet::<AssetId>::new();

    for skin in &imported.skins {
        if !seen_skeletons.insert(skin.skeleton.id.clone()) {
            continue;
        }
        let mut detection = detect_humanoid_profile(&skin.skeleton);
        diagnostics.append(&mut detection.diagnostics);
        if let Some(profile) = detection.profile {
            profiles.push(profile);
        }
    }

    let profiles_by_skeleton = profiles
        .iter()
        .enumerate()
        .map(|(index, profile)| (profile.skeleton.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut motions = Vec::new();

    for animation in &imported.animations {
        let Some(skin) = imported.skins.get(animation.skin_index) else {
            continue;
        };
        let Some(&profile_index) = profiles_by_skeleton.get(skin.skeleton.id.as_str()) else {
            continue;
        };
        let profile = &profiles[profile_index];
        match build_humanoid_motion(&animation.clip, &skin.skeleton, profile) {
            Ok(mut built) => {
                diagnostics.append(&mut built.diagnostics);
                motions.push(GltfHumanoidMotionData {
                    source_index: animation.source_index,
                    id: imported_humanoid_motion_sub_asset_id(&animation.id),
                    name: animation.name.clone(),
                    skin_index: animation.skin_index,
                    motion: built.motion,
                });
            }
            Err(error) => diagnostics.push(Diagnostic::warning(
                "anim.humanoid_conversion_unavailable",
                format!(
                    "animation `{}` keeps its Native variant but has no Humanoid variant: {error}",
                    animation.name
                ),
            )),
        }
    }
    motions.sort_by_key(|motion| motion.source_index);

    HumanoidImportCatalog {
        profiles,
        motions,
        diagnostics,
    }
}

/// Builds stable imported-sub-asset catalog entries for portable Humanoid variants.
pub fn humanoid_imported_sub_assets(catalog: &HumanoidImportCatalog) -> Vec<ImportedSubAsset> {
    catalog
        .motions
        .iter()
        .map(|motion| ImportedSubAsset {
            id: motion.id.as_str().to_owned(),
            kind: ImportedSubAssetKind::HumanoidMotion,
            name: format!("{} Humanoid", motion.name),
            index: motion.source_index as u32,
            target_model_source: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanoid_sub_asset_ids_are_nested_under_native_ids() {
        let native = AssetId::generate();
        let catalog = HumanoidImportCatalog {
            profiles: Vec::new(),
            motions: vec![GltfHumanoidMotionData {
                source_index: 3,
                id: imported_humanoid_motion_sub_asset_id(&native),
                name: "Walk".to_owned(),
                skin_index: 0,
                motion: HumanoidMotion {
                    duration: 1.0,
                    rotations: Vec::new(),
                    root_motion: None,
                    events: Vec::new(),
                    source_skeleton_identity: 1,
                },
            }],
            diagnostics: Vec::new(),
        };
        let sub_assets = humanoid_imported_sub_assets(&catalog);
        assert_eq!(sub_assets.len(), 1);
        assert_eq!(
            sub_assets[0].id,
            imported_humanoid_motion_sub_asset_id(&native).as_str()
        );
        assert_eq!(sub_assets[0].kind, ImportedSubAssetKind::HumanoidMotion);
    }
}
