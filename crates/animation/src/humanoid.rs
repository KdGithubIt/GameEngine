//! Humanoid profile validation and import-time detection (ADR 0110).
//!
//! Profiles are model-owned mappings from portable humanoid semantics to the
//! stable [`BoneId`] values of one imported [`SkeletonAsset`]. Name matching is
//! used only to propose the first mapping; persisted and runtime-facing data use
//! BoneIds exclusively.

use crate::skeleton_asset::{BoneId, SkeletonAsset};
use engine_assets::asset::{HumanoidBone, HumanoidProfile, HumanoidProfileOrigin};
use engine_authoring::diagnostic::Diagnostic;
use hashbrown::HashMap;
use std::collections::BTreeMap;
use std::fmt;

/// Why a humanoid profile cannot be used safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HumanoidError {
    /// Profile belongs to another skeleton asset.
    SkeletonMismatch,
    /// Profile was validated against an older skeleton identity.
    SkeletonIdentityMismatch,
    /// A required semantic has no mapping.
    MissingRequired(HumanoidBone),
    /// A mapped stable BoneId is absent from the skeleton.
    MissingBone {
        /// Semantic whose mapping is invalid.
        semantic: HumanoidBone,
        /// Missing stable BoneId payload.
        bone: u32,
    },
    /// Two semantics map to one concrete bone.
    DuplicateBone {
        /// Earlier semantic using the bone.
        first: HumanoidBone,
        /// Later semantic using the same bone.
        second: HumanoidBone,
        /// Duplicated stable BoneId payload.
        bone: u32,
    },
    /// A required body chain violates ancestry order.
    InvalidHierarchy {
        /// Semantic that must be above the descendant.
        ancestor: HumanoidBone,
        /// Semantic that must descend from the ancestor.
        descendant: HumanoidBone,
    },
}

impl fmt::Display for HumanoidError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SkeletonMismatch => formatter.write_str("humanoid profile targets another skeleton"),
            Self::SkeletonIdentityMismatch => formatter.write_str(
                "humanoid profile is stale for the current skeleton identity",
            ),
            Self::MissingRequired(bone) => {
                write!(formatter, "missing required humanoid semantic {bone:?}")
            }
            Self::MissingBone { semantic, bone } => write!(
                formatter,
                "humanoid semantic {semantic:?} refers to missing BoneId({bone})"
            ),
            Self::DuplicateBone {
                first,
                second,
                bone,
            } => write!(
                formatter,
                "humanoid semantics {first:?} and {second:?} both refer to BoneId({bone})"
            ),
            Self::InvalidHierarchy {
                ancestor,
                descendant,
            } => write!(
                formatter,
                "humanoid semantic {ancestor:?} must be an ancestor of {descendant:?}"
            ),
        }
    }
}

impl std::error::Error for HumanoidError {}

/// Result of conservative import-time humanoid detection.
#[derive(Debug, Clone)]
pub struct HumanoidProfileDetection {
    /// Valid profile when all required semantics resolved and passed structural checks.
    pub profile: Option<HumanoidProfile>,
    /// Diagnostics explaining ambiguity or why automatic conversion is unavailable.
    pub diagnostics: Vec<Diagnostic>,
}

/// Validates a persisted model profile against its concrete skeleton.
///
/// Chains are checked by ancestry rather than direct adjacency so helper and
/// twist bones may remain between mapped semantics.
pub fn validate_humanoid_profile(
    profile: &HumanoidProfile,
    skeleton: &SkeletonAsset,
) -> Result<(), HumanoidError> {
    if profile.skeleton != skeleton.id.as_str() {
        return Err(HumanoidError::SkeletonMismatch);
    }
    if profile.skeleton_identity != skeleton.identity.0 {
        return Err(HumanoidError::SkeletonIdentityMismatch);
    }
    for semantic in HumanoidBone::REQUIRED {
        if !profile.bones.contains_key(&semantic) {
            return Err(HumanoidError::MissingRequired(semantic));
        }
    }

    let mut assigned = HashMap::<u32, HumanoidBone>::new();
    for (&semantic, &bone) in &profile.bones {
        if skeleton.bone_index(BoneId(bone)).is_none() {
            return Err(HumanoidError::MissingBone { semantic, bone });
        }
        if let Some(first) = assigned.insert(bone, semantic) {
            return Err(HumanoidError::DuplicateBone {
                first,
                second: semantic,
                bone,
            });
        }
    }
    if let Some(root) = profile.motion_root
        && skeleton.bone_index(BoneId(root)).is_none()
    {
        return Err(HumanoidError::MissingBone {
            semantic: HumanoidBone::Hips,
            bone: root,
        });
    }

    for (ancestor, descendant) in required_ancestry_pairs() {
        if !is_ancestor(
            skeleton,
            BoneId(profile.bones[&ancestor]),
            BoneId(profile.bones[&descendant]),
        ) {
            return Err(HumanoidError::InvalidHierarchy {
                ancestor,
                descendant,
            });
        }
    }
    Ok(())
}

/// Detects a conservative first profile from common DCC, game-engine, and PMX conventions.
///
/// Detection is an import-time proposal only. Matches are immediately converted
/// to stable BoneIds and structurally validated; runtime conversion never rebinds
/// by name. Hierarchy constraints and ordered naming preferences may resolve a
/// stronger candidate, while unresolved ties remain explicit diagnostics.
pub fn detect_humanoid_profile(skeleton: &SkeletonAsset) -> HumanoidProfileDetection {
    let mut candidates = BTreeMap::<HumanoidBone, Vec<HumanoidCandidate>>::new();
    for bone in &skeleton.bones {
        let normalized = normalize_bone_name(&bone.name);
        for semantic in all_semantics() {
            if let Some(priority) = alias_priority(semantic, &normalized) {
                push_candidate(&mut candidates, semantic, bone.id, priority);
            }
        }
    }

    prune_candidates_by_hierarchy(skeleton, &mut candidates);
    infer_hips_candidate(skeleton, &mut candidates);
    prune_candidates_by_hierarchy(skeleton, &mut candidates);

    let mut bones = BTreeMap::new();
    let mut uncertain_bones = Vec::new();
    let mut diagnostics = Vec::new();
    for semantic in all_semantics() {
        match candidates.get(&semantic).map(Vec::as_slice) {
            Some([candidate]) => {
                bones.insert(semantic, candidate.bone.0);
            }
            Some(found) if !found.is_empty() => {
                uncertain_bones.push(semantic);
                if let Some(candidate) = preferred_candidate(found) {
                    bones.insert(semantic, candidate.bone.0);
                    diagnostics.push(Diagnostic::warning(
                        "anim.humanoid_mapping_ambiguous",
                        format!(
                            "humanoid semantic {semantic:?} matched {} source bones; hierarchy and naming preference selected one candidate, so review the model profile",
                            found.len()
                        ),
                    ));
                } else {
                    diagnostics.push(Diagnostic::warning(
                        "anim.humanoid_mapping_ambiguous",
                        format!(
                            "humanoid semantic {semantic:?} matched {} equally plausible source bones; configure the model profile explicitly",
                            found.len()
                        ),
                    ));
                }
            }
            _ => {}
        }
    }

    let motion_root = skeleton
        .bones
        .iter()
        .find(|bone| is_motion_root_name(&bone.name))
        .map(|bone| bone.id.0);
    let profile = HumanoidProfile {
        skeleton: skeleton.id.as_str().to_owned(),
        skeleton_identity: skeleton.identity.0,
        bones,
        motion_root,
        uncertain_bones,
        origin: HumanoidProfileOrigin::Automatic,
    };

    match validate_humanoid_profile(&profile, skeleton) {
        Ok(()) => HumanoidProfileDetection {
            profile: Some(profile),
            diagnostics,
        },
        Err(error) => {
            diagnostics.push(Diagnostic::warning(
                "anim.humanoid_profile_unavailable",
                format!(
                    "skeleton `{}` cannot expose a Humanoid variant automatically: {error}",
                    skeleton.name
                ),
            ));
            HumanoidProfileDetection {
                profile: None,
                diagnostics,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HumanoidCandidate {
    bone: BoneId,
    priority: usize,
}

fn alias_priority(semantic: HumanoidBone, normalized: &str) -> Option<usize> {
    aliases(semantic)
        .iter()
        .position(|alias| *alias == normalized)
        .map(|index| index + 1)
}

fn push_candidate(
    candidates: &mut BTreeMap<HumanoidBone, Vec<HumanoidCandidate>>,
    semantic: HumanoidBone,
    bone: BoneId,
    priority: usize,
) {
    let entries = candidates.entry(semantic).or_default();
    if let Some(existing) = entries.iter_mut().find(|candidate| candidate.bone == bone) {
        existing.priority = existing.priority.min(priority);
    } else {
        entries.push(HumanoidCandidate { bone, priority });
    }
}

fn preferred_candidate(candidates: &[HumanoidCandidate]) -> Option<HumanoidCandidate> {
    let best_priority = candidates.iter().map(|candidate| candidate.priority).min()?;
    let mut preferred = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.priority == best_priority);
    let candidate = preferred.next()?;
    preferred.next().is_none().then_some(candidate)
}

fn prune_candidates_by_hierarchy(
    skeleton: &SkeletonAsset,
    candidates: &mut BTreeMap<HumanoidBone, Vec<HumanoidCandidate>>,
) {
    loop {
        let mut changed = false;
        for (ancestor_semantic, descendant_semantic) in required_ancestry_pairs() {
            let Some(ancestors) = candidates.get(&ancestor_semantic).cloned() else {
                continue;
            };
            let Some(descendants) = candidates.get(&descendant_semantic).cloned() else {
                continue;
            };

            let has_compatible_pair = ancestors.iter().any(|ancestor| {
                descendants.iter().any(|descendant| {
                    is_ancestor(skeleton, ancestor.bone, descendant.bone)
                })
            });
            if !has_compatible_pair {
                continue;
            }

            let filtered_ancestors = ancestors
                .iter()
                .copied()
                .filter(|ancestor| {
                    descendants.iter().any(|descendant| {
                        is_ancestor(skeleton, ancestor.bone, descendant.bone)
                    })
                })
                .collect::<Vec<_>>();
            let filtered_descendants = descendants
                .iter()
                .copied()
                .filter(|descendant| {
                    ancestors.iter().any(|ancestor| {
                        is_ancestor(skeleton, ancestor.bone, descendant.bone)
                    })
                })
                .collect::<Vec<_>>();

            if filtered_ancestors.len() != ancestors.len() {
                candidates.insert(ancestor_semantic, filtered_ancestors);
                changed = true;
            }
            if filtered_descendants.len() != descendants.len() {
                candidates.insert(descendant_semantic, filtered_descendants);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

fn infer_hips_candidate(
    skeleton: &SkeletonAsset,
    candidates: &mut BTreeMap<HumanoidBone, Vec<HumanoidCandidate>>,
) {
    use HumanoidBone::*;

    let Some(spine) = candidates
        .get(&Spine)
        .and_then(|found| preferred_candidate(found))
        .map(|candidate| candidate.bone)
    else {
        return;
    };
    let Some(left_upper_leg) = candidates
        .get(&LeftUpperLeg)
        .and_then(|found| preferred_candidate(found))
        .map(|candidate| candidate.bone)
    else {
        return;
    };
    let Some(right_upper_leg) = candidates
        .get(&RightUpperLeg)
        .and_then(|found| preferred_candidate(found))
        .map(|candidate| candidate.bone)
    else {
        return;
    };
    let anchors = [spine, left_upper_leg, right_upper_leg];
    let Some(hips) = lowest_common_ancestor(skeleton, &anchors) else {
        return;
    };
    if anchors.contains(&hips) {
        return;
    }
    let Some(hips_index) = skeleton.bone_index(hips) else {
        return;
    };
    let Some(hips_bone) = skeleton.bones.get(hips_index) else {
        return;
    };
    if hips_bone.parent.is_none() || is_motion_root_name(&hips_bone.name) {
        return;
    }

    push_candidate(candidates, Hips, hips, 0);
}

fn lowest_common_ancestor(skeleton: &SkeletonAsset, bones: &[BoneId]) -> Option<BoneId> {
    let (&first, rest) = bones.split_first()?;
    let mut current = skeleton.bone_index(first)?;
    loop {
        let bone = skeleton.bones.get(current)?;
        if rest
            .iter()
            .all(|&descendant| is_ancestor(skeleton, bone.id, descendant))
        {
            return Some(bone.id);
        }
        current = bone.parent?;
    }
}

fn is_motion_root_name(name: &str) -> bool {
    matches!(
        normalize_bone_name(name).as_str(),
        "root" | "motionroot" | "全ての親" | "センター"
    )
}

fn required_ancestry_pairs() -> [(HumanoidBone, HumanoidBone); 14] {
    use HumanoidBone::*;
    [
        (Hips, Spine),
        (Spine, Head),
        (Spine, LeftUpperArm),
        (LeftUpperArm, LeftLowerArm),
        (LeftLowerArm, LeftHand),
        (Spine, RightUpperArm),
        (RightUpperArm, RightLowerArm),
        (RightLowerArm, RightHand),
        (Hips, LeftUpperLeg),
        (LeftUpperLeg, LeftLowerLeg),
        (LeftLowerLeg, LeftFoot),
        (Hips, RightUpperLeg),
        (RightUpperLeg, RightLowerLeg),
        (RightLowerLeg, RightFoot),
    ]
}

fn is_ancestor(skeleton: &SkeletonAsset, ancestor: BoneId, descendant: BoneId) -> bool {
    let Some(ancestor) = skeleton.bone_index(ancestor) else {
        return false;
    };
    let Some(mut current) = skeleton.bone_index(descendant) else {
        return false;
    };
    loop {
        if current == ancestor {
            return true;
        }
        let Some(parent) = skeleton.bones[current].parent else {
            return false;
        };
        current = parent;
    }
}

fn all_semantics() -> [HumanoidBone; 35] {
    use HumanoidBone::*;
    [
        Hips,
        Spine,
        Chest,
        UpperChest,
        Neck,
        Head,
        LeftShoulder,
        LeftUpperArm,
        LeftLowerArm,
        LeftHand,
        RightShoulder,
        RightUpperArm,
        RightLowerArm,
        RightHand,
        LeftUpperLeg,
        LeftLowerLeg,
        LeftFoot,
        LeftToes,
        RightUpperLeg,
        RightLowerLeg,
        RightFoot,
        RightToes,
        LeftThumbProximal,
        LeftIndexProximal,
        LeftMiddleProximal,
        LeftRingProximal,
        LeftLittleProximal,
        RightThumbProximal,
        RightIndexProximal,
        RightMiddleProximal,
        RightRingProximal,
        RightLittleProximal,
        LeftEye,
        RightEye,
        Jaw,
    ]
}

fn aliases(semantic: HumanoidBone) -> &'static [&'static str] {
    use HumanoidBone::*;
    match semantic {
        Hips => &["hips", "pelvis", "hip", "腰", "下半身"],
        Spine => &["spine", "waist", "spine1", "torso", "upperbody", "上半身"],
        Chest => &["chest", "spine2", "upperbody2", "thorax", "上半身2"],
        UpperChest => &["upperchest", "spine3", "upperbody3", "上半身3"],
        Neck => &["neck", "neck1", "首"],
        Head => &["head", "頭"],
        LeftShoulder => &[
            "leftshoulder",
            "leftclavicle",
            "leftcollar",
            "lshoulder",
            "lclavicle",
            "lcollar",
            "左肩",
        ],
        LeftUpperArm => &[
            "leftupperarm",
            "leftuparm",
            "leftarm",
            "lupperarm",
            "luparm",
            "larm",
            "左腕",
        ],
        LeftLowerArm => &[
            "leftlowerarm",
            "leftforearm",
            "leftelbow",
            "llowerarm",
            "lforearm",
            "lelbow",
            "左ひじ",
            "左肘",
        ],
        LeftHand => &["lefthand", "leftwrist", "lhand", "lwrist", "左手首"],
        RightShoulder => &[
            "rightshoulder",
            "rightclavicle",
            "rightcollar",
            "rshoulder",
            "rclavicle",
            "rcollar",
            "右肩",
        ],
        RightUpperArm => &[
            "rightupperarm",
            "rightuparm",
            "rightarm",
            "rupperarm",
            "ruparm",
            "rarm",
            "右腕",
        ],
        RightLowerArm => &[
            "rightlowerarm",
            "rightforearm",
            "rightelbow",
            "rlowerarm",
            "rforearm",
            "relbow",
            "右ひじ",
            "右肘",
        ],
        RightHand => &["righthand", "rightwrist", "rhand", "rwrist", "右手首"],
        LeftUpperLeg => &[
            "leftupperleg",
            "leftupleg",
            "leftthigh",
            "lefthip",
            "lupperleg",
            "lupleg",
            "lthigh",
            "lhip",
            "左足",
        ],
        LeftLowerLeg => &[
            "leftlowerleg",
            "leftleg",
            "leftcalf",
            "leftshin",
            "leftknee",
            "llowerleg",
            "lleg",
            "lcalf",
            "lshin",
            "lknee",
            "左ひざ",
            "左膝",
        ],
        LeftFoot => &["leftfoot", "leftankle", "lfoot", "lankle", "左足首"],
        LeftToes => &[
            "lefttoes",
            "lefttoe",
            "lefttoebase",
            "leftball",
            "ltoes",
            "ltoe",
            "ltoebase",
            "lball",
            "左つま先",
        ],
        RightUpperLeg => &[
            "rightupperleg",
            "rightupleg",
            "rightthigh",
            "righthip",
            "rupperleg",
            "rupleg",
            "rthigh",
            "rhip",
            "右足",
        ],
        RightLowerLeg => &[
            "rightlowerleg",
            "rightleg",
            "rightcalf",
            "rightshin",
            "rightknee",
            "rlowerleg",
            "rleg",
            "rcalf",
            "rshin",
            "rknee",
            "右ひざ",
            "右膝",
        ],
        RightFoot => &["rightfoot", "rightankle", "rfoot", "rankle", "右足首"],
        RightToes => &[
            "righttoes",
            "righttoe",
            "righttoebase",
            "rightball",
            "rtoes",
            "rtoe",
            "rtoebase",
            "rball",
            "右つま先",
        ],
        LeftThumbProximal => &["leftthumb1", "leftthumbproximal", "lthumb1"],
        LeftIndexProximal => &["leftindex1", "leftindexproximal", "lindex1"],
        LeftMiddleProximal => &["leftmiddle1", "leftmiddleproximal", "lmiddle1"],
        LeftRingProximal => &["leftring1", "leftringproximal", "lring1"],
        LeftLittleProximal => &[
            "leftlittle1",
            "leftpinky1",
            "leftlittleproximal",
            "llittle1",
            "lpinky1",
        ],
        RightThumbProximal => &["rightthumb1", "rightthumbproximal", "rthumb1"],
        RightIndexProximal => &["rightindex1", "rightindexproximal", "rindex1"],
        RightMiddleProximal => &["rightmiddle1", "rightmiddleproximal", "rmiddle1"],
        RightRingProximal => &["rightring1", "rightringproximal", "rring1"],
        RightLittleProximal => &[
            "rightlittle1",
            "rightpinky1",
            "rightlittleproximal",
            "rlittle1",
            "rpinky1",
        ],
        LeftEye => &["lefteye", "lefteyeball", "leye", "左目"],
        RightEye => &["righteye", "righteyeball", "reye", "右目"],
        Jaw => &["jaw", "mandible", "あご", "顎"],
    }
}

fn normalize_bone_name(name: &str) -> String {
    let leaf = name.rsplit([':', '|']).next().unwrap_or(name);
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut previous_was_lower_or_digit = false;

    for character in leaf.chars() {
        if !character.is_alphanumeric() {
            if !token.is_empty() {
                tokens.push(normalize_name_token(&token));
                token.clear();
            }
            previous_was_lower_or_digit = false;
            continue;
        }
        if character.is_ascii_uppercase() && previous_was_lower_or_digit && !token.is_empty() {
            tokens.push(normalize_name_token(&token));
            token.clear();
        }
        token.extend(character.to_lowercase());
        previous_was_lower_or_digit = character.is_ascii_lowercase() || character.is_ascii_digit();
    }
    if !token.is_empty() {
        tokens.push(normalize_name_token(&token));
    }

    while matches!(
        tokens.first().map(String::as_str),
        Some(
            "armature"
                | "skeleton"
                | "rig"
                | "mixamorig"
                | "bip"
                | "bip1"
                | "j"
                | "joint"
                | "jnt"
                | "bone"
                | "def"
                | "cc"
                | "base"
        )
    ) {
        tokens.remove(0);
    }

    let mut side = None;
    let mut body = String::new();
    for token in tokens {
        let token_side = match token.as_str() {
            "l" | "lf" | "left" => Some("left"),
            "r" | "rt" | "right" => Some("right"),
            _ => None,
        };
        if let Some(token_side) = token_side {
            match side {
                Some(existing) if existing != token_side => return String::new(),
                Some(_) => {}
                None => side = Some(token_side),
            }
        } else {
            body.push_str(&token);
        }
    }

    match side {
        Some(side) => format!("{side}{body}"),
        None => body,
    }
}

fn normalize_name_token(token: &str) -> String {
    let Some(digit_start) = token.find(|character: char| character.is_ascii_digit()) else {
        return token.to_owned();
    };
    let (prefix, digits) = token.split_at(digit_start);
    if !digits.chars().all(|character| character.is_ascii_digit()) {
        return token.to_owned();
    }
    let digits = digits.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    format!("{prefix}{digits}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton_asset::{compute_skeleton_identity, BoneDef};
    use engine_authoring::id::AssetId;
    use glam::{Quat, Vec3};

    fn skeleton_from(definitions: &[(&str, Option<usize>)]) -> SkeletonAsset {
        let bones = definitions
            .iter()
            .enumerate()
            .map(|(index, (name, parent))| BoneDef {
                id: BoneId(index as u32),
                name: (*name).to_owned(),
                parent: *parent,
                rest_translation: Vec3::ZERO,
                rest_rotation: Quat::IDENTITY,
                rest_scale: Vec3::ONE,
            })
            .collect::<Vec<_>>();
        SkeletonAsset {
            id: AssetId::generate(),
            name: "humanoid".to_owned(),
            identity: compute_skeleton_identity(&bones),
            next_bone_id: bones.len() as u32,
            bones,
        }
    }

    fn skeleton() -> SkeletonAsset {
        skeleton_from(&[
            ("Hips", None),
            ("helper", Some(0)),
            ("Spine", Some(1)),
            ("Head", Some(2)),
            ("LeftArm", Some(2)),
            ("LeftForeArm", Some(4)),
            ("LeftHand", Some(5)),
            ("RightArm", Some(2)),
            ("RightForeArm", Some(7)),
            ("RightHand", Some(8)),
            ("LeftUpLeg", Some(0)),
            ("LeftLeg", Some(10)),
            ("LeftFoot", Some(11)),
            ("RightUpLeg", Some(0)),
            ("RightLeg", Some(13)),
            ("RightFoot", Some(14)),
        ])
    }

    #[test]
    fn detection_accepts_helper_bones_between_required_semantics() {
        let skeleton = skeleton();
        let profile = detect_humanoid_profile(&skeleton).profile.expect("profile");
        assert_eq!(profile.bone_id(HumanoidBone::Spine), Some(2));
        assert!(validate_humanoid_profile(&profile, &skeleton).is_ok());
    }

    #[test]
    fn detection_accepts_mmd_semi_standard_branching_lower_body() {
        let skeleton = skeleton_from(&[
            ("全ての親", None),
            ("センター", Some(0)),
            ("グルーブ", Some(1)),
            ("腰", Some(2)),
            ("上半身", Some(3)),
            ("上半身2", Some(4)),
            ("首", Some(5)),
            ("頭", Some(6)),
            ("左腕", Some(5)),
            ("左ひじ", Some(8)),
            ("左手首", Some(9)),
            ("右腕", Some(5)),
            ("右ひじ", Some(11)),
            ("右手首", Some(12)),
            ("下半身", Some(3)),
            ("左足", Some(14)),
            ("左ひざ", Some(15)),
            ("左足首", Some(16)),
            ("右足", Some(14)),
            ("右ひざ", Some(18)),
            ("右足首", Some(19)),
        ]);

        let profile = detect_humanoid_profile(&skeleton).profile.expect("profile");
        assert_eq!(profile.bone_id(HumanoidBone::Hips), Some(3));
        assert_eq!(profile.bone_id(HumanoidBone::Spine), Some(4));
        assert_eq!(profile.bone_id(HumanoidBone::LeftUpperLeg), Some(15));
        assert_eq!(profile.motion_root, Some(0));
        assert!(validate_humanoid_profile(&profile, &skeleton).is_ok());
    }

    #[test]
    fn detection_infers_branching_hips_when_name_is_unknown() {
        let skeleton = skeleton_from(&[
            ("root", None),
            ("center", Some(0)),
            ("pelvic_control_x", Some(1)),
            ("UpperBody", Some(2)),
            ("Head", Some(3)),
            ("LeftUpperArm", Some(3)),
            ("LeftLowerArm", Some(5)),
            ("LeftHand", Some(6)),
            ("RightUpperArm", Some(3)),
            ("RightLowerArm", Some(8)),
            ("RightHand", Some(9)),
            ("LowerBody", Some(2)),
            ("LeftUpperLeg", Some(11)),
            ("LeftLowerLeg", Some(12)),
            ("LeftFoot", Some(13)),
            ("RightUpperLeg", Some(11)),
            ("RightLowerLeg", Some(15)),
            ("RightFoot", Some(16)),
        ]);

        let profile = detect_humanoid_profile(&skeleton).profile.expect("profile");
        assert_eq!(profile.bone_id(HumanoidBone::Hips), Some(2));
        assert_eq!(profile.bone_id(HumanoidBone::Spine), Some(3));
        assert!(validate_humanoid_profile(&profile, &skeleton).is_ok());
    }

    #[test]
    fn detection_accepts_unreal_style_side_suffixes_and_numbered_spine() {
        let skeleton = skeleton_from(&[
            ("root", None),
            ("pelvis", Some(0)),
            ("spine_01", Some(1)),
            ("spine_02", Some(2)),
            ("neck_01", Some(3)),
            ("head", Some(4)),
            ("clavicle_l", Some(3)),
            ("upperarm_l", Some(6)),
            ("lowerarm_l", Some(7)),
            ("hand_l", Some(8)),
            ("clavicle_r", Some(3)),
            ("upperarm_r", Some(10)),
            ("lowerarm_r", Some(11)),
            ("hand_r", Some(12)),
            ("thigh_l", Some(1)),
            ("calf_l", Some(14)),
            ("foot_l", Some(15)),
            ("ball_l", Some(16)),
            ("thigh_r", Some(1)),
            ("calf_r", Some(18)),
            ("foot_r", Some(19)),
            ("ball_r", Some(20)),
        ]);

        let profile = detect_humanoid_profile(&skeleton).profile.expect("profile");
        assert_eq!(profile.bone_id(HumanoidBone::Hips), Some(1));
        assert_eq!(profile.bone_id(HumanoidBone::Spine), Some(2));
        assert_eq!(profile.bone_id(HumanoidBone::Chest), Some(3));
        assert_eq!(profile.bone_id(HumanoidBone::LeftUpperArm), Some(7));
        assert_eq!(profile.bone_id(HumanoidBone::RightFoot), Some(20));
        assert_eq!(profile.bone_id(HumanoidBone::LeftToes), Some(17));
        assert!(validate_humanoid_profile(&profile, &skeleton).is_ok());
    }

    #[test]
    fn detection_accepts_biped_prefix_and_side_tokens() {
        let skeleton = skeleton_from(&[
            ("Bip01 Pelvis", None),
            ("Bip01 Spine", Some(0)),
            ("Bip01 Head", Some(1)),
            ("Bip01 L UpperArm", Some(1)),
            ("Bip01 L Forearm", Some(3)),
            ("Bip01 L Hand", Some(4)),
            ("Bip01 R UpperArm", Some(1)),
            ("Bip01 R Forearm", Some(6)),
            ("Bip01 R Hand", Some(7)),
            ("Bip01 L Thigh", Some(0)),
            ("Bip01 L Calf", Some(9)),
            ("Bip01 L Foot", Some(10)),
            ("Bip01 R Thigh", Some(0)),
            ("Bip01 R Calf", Some(12)),
            ("Bip01 R Foot", Some(13)),
        ]);

        let profile = detect_humanoid_profile(&skeleton).profile.expect("profile");
        assert_eq!(profile.bone_id(HumanoidBone::Hips), Some(0));
        assert_eq!(profile.bone_id(HumanoidBone::LeftLowerArm), Some(4));
        assert_eq!(profile.bone_id(HumanoidBone::RightUpperLeg), Some(12));
        assert!(validate_humanoid_profile(&profile, &skeleton).is_ok());
    }

    #[test]
    fn detection_prefers_base_spine_when_numbered_spine_is_also_present() {
        let skeleton = skeleton_from(&[
            ("Hips", None),
            ("Spine", Some(0)),
            ("Spine_01", Some(1)),
            ("Head", Some(2)),
            ("LeftUpperArm", Some(2)),
            ("LeftLowerArm", Some(4)),
            ("LeftHand", Some(5)),
            ("RightUpperArm", Some(2)),
            ("RightLowerArm", Some(7)),
            ("RightHand", Some(8)),
            ("LeftUpperLeg", Some(0)),
            ("LeftLowerLeg", Some(10)),
            ("LeftFoot", Some(11)),
            ("RightUpperLeg", Some(0)),
            ("RightLowerLeg", Some(13)),
            ("RightFoot", Some(14)),
        ]);

        let detection = detect_humanoid_profile(&skeleton);
        let profile = detection.profile.expect("profile");
        assert_eq!(profile.bone_id(HumanoidBone::Spine), Some(1));
        assert!(profile.uncertain_bones.contains(&HumanoidBone::Spine));
        assert!(validate_humanoid_profile(&profile, &skeleton).is_ok());
    }

    #[test]
    fn normalization_accepts_common_dcc_side_and_prefix_variants() {
        assert_eq!(
            normalize_bone_name("Armature|DEF-upper_arm.L"),
            "leftupperarm"
        );
        assert_eq!(normalize_bone_name("CC_Base_L_Thigh"), "leftthigh");
        assert_eq!(normalize_bone_name("spine_01"), "spine1");
    }

    #[test]
    fn duplicate_mapping_is_invalid() {
        let skeleton = skeleton();
        let mut profile = detect_humanoid_profile(&skeleton).profile.expect("profile");
        profile.bones.insert(
            HumanoidBone::RightHand,
            profile.bones[&HumanoidBone::LeftHand],
        );
        assert!(matches!(
            validate_humanoid_profile(&profile, &skeleton),
            Err(HumanoidError::DuplicateBone { .. })
        ));
    }
}
