//! Humanoid profile authoring controls for model import settings (ADR 0110).

use crate::ui::*;

/// Editable Humanoid profile draft owned by one imported skeleton.
#[derive(Clone)]
pub(super) struct HumanoidProfileEditorState {
    profile: engine::asset::HumanoidProfile,
    enabled: bool,
    skeleton_name: String,
    current_skeleton_identity: Option<u64>,
    bone_choices: Vec<engine::asset::SkeletonBoneRecord>,
}

/// Builds editable Humanoid drafts from persisted import metadata.
///
/// A skeleton with no detected profile still receives a disabled draft so the
/// author can explicitly configure it. Existing orphaned authored profiles are
/// retained so reimport cannot silently erase manual work.
pub(super) fn humanoid_profile_editor_states(
    settings: &engine::asset::ImportSettings,
) -> Vec<HumanoidProfileEditorState> {
    let mut states = Vec::new();

    for record in &settings.skeleton_records {
        let existing = settings
            .humanoid_profiles
            .iter()
            .find(|profile| profile.skeleton == record.id)
            .cloned();
        let skeleton_name = settings
            .sub_assets
            .iter()
            .find(|sub_asset| {
                sub_asset.kind == engine::ImportedSubAssetKind::Skeleton
                    && sub_asset.id == record.id
            })
            .map(|sub_asset| sub_asset.name.clone())
            .unwrap_or_else(|| record.id.clone());
        let (profile, enabled) = match existing {
            Some(profile) => (profile, true),
            None => (
                engine::asset::HumanoidProfile {
                    skeleton: record.id.clone(),
                    skeleton_identity: record.identity,
                    bones: std::collections::BTreeMap::new(),
                    motion_root: None,
                    uncertain_bones: Vec::new(),
                    origin: engine::asset::HumanoidProfileOrigin::Authored,
                },
                false,
            ),
        };
        let mut bone_choices = record.bones.clone();
        bone_choices.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.bone_id.cmp(&right.bone_id))
        });
        states.push(HumanoidProfileEditorState {
            profile,
            enabled,
            skeleton_name,
            current_skeleton_identity: Some(record.identity),
            bone_choices,
        });
    }

    for profile in &settings.humanoid_profiles {
        if settings
            .skeleton_records
            .iter()
            .any(|record| record.id == profile.skeleton)
        {
            continue;
        }
        states.push(HumanoidProfileEditorState {
            profile: profile.clone(),
            enabled: true,
            skeleton_name: profile.skeleton.clone(),
            current_skeleton_identity: None,
            bone_choices: Vec::new(),
        });
    }

    states
}

/// Returns exactly the enabled profiles that should be persisted before reimport.
pub(super) fn persisted_humanoid_profiles(
    states: &[HumanoidProfileEditorState],
) -> Vec<engine::asset::HumanoidProfile> {
    states
        .iter()
        .filter(|state| state.enabled)
        .map(|state| state.profile.clone())
        .collect()
}

/// Draws the manual Humanoid mapping section inside Import Settings.
pub(super) fn show_humanoid_profiles_editor(
    ui: &mut egui::Ui,
    profiles: &mut [HumanoidProfileEditorState],
) {
    ui.collapsing("Configure Humanoid", |ui| {
        ui.small(
            "Mappings use stable BoneIds. Editing any field marks the profile Authored, so \
             reimport will not silently replace it.",
        );

        for profile in profiles {
            let title = format!("{} ({})", profile.skeleton_name, profile.profile.skeleton);
            egui::CollapsingHeader::new(title)
                .default_open(profile.enabled)
                .show(ui, |ui| {
                    if !profile.enabled {
                        ui.label(
                            "Automatic detection did not produce a persisted Humanoid profile for \
                             this skeleton.",
                        );
                        if ui.button("Configure Humanoid").clicked() {
                            mark_humanoid_profile_authored(profile);
                        }
                        return;
                    }

                    ui.label(format!("Origin: {:?}", profile.profile.origin));
                    match profile.current_skeleton_identity {
                        Some(identity) if identity != profile.profile.skeleton_identity => {
                            ui.label(
                                "This authored profile is stale for the current skeleton. Repair a \
                                 mapping below to confirm it against the current skeleton identity.",
                            );
                        }
                        None => {
                            ui.label(
                                "This authored profile belongs to a skeleton no longer imported by \
                                 this source. It is retained for manual repair.",
                            );
                        }
                        _ => {}
                    }

                    let missing_required = engine::asset::HumanoidBone::REQUIRED
                        .iter()
                        .filter(|semantic| !profile.profile.bones.contains_key(semantic))
                        .count();
                    if missing_required != 0 {
                        ui.label(format!(
                            "{missing_required} required Humanoid semantics are still unmapped."
                        ));
                    }
                    if !profile.profile.uncertain_bones.is_empty() {
                        ui.label(format!(
                            "Uncertain automatic mappings: {:?}",
                            profile.profile.uncertain_bones
                        ));
                    }

                    egui::ScrollArea::vertical()
                        .id_salt(("humanoid-profile", profile.profile.skeleton.as_str()))
                        .max_height(360.0)
                        .show(ui, |ui| {
                            egui::Grid::new((
                                "humanoid-profile-grid",
                                profile.profile.skeleton.as_str(),
                            ))
                            .striped(true)
                            .show(ui, |ui| {
                                for semantic in engine::asset::HumanoidBone::ALL {
                                    ui.label(format!("{semantic:?}"));
                                    let before = profile.profile.bone_id(semantic);
                                    let mut selected = before;
                                    egui::ComboBox::from_id_salt((
                                        "humanoid-bone",
                                        profile.profile.skeleton.as_str(),
                                        semantic,
                                    ))
                                    .selected_text(humanoid_bone_choice_label(
                                        selected,
                                        &profile.bone_choices,
                                    ))
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut selected,
                                            None,
                                            "(Unmapped)",
                                        );
                                        for choice in &profile.bone_choices {
                                            ui.selectable_value(
                                                &mut selected,
                                                Some(choice.bone_id),
                                                format!(
                                                    "{} (BoneId {})",
                                                    choice.name, choice.bone_id
                                                ),
                                            );
                                        }
                                    });
                                    if selected != before {
                                        set_humanoid_bone_mapping(profile, semantic, selected);
                                    }
                                    ui.end_row();
                                }

                                ui.label("Motion Root");
                                let before = profile.profile.motion_root;
                                let mut selected = before;
                                egui::ComboBox::from_id_salt((
                                    "humanoid-motion-root",
                                    profile.profile.skeleton.as_str(),
                                ))
                                .selected_text(humanoid_bone_choice_label(
                                    selected,
                                    &profile.bone_choices,
                                ))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut selected, None, "(None)");
                                    for choice in &profile.bone_choices {
                                        ui.selectable_value(
                                            &mut selected,
                                            Some(choice.bone_id),
                                            format!(
                                                "{} (BoneId {})",
                                                choice.name, choice.bone_id
                                            ),
                                        );
                                    }
                                });
                                if selected != before {
                                    set_humanoid_motion_root(profile, selected);
                                }
                                ui.end_row();
                            });
                        });
                });
        }
    });
}

fn mark_humanoid_profile_authored(state: &mut HumanoidProfileEditorState) {
    state.enabled = true;
    state.profile.origin = engine::asset::HumanoidProfileOrigin::Authored;
    if let Some(identity) = state.current_skeleton_identity {
        state.profile.skeleton_identity = identity;
    }
}

fn set_humanoid_bone_mapping(
    state: &mut HumanoidProfileEditorState,
    semantic: engine::asset::HumanoidBone,
    bone_id: Option<u32>,
) {
    match bone_id {
        Some(bone_id) => {
            state.profile.bones.insert(semantic, bone_id);
        }
        None => {
            state.profile.bones.remove(&semantic);
        }
    }
    state
        .profile
        .uncertain_bones
        .retain(|uncertain| *uncertain != semantic);
    mark_humanoid_profile_authored(state);
}

fn set_humanoid_motion_root(state: &mut HumanoidProfileEditorState, bone_id: Option<u32>) {
    state.profile.motion_root = bone_id;
    mark_humanoid_profile_authored(state);
}

fn humanoid_bone_choice_label(
    selected: Option<u32>,
    choices: &[engine::asset::SkeletonBoneRecord],
) -> String {
    let Some(selected) = selected else {
        return "(Unmapped)".to_owned();
    };
    choices
        .iter()
        .find(|choice| choice.bone_id == selected)
        .map(|choice| format!("{} (BoneId {})", choice.name, choice.bone_id))
        .unwrap_or_else(|| format!("Missing BoneId {selected}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skeleton_settings() -> engine::asset::ImportSettings {
        let source = AssetId::generate();
        let skeleton = engine::asset::imported_sub_asset_id(
            &source,
            engine::ImportedSubAssetKind::Skeleton,
            0,
        );
        engine::asset::ImportSettings {
            skeleton_records: vec![engine::asset::SkeletonRecord {
                id: skeleton.as_str().to_owned(),
                identity: 42,
                next_bone_id: 3,
                bones: vec![
                    engine::asset::SkeletonBoneRecord {
                        bone_id: 1,
                        name: "Hips".to_owned(),
                    },
                    engine::asset::SkeletonBoneRecord {
                        bone_id: 2,
                        name: "Head".to_owned(),
                    },
                ],
            }],
            sub_assets: vec![engine::asset::ImportedSubAsset {
                id: skeleton.as_str().to_owned(),
                kind: engine::ImportedSubAssetKind::Skeleton,
                name: "Character".to_owned(),
                index: 0,
                target_model_source: None,
            }],
            ..engine::asset::ImportSettings::default()
        }
    }

    #[test]
    fn configure_humanoid_can_author_a_profile_when_detection_created_none() {
        let settings = skeleton_settings();
        let mut states = humanoid_profile_editor_states(&settings);
        assert_eq!(states.len(), 1);
        assert!(!states[0].enabled);

        set_humanoid_bone_mapping(
            &mut states[0],
            engine::asset::HumanoidBone::Hips,
            Some(1),
        );
        set_humanoid_motion_root(&mut states[0], Some(1));

        let persisted = persisted_humanoid_profiles(&states);
        assert_eq!(persisted.len(), 1);
        assert_eq!(
            persisted[0].origin,
            engine::asset::HumanoidProfileOrigin::Authored
        );
        assert_eq!(persisted[0].skeleton_identity, 42);
        assert_eq!(
            persisted[0].bone_id(engine::asset::HumanoidBone::Hips),
            Some(1)
        );
        assert_eq!(persisted[0].motion_root, Some(1));
    }

    #[test]
    fn editing_automatic_mapping_marks_it_authored_and_clears_uncertainty() {
        let mut settings = skeleton_settings();
        let skeleton = settings.skeleton_records[0].id.clone();
        settings.humanoid_profiles.push(engine::asset::HumanoidProfile {
            skeleton,
            skeleton_identity: 42,
            bones: std::collections::BTreeMap::from([(
                engine::asset::HumanoidBone::Hips,
                1,
            )]),
            motion_root: None,
            uncertain_bones: vec![engine::asset::HumanoidBone::Hips],
            origin: engine::asset::HumanoidProfileOrigin::Automatic,
        });

        let mut states = humanoid_profile_editor_states(&settings);
        assert!(states[0].enabled);
        assert_eq!(
            states[0].profile.origin,
            engine::asset::HumanoidProfileOrigin::Automatic
        );

        set_humanoid_bone_mapping(
            &mut states[0],
            engine::asset::HumanoidBone::Hips,
            Some(2),
        );

        assert_eq!(
            states[0].profile.origin,
            engine::asset::HumanoidProfileOrigin::Authored
        );
        assert!(states[0].profile.uncertain_bones.is_empty());
        assert_eq!(
            states[0].profile.bone_id(engine::asset::HumanoidBone::Hips),
            Some(2)
        );
    }
}
