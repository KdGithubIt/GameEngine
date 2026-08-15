//! Ground-contact interval detection over an animation clip (ADR 0080 §1).
//!
//! Detection is the only part of the ADR 0080 foot-IK pipeline that is
//! baked: [`detect_contact_intervals`] FK-samples a clip's own channels at a
//! fixed rate and records when a candidate bone is both slow and low
//! relative to its own per-clip minimum height. The result
//! ([`ContactInterval`]) is stored on [`crate::animation::AnimationClip`].
//! Runtime correction ([`crate::foot_ik`]) is a separate, never-baked pass
//! that consumes these intervals against the final blended pose.
//!
//! Detection must be re-run (never copied) whenever a clip's channels
//! change proportions — in particular after [`crate::retarget::retarget_clip`]
//! produces a baked clip for a different skeleton, since the target rig's
//! own leg lengths decide where its feet actually plant.

use crate::animation::AnimationClip;
use crate::retarget::{model_pose_from_local, sampled_local_pose};
use crate::skeleton_asset::{BoneId, SkeletonAsset};
use glam::Vec3;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Thresholds (ADR 0080 §1)
// ---------------------------------------------------------------------------

/// Rate at which a clip's candidate bones are FK-sampled for contact
/// detection, in Hz.
pub(crate) const CONTACT_SAMPLE_HZ: f32 = 60.0;

/// A candidate bone counts as planted only while its model-space speed
/// between consecutive samples stays below this threshold, in meters per
/// second.
pub(crate) const CONTACT_SPEED_THRESHOLD: f32 = 0.15;

/// A candidate bone counts as planted only while its height above its own
/// per-clip minimum (this bone's lowest sampled model-space Y across the
/// whole clip) stays below this threshold, in meters.
pub(crate) const CONTACT_HEIGHT_THRESHOLD: f32 = 0.08;

/// A contiguous run of contact frames must span at least this long, in
/// seconds, to survive as a [`ContactInterval`].
pub(crate) const CONTACT_MIN_DURATION: f32 = 0.1;

/// Two adjacent contact runs separated by a gap shorter than this, in
/// seconds, are bridged into a single interval instead of staying separate.
pub(crate) const CONTACT_GAP_BRIDGE: f32 = 0.05;

/// Default candidate name patterns used when
/// [`crate::asset::ImportSettings::contact_bones`] is empty (ADR 0080 §1).
/// Matched case-insensitively as a substring of the bone name.
const DEFAULT_CANDIDATE_NAME_PATTERNS: &[&str] = &["foot", "ankle", "toe"];

// ---------------------------------------------------------------------------
// ContactInterval
// ---------------------------------------------------------------------------

/// One detected ground-contact span for a single bone (ADR 0080 §1).
///
/// `start` is inclusive and `end` is exclusive, both in clip-local seconds.
/// Stored on [`AnimationClip::contacts`], sorted by `(start, bone)`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ContactInterval {
    /// The candidate bone this interval was detected for.
    pub bone: BoneId,
    /// Clip-local start time in seconds, inclusive.
    pub start: f32,
    /// Clip-local end time in seconds, exclusive.
    pub end: f32,
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Detects [`ContactInterval`]s for every candidate bone in `skeleton`,
/// FK-sampling `clip` against it at a fixed 60 Hz rate (ADR 0080 §1).
///
/// Candidate bones are every bone in `skeleton` whose name contains
/// `"foot"`, `"ankle"`, or `"toe"` (case-insensitive), unless
/// `contact_bone_names` is non-empty — in which case it **replaces** that
/// heuristic entirely: only bones whose name case-insensitively equals one
/// of `contact_bone_names`'s entries are considered.
///
/// Returns an empty list when `clip.duration` is non-positive or no
/// candidate bone resolves, rather than treating either as an error: a clip
/// with no feet (or no motion) simply has no contacts, which is a valid and
/// common outcome, not a failure.
pub fn detect_contact_intervals(
    clip: &AnimationClip,
    skeleton: &SkeletonAsset,
    contact_bone_names: &[String],
) -> Vec<ContactInterval> {
    let candidates = candidate_bone_indices(skeleton, contact_bone_names);
    if candidates.is_empty() || clip.duration <= 0.0 {
        return Vec::new();
    }

    let sample_times = sample_times(clip.duration);
    let mut positions: Vec<Vec<Vec3>> =
        vec![Vec::with_capacity(sample_times.len()); candidates.len()];
    for &t in &sample_times {
        let local = sampled_local_pose(skeleton, clip, t);
        let model = model_pose_from_local(skeleton, &local);
        for (slot, &bone_index) in candidates.iter().enumerate() {
            positions[slot].push(model[bone_index].0);
        }
    }

    let mut all_intervals = Vec::new();
    for (slot, &bone_index) in candidates.iter().enumerate() {
        let bone_id = skeleton.bones[bone_index].id;
        all_intervals.extend(detect_intervals_for_bone(
            bone_id,
            &positions[slot],
            &sample_times,
            clip.duration,
        ));
    }
    all_intervals.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.bone.0.cmp(&b.bone.0))
    });
    all_intervals
}

/// Resolves candidate bone indices into `skeleton.bones` per
/// [`detect_contact_intervals`]'s name-matching rule.
fn candidate_bone_indices(skeleton: &SkeletonAsset, contact_bone_names: &[String]) -> Vec<usize> {
    if contact_bone_names.is_empty() {
        skeleton
            .bones
            .iter()
            .enumerate()
            .filter(|(_, bone)| matches_default_candidate_pattern(&bone.name))
            .map(|(index, _)| index)
            .collect()
    } else {
        let wanted: Vec<String> = contact_bone_names
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect();
        skeleton
            .bones
            .iter()
            .enumerate()
            .filter(|(_, bone)| wanted.contains(&bone.name.to_ascii_lowercase()))
            .map(|(index, _)| index)
            .collect()
    }
}

fn matches_default_candidate_pattern(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    DEFAULT_CANDIDATE_NAME_PATTERNS
        .iter()
        .any(|pattern| lower.contains(pattern))
}

/// Builds the sample-time grid `[0, dt, 2*dt, ..., duration]` at
/// [`CONTACT_SAMPLE_HZ`], always including `duration` itself as the final
/// sample regardless of whether it lands exactly on the grid.
fn sample_times(duration: f32) -> Vec<f32> {
    let dt = 1.0 / CONTACT_SAMPLE_HZ;
    let count = (duration / dt).round() as usize + 1;
    (0..count)
        .map(|index| (index as f32 * dt).min(duration))
        .collect()
}

/// Detects contact runs for a single bone's already-sampled model-space
/// positions, per [`detect_contact_intervals`]'s speed/height/duration/gap
/// rules.
fn detect_intervals_for_bone(
    bone_id: BoneId,
    positions: &[Vec3],
    times: &[f32],
    duration: f32,
) -> Vec<ContactInterval> {
    let sample_count = positions.len();
    if sample_count == 0 {
        return Vec::new();
    }
    let min_height = positions
        .iter()
        .map(|position| position.y)
        .fold(f32::INFINITY, f32::min);

    // Forward-difference speed: sample i's speed is measured against the
    // *next* sample, so a run's reported end time lands exactly where
    // motion resumes rather than one sample late. The final sample has no
    // "next" to measure against, so it repeats the previous speed.
    let mut speeds = vec![0.0_f32; sample_count];
    for i in 0..sample_count.saturating_sub(1) {
        let dt = (times[i + 1] - times[i]).max(f32::EPSILON);
        speeds[i] = (positions[i + 1] - positions[i]).length() / dt;
    }
    if sample_count >= 2 {
        speeds[sample_count - 1] = speeds[sample_count - 2];
    }

    let is_contact: Vec<bool> = (0..sample_count)
        .map(|i| {
            speeds[i] < CONTACT_SPEED_THRESHOLD
                && (positions[i].y - min_height) < CONTACT_HEIGHT_THRESHOLD
        })
        .collect();

    let mut runs: Vec<(f32, f32)> = Vec::new();
    let mut run_start: Option<usize> = None;
    for i in 0..sample_count {
        if is_contact[i] {
            if run_start.is_none() {
                run_start = Some(i);
            }
        } else if let Some(start) = run_start.take() {
            runs.push((times[start], times[i]));
        }
    }
    if let Some(start) = run_start {
        // Contact persists through the clip's last sample: the run has no
        // observed "motion resumes" frame, so its end is one sample beyond
        // the last sample, clamped to the clip's own duration.
        let dt = 1.0 / CONTACT_SAMPLE_HZ;
        let end = (times[sample_count - 1] + dt).min(duration);
        runs.push((times[start], end));
    }

    // Bridge adjacent runs separated by a sub-threshold gap (ADR 0080 §1).
    let mut bridged: Vec<(f32, f32)> = Vec::new();
    for (start, end) in runs {
        if let Some(last) = bridged.last_mut() {
            let gap: f32 = start - last.1;
            if gap < CONTACT_GAP_BRIDGE {
                last.1 = end;
                continue;
            }
        }
        bridged.push((start, end));
    }

    bridged
        .into_iter()
        .filter(|(start, end)| end - start >= CONTACT_MIN_DURATION)
        .map(|(start, end)| ContactInterval {
            bone: bone_id,
            start,
            end,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::{AnimChannel, AnimProperty, Keyframe};
    use crate::skeleton_asset::{compute_skeleton_identity, BoneDef};
    use engine_authoring::id::AssetId;
    use glam::Quat;

    fn single_bone_skeleton(name: &str) -> SkeletonAsset {
        let bones = vec![BoneDef {
            id: BoneId(0),
            name: name.to_owned(),
            parent: None,
            rest_translation: Vec3::ZERO,
            rest_rotation: Quat::IDENTITY,
            rest_scale: Vec3::ONE,
        }];
        let identity = compute_skeleton_identity(&bones);
        SkeletonAsset {
            id: AssetId::generate(),
            name: "rig".to_owned(),
            bones,
            identity,
            next_bone_id: 1,
        }
    }

    fn translation_channel(keyframes: &[(f32, f32, f32)]) -> AnimChannel {
        AnimChannel {
            property: AnimProperty::Translation,
            target_bone: Some(BoneId(0)),
            keyframes: keyframes
                .iter()
                .map(|&(t, x, y)| Keyframe {
                    time: t,
                    value: [x, y, 0.0, 1.0],
                })
                .collect(),
        }
    }

    fn clip_with_channel(duration: f32, channel: AnimChannel) -> AnimationClip {
        AnimationClip {
            duration,
            channels: vec![channel],
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        }
    }

    /// A one-second walk cycle for a single root-bone "foot": planted flat
    /// on the ground for the first and last 0.3s (a stance phase at each
    /// end), with a fast swing arc in between. Keyframe times are chosen as
    /// exact multiples of the 1/60s sample grid so linear interpolation
    /// reproduces exact values at every sampled time.
    fn sine_walk_clip() -> AnimationClip {
        clip_with_channel(
            1.0,
            translation_channel(&[
                (0.0, 0.0, 0.0),
                (18.0 / 60.0, 0.0, 0.0), // t=0.3: still planted
                (30.0 / 60.0, 0.5, 0.3), // t=0.5: mid-swing, lifted
                (42.0 / 60.0, 1.0, 0.0), // t=0.7: foot returns to ground
                (1.0, 1.0, 0.0),         // t=1.0: planted through clip end
            ]),
        )
    }

    #[test]
    fn detect_contact_intervals_finds_the_planted_stance_phases_of_a_sine_walk() {
        let skeleton = single_bone_skeleton("foot_l");
        let clip = sine_walk_clip();

        let intervals = detect_contact_intervals(&clip, &skeleton, &[]);

        assert_eq!(
            intervals.len(),
            2,
            "expected one stance interval at each end of the swing, got {intervals:?}"
        );
        assert_eq!(intervals[0].bone, BoneId(0));
        assert!(
            (intervals[0].start - 0.0).abs() < 1e-3 && (intervals[0].end - 0.3).abs() < 1e-3,
            "first stance interval must cover the first planted phase, got {:?}",
            intervals[0]
        );
        assert!(
            (intervals[1].start - 0.7).abs() < 1e-3 && (intervals[1].end - 1.0).abs() < 1e-3,
            "second stance interval must cover the closing planted phase, got {:?}",
            intervals[1]
        );
    }

    #[test]
    fn detect_contact_intervals_height_gate_prevents_a_full_clip_interval_when_speed_stays_low() {
        // A "moonwalk"-style trajectory: constant, slow vertical drift (well
        // under the speed threshold for the whole clip) that nonetheless
        // rises far enough above its own minimum to fail the height gate
        // for most of the clip. Speed alone must not be enough to mark the
        // whole clip as one contact interval.
        let skeleton = single_bone_skeleton("foot_r");
        // 10s clip, 0.05 m/s constant vertical drift: always below the 0.15
        // m/s speed threshold, but the foot rises 0.5m above its own
        // minimum by the end, far past the 0.08m height gate.
        let clip = clip_with_channel(
            10.0,
            translation_channel(&[(0.0, 0.0, 0.0), (10.0, 0.0, 0.5)]),
        );

        let intervals = detect_contact_intervals(&clip, &skeleton, &[]);

        let total_contact: f32 = intervals
            .iter()
            .map(|interval| interval.end - interval.start)
            .sum();
        assert!(
            total_contact < clip.duration,
            "the height gate must exclude the risen portion of the clip even though speed stays low, got {intervals:?}"
        );
        assert!(
            intervals.iter().all(|interval| interval.start < 2.0),
            "contact must only cover the near-minimum portion close to t=0, got {intervals:?}"
        );
    }

    #[test]
    fn contact_bones_override_replaces_the_name_heuristic() {
        let bones = vec![
            BoneDef {
                id: BoneId(0),
                name: "left_foot".to_owned(),
                parent: None,
                rest_translation: Vec3::ZERO,
                rest_rotation: Quat::IDENTITY,
                rest_scale: Vec3::ONE,
            },
            BoneDef {
                id: BoneId(1),
                name: "weapon_socket".to_owned(),
                parent: None,
                rest_translation: Vec3::ZERO,
                rest_rotation: Quat::IDENTITY,
                rest_scale: Vec3::ONE,
            },
        ];
        let identity = compute_skeleton_identity(&bones);
        let skeleton = SkeletonAsset {
            id: AssetId::generate(),
            name: "rig".to_owned(),
            bones,
            identity,
            next_bone_id: 2,
        };
        // A perfectly still clip: both bones are always at the origin, so
        // both would trivially satisfy the speed/height gates for their
        // entire duration if considered at all.
        let clip = AnimationClip {
            duration: 1.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        };

        let default_heuristic = detect_contact_intervals(&clip, &skeleton, &[]);
        assert_eq!(
            default_heuristic.len(),
            1,
            "only `left_foot` matches the name heuristic"
        );
        assert_eq!(default_heuristic[0].bone, BoneId(0));

        let overridden = detect_contact_intervals(&clip, &skeleton, &["weapon_socket".to_owned()]);
        assert_eq!(
            overridden.len(),
            1,
            "the override must replace, not add to, the name heuristic"
        );
        assert_eq!(
            overridden[0].bone,
            BoneId(1),
            "the override must select the named bone even though its name does not match the heuristic"
        );
    }

    #[test]
    fn detect_intervals_for_bone_bridges_a_short_gap_between_two_contact_runs() {
        // Two flat (zero-speed) runs separated by a single fast, excursion
        // sample at index 8: the forward-difference speed check marks
        // indices 7 and 8 as non-contact (transition out, then in), a gap
        // of 2/60s ~= 0.033s, under the 0.05s bridge threshold, so the two
        // runs must merge into one interval.
        let dt = 1.0 / CONTACT_SAMPLE_HZ;
        let times: Vec<f32> = (0..20).map(|i| i as f32 * dt).collect();
        let positions: Vec<Vec3> = (0..20)
            .map(|i| {
                if i == 8 {
                    Vec3::new(10.0, 0.0, 0.0)
                } else {
                    Vec3::ZERO
                }
            })
            .collect();

        let intervals = detect_intervals_for_bone(BoneId(0), &positions, &times, times[19] + dt);

        assert_eq!(
            intervals.len(),
            1,
            "a sub-threshold gap must bridge into a single interval, got {intervals:?}"
        );
    }

    #[test]
    fn detect_intervals_for_bone_keeps_two_runs_separated_by_a_long_gap() {
        // Two flat runs separated by a fast excursion spanning indices
        // 12..20: the non-contact gap this produces (11..20, ~0.15s) is
        // safely above the 0.05s bridge threshold, and both surviving runs
        // are safely above the 0.1s minimum duration.
        let dt = 1.0 / CONTACT_SAMPLE_HZ;
        let sample_count = 30;
        let times: Vec<f32> = (0..sample_count).map(|i| i as f32 * dt).collect();
        let positions: Vec<Vec3> = (0..sample_count)
            .map(|i| {
                if (12..20).contains(&i) {
                    Vec3::new(i as f32, 0.0, 0.0)
                } else {
                    Vec3::ZERO
                }
            })
            .collect();

        let intervals =
            detect_intervals_for_bone(BoneId(0), &positions, &times, times[sample_count - 1] + dt);

        assert_eq!(
            intervals.len(),
            2,
            "a gap at or above the bridge threshold must keep two separate intervals, got {intervals:?}"
        );
    }

    #[test]
    fn detect_intervals_for_bone_drops_a_run_shorter_than_the_minimum_duration() {
        let dt = 1.0 / CONTACT_SAMPLE_HZ;
        let sample_count = 30;
        let times: Vec<f32> = (0..sample_count).map(|i| i as f32 * dt).collect();
        // Only 3 contact samples (~0.05s), well under the 0.1s minimum, with
        // no neighboring run to bridge with.
        let positions: Vec<Vec3> = (0..sample_count)
            .map(|i| {
                if (10..13).contains(&i) {
                    Vec3::ZERO
                } else {
                    Vec3::new(i as f32, 0.0, 0.0)
                }
            })
            .collect();

        let intervals =
            detect_intervals_for_bone(BoneId(0), &positions, &times, times[sample_count - 1] + dt);

        assert!(
            intervals.is_empty(),
            "an isolated run shorter than the minimum duration must be dropped, got {intervals:?}"
        );
    }
}
