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
use engine_animation::humanoid::{detect_humanoid_profile, validate_humanoid_profile};
use engine_animation::humanoid_motion::{build_humanoid_motion, HumanoidMotion};
use engine_assets::asset::HumanoidProfileOrigin;
use engine_authoring::diagnostic::Diagnostic;
use engine_authoring::id::AssetId;
use engine_rig::skeleton_asset::SkeletonAsset;
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

enum ReconciledProfile {
    Usable(HumanoidProfile),
    PreservedStaleAuthored(HumanoidProfile),
    Unavailable,
}

fn reconcile_profile(
    skeleton: &SkeletonAsset,
    existing: Option<&HumanoidProfile>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ReconciledProfile {
    if let Some(profile) = existing {
        match validate_humanoid_profile(profile, skeleton) {
            Ok(()) => {
                if !profile.uncertain_bones.is_empty() {
                    diagnostics.push(Diagnostic::warning(
                        "anim.humanoid_profile_uncertain",
                        format!(
                            "skeleton `{}` keeps a structurally valid Humanoid profile with uncertain mappings: {:?}",
                            skeleton.name, profile.uncertain_bones
                        ),
                    ));
                }
                return ReconciledProfile::Usable(profile.clone());
            }
            Err(error) if profile.origin == HumanoidProfileOrigin::Authored => {
                diagnostics.push(Diagnostic::warning(
                    "anim.humanoid_profile_stale_authored",
                    format!(
                        "authored Humanoid profile for skeleton `{}` is stale and was preserved instead of replaced: {error}",
                        skeleton.name
                    ),
                ));
                return ReconciledProfile::PreservedStaleAuthored(profile.clone());
            }
            Err(_) => {
                // Automatic profiles are generated metadata. A structural
                // change invalidates them, so re-detection is the correct
                // source of truth rather than preserving stale generated IDs.
            }
        }
    }

    let mut detection = detect_humanoid_profile(skeleton);
    diagnostics.append(&mut detection.diagnostics);
    detection
        .profile
        .map(ReconciledProfile::Usable)
        .unwrap_or(ReconciledProfile::Unavailable)
}

/// Builds model-owned Humanoid profiles and portable motion variants.
///
/// Detection is conservative and happens only during import/authoring. Native
/// clips remain authoritative and usable when a profile or conversion is
/// unavailable. Existing authored profiles are never silently replaced during
/// reimport: structurally valid mappings are reused, while stale authored
/// mappings are preserved with a diagnostic and excluded from Humanoid bake.
pub fn build_humanoid_import_catalog(
    imported: &GltfImportResult,
    existing_profiles: &[HumanoidProfile],
) -> HumanoidImportCatalog {
    let mut profiles = Vec::new();
    let mut diagnostics = Vec::new();
    let mut seen_skeletons = HashSet::<AssetId>::new();
    let mut consumed_existing = HashSet::<usize>::new();
    let mut usable_profiles = HashMap::<AssetId, usize>::new();

    for skin in &imported.skins {
        if !seen_skeletons.insert(skin.skeleton.id.clone()) {
            continue;
        }
        let existing = existing_profiles
            .iter()
            .enumerate()
            .find(|(_, profile)| profile.skeleton == skin.skeleton.id.as_str());
        if let Some((index, _)) = existing {
            consumed_existing.insert(index);
        }

        match reconcile_profile(
            &skin.skeleton,
            existing.map(|(_, profile)| profile),
            &mut diagnostics,
        ) {
            ReconciledProfile::Usable(profile) => {
                let profile_index = profiles.len();
                profiles.push(profile);
                usable_profiles.insert(skin.skeleton.id.clone(), profile_index);
            }
            ReconciledProfile::PreservedStaleAuthored(profile) => profiles.push(profile),
            ReconciledProfile::Unavailable => {}
        }
    }

    for (index, profile) in existing_profiles.iter().enumerate() {
        if consumed_existing.contains(&index) || profile.origin != HumanoidProfileOrigin::Authored {
            continue;
        }
        diagnostics.push(Diagnostic::warning(
            "anim.humanoid_profile_orphaned_authored",
            format!(
                "authored Humanoid profile for skeleton `{}` no longer matches a skeleton imported by this model; the mapping was retained for manual repair",
                profile.skeleton
            ),
        ));
        profiles.push(profile.clone());
    }

    let mut motions = Vec::new();

    for animation in &imported.animations {
        let Some(skin) = imported.skins.get(animation.skin_index) else {
            continue;
        };
        let Some(&profile_index) = usable_profiles.get(&skin.skeleton.id) else {
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
