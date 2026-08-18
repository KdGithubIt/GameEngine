//! Target-aware animation motion binding resolution (ADR 0154).
//!
//! This module owns the pure routing policy shared by runtime, Editor preview,
//! validation, and packaging. It deliberately performs no I/O and never uses
//! display names as identity.

use engine_authoring::id::AssetId;
use std::fmt;

/// Catalog-level kind of the selected Animation Set motion candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationMotionCandidateKind {
    /// A concrete Native animation bound to one source skeleton.
    ModelBound,
    /// A skeleton-independent Humanoid motion candidate.
    Humanoid,
}

/// Immutable facts consumed by [`plan_animation_motion`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationMotionPlanInput {
    /// Stable imported sub-asset selected by the Animation Set.
    pub candidate: AssetId,
    /// Catalog kind of [`Self::candidate`].
    pub candidate_kind: AnimationMotionCandidateKind,
    /// Skeleton bound by a model-specific Native candidate.
    pub source_skeleton: Option<AssetId>,
    /// Skeleton owned by the entity being previewed or played.
    pub target_skeleton: AssetId,
    /// Exact registered Retarget Map for source -> target, when one exists.
    pub retarget_map: Option<AssetId>,
    /// Logical portable Humanoid variant for the same imported animation.
    pub humanoid_fallback: Option<AssetId>,
    /// Whether the target skeleton currently has a usable Humanoid profile.
    pub target_humanoid_usable: bool,
}

/// Stable failure classes produced by the shared motion planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationMotionFailure {
    /// A model-bound candidate could not identify its source skeleton.
    MissingSourceSkeleton,
    /// An explicit Humanoid candidate cannot bake onto the selected target.
    TargetHumanoidUnavailable,
    /// Native did not match and neither Retarget nor Humanoid can resolve.
    NoCompatibleRoute,
}

impl fmt::Display for AnimationMotionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSourceSkeleton => formatter.write_str("source skeleton is unavailable"),
            Self::TargetHumanoidUnavailable => {
                formatter.write_str("target skeleton has no usable Humanoid profile")
            }
            Self::NoCompatibleRoute => formatter.write_str(
                "Native skeleton differs and no explicit Retarget Map or usable Humanoid fallback is available",
            ),
        }
    }
}

/// Deterministic route selected for one candidate/target pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnimationMotionRoute {
    /// Play the selected model-bound Native clip directly.
    Native,
    /// Retarget the selected Native clip through the named explicit map.
    Retarget {
        /// Stable registered Retarget Map asset.
        map: AssetId,
    },
    /// Bake the named skeleton-independent Humanoid motion for the target.
    Humanoid {
        /// Stable logical Humanoid motion asset.
        motion: AssetId,
    },
    /// No legal route exists for the selected candidate/target pair.
    Failed {
        /// Stable machine-readable failure class.
        reason: AnimationMotionFailure,
    },
}

impl AnimationMotionRoute {
    /// Short route label used by Editor badges and diagnostics.
    pub fn badge(&self) -> &'static str {
        match self {
            Self::Native => "Native",
            Self::Retarget { .. } => "Retarget",
            Self::Humanoid { .. } => "Humanoid",
            Self::Failed { .. } => "Failed",
        }
    }

    /// Human-readable routing trace suitable for Problems/build diagnostics.
    pub fn attempted_routing(&self) -> String {
        match self {
            Self::Native => "Native: source skeleton matches target".to_owned(),
            Self::Retarget { map } => format!("Native mismatch -> Retarget Map `{}`", map.as_str()),
            Self::Humanoid { motion } => {
                format!("Native mismatch -> no Retarget Map -> Humanoid `{}`", motion.as_str())
            }
            Self::Failed { reason } => {
                format!("Native -> Retarget -> Humanoid -> Failed: {reason}")
            }
        }
    }
}

/// Resolves one imported motion candidate for one concrete target skeleton.
///
/// Model-bound candidates follow the first-release precedence exactly:
/// Native -> explicit Retarget Map -> Humanoid -> Failed. Explicit Humanoid
/// candidates only take the Humanoid branch and never switch to Native or a
/// Retarget Map.
pub fn plan_animation_motion(input: &AnimationMotionPlanInput) -> AnimationMotionRoute {
    if input.candidate_kind == AnimationMotionCandidateKind::Humanoid {
        return if input.target_humanoid_usable {
            AnimationMotionRoute::Humanoid {
                motion: input.candidate.clone(),
            }
        } else {
            AnimationMotionRoute::Failed {
                reason: AnimationMotionFailure::TargetHumanoidUnavailable,
            }
        };
    }

    let Some(source_skeleton) = input.source_skeleton.as_ref() else {
        return AnimationMotionRoute::Failed {
            reason: AnimationMotionFailure::MissingSourceSkeleton,
        };
    };
    if source_skeleton == &input.target_skeleton {
        return AnimationMotionRoute::Native;
    }
    if let Some(map) = &input.retarget_map {
        return AnimationMotionRoute::Retarget { map: map.clone() };
    }
    if input.target_humanoid_usable
        && let Some(motion) = &input.humanoid_fallback
    {
        return AnimationMotionRoute::Humanoid {
            motion: motion.clone(),
        };
    }
    AnimationMotionRoute::Failed {
        reason: AnimationMotionFailure::NoCompatibleRoute,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> AssetId {
        AssetId::generate()
    }

    fn model_input(source: AssetId, target: AssetId) -> AnimationMotionPlanInput {
        AnimationMotionPlanInput {
            candidate: id(),
            candidate_kind: AnimationMotionCandidateKind::ModelBound,
            source_skeleton: Some(source),
            target_skeleton: target,
            retarget_map: None,
            humanoid_fallback: None,
            target_humanoid_usable: false,
        }
    }

    #[test]
    fn same_skeleton_resolves_native() {
        let skeleton = id();
        let input = model_input(skeleton.clone(), skeleton);
        assert_eq!(plan_animation_motion(&input), AnimationMotionRoute::Native);
    }

    #[test]
    fn retarget_precedes_humanoid_fallback() {
        let map = id();
        let humanoid = id();
        let mut input = model_input(id(), id());
        input.retarget_map = Some(map.clone());
        input.humanoid_fallback = Some(humanoid);
        input.target_humanoid_usable = true;
        assert_eq!(
            plan_animation_motion(&input),
            AnimationMotionRoute::Retarget { map }
        );
    }

    #[test]
    fn humanoid_is_used_after_native_and_retarget_fail() {
        let humanoid = id();
        let mut input = model_input(id(), id());
        input.humanoid_fallback = Some(humanoid.clone());
        input.target_humanoid_usable = true;
        assert_eq!(
            plan_animation_motion(&input),
            AnimationMotionRoute::Humanoid { motion: humanoid }
        );
    }

    #[test]
    fn model_candidate_fails_without_any_compatible_route() {
        let input = model_input(id(), id());
        assert_eq!(
            plan_animation_motion(&input),
            AnimationMotionRoute::Failed {
                reason: AnimationMotionFailure::NoCompatibleRoute
            }
        );
    }

    #[test]
    fn explicit_humanoid_never_switches_to_native_or_retarget() {
        let candidate = id();
        let target = id();
        let input = AnimationMotionPlanInput {
            candidate: candidate.clone(),
            candidate_kind: AnimationMotionCandidateKind::Humanoid,
            source_skeleton: Some(target.clone()),
            target_skeleton: target,
            retarget_map: Some(id()),
            humanoid_fallback: None,
            target_humanoid_usable: true,
        };
        assert_eq!(
            plan_animation_motion(&input),
            AnimationMotionRoute::Humanoid { motion: candidate }
        );
    }
}
