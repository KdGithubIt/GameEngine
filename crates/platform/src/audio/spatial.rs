//! Backend-neutral spatial-audio contracts and deterministic mixer math.

use std::f32::consts::FRAC_PI_4;

/// Runtime-only identifier for a platform audio voice.
///
/// Voice identifiers are allocated by [`super::AudioSystem`] and never belong
/// in authoring data or serialized scene state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AudioVoiceId(pub(crate) u64);

impl AudioVoiceId {
    pub(crate) fn raw(self) -> u64 {
        self.0
    }
}

/// Distance attenuation curve used by a spatial voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpatialRolloff {
    /// Linearly fades from full gain at the minimum distance to silence at the maximum distance.
    #[default]
    Linear,
    /// Uses a normalized inverse-distance curve while preserving the same exact endpoints.
    Inverse,
}

/// World-space pose used by the spatial mixer for one listener.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioListenerPose {
    /// World-space listener position.
    pub position: [f32; 3],
    /// World-space forward direction. GameEngine uses local `-Z` as forward.
    pub forward: [f32; 3],
    /// World-space up direction. GameEngine uses local `+Y` as up.
    pub up: [f32; 3],
}

impl Default for AudioListenerPose {
    fn default() -> Self {
        Self {
            position: [0.0, 0.0, 0.0],
            forward: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
        }
    }
}

/// World-space pose used by the spatial mixer for one emitter.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AudioEmitterPose {
    /// World-space emitter position.
    pub position: [f32; 3],
}

/// Complete backend-neutral spatial parameters for one playing voice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceSpatialParams {
    /// Current world-space emitter pose.
    pub emitter: AudioEmitterPose,
    /// Current world-space listener pose.
    pub listener: AudioListenerPose,
    /// Per-voice gain before the sound-effect bus and master volume.
    pub volume: f32,
    /// Blend from centered 2D (`0`) to fully positional (`1`).
    pub spatial_blend: f32,
    /// Distance at which attenuation begins.
    pub min_distance: f32,
    /// Distance at which attenuation reaches zero.
    pub max_distance: f32,
    /// Distance attenuation curve.
    pub rolloff: SpatialRolloff,
}

impl Default for VoiceSpatialParams {
    fn default() -> Self {
        Self {
            emitter: AudioEmitterPose::default(),
            listener: AudioListenerPose::default(),
            volume: 1.0,
            spatial_blend: 1.0,
            min_distance: 1.0,
            max_distance: 20.0,
            rolloff: SpatialRolloff::Linear,
        }
    }
}

/// Per-channel gains produced by the engine-owned spatial mixer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StereoGains {
    /// Left output-channel gain.
    pub left: f32,
    /// Right output-channel gain.
    pub right: f32,
}

/// Computes deterministic distance attenuation in the inclusive range `0..=1`.
///
/// Invalid inputs are sanitized to finite non-negative distances. Both curves
/// are exactly `1` at or inside `min_distance` and exactly `0` at or beyond
/// `max_distance`.
pub fn attenuation_gain(
    distance: f32,
    min_distance: f32,
    max_distance: f32,
    rolloff: SpatialRolloff,
) -> f32 {
    let distance = finite_non_negative(distance);
    let min_distance = finite_non_negative(min_distance);
    let max_distance = finite_non_negative(max_distance).max(min_distance);
    if distance <= min_distance {
        return 1.0;
    }
    if distance >= max_distance || max_distance <= min_distance {
        return 0.0;
    }

    let span = max_distance - min_distance;
    let offset = distance - min_distance;
    match rolloff {
        SpatialRolloff::Linear => (1.0 - offset / span).clamp(0.0, 1.0),
        SpatialRolloff::Inverse => {
            // A one-world-unit reference keeps min_distance=0 well-defined.
            // Subtracting and renormalizing the value at max_distance gives
            // the inverse curve the same exact 1 -> 0 endpoints as Linear.
            let reference = min_distance.max(1.0);
            let raw = reference / (reference + offset);
            let floor = reference / (reference + span);
            ((raw - floor) / (1.0 - floor)).clamp(0.0, 1.0)
        }
    }
}

/// Computes equal-power stereo gains for one voice.
///
/// `spatial_blend == 0` is a strict centered-2D bypass: emitter distance and
/// listener orientation cannot change the result. Positional playback derives
/// left/right pan from the listener's world-space orientation and multiplies it
/// by the selected distance rolloff.
pub fn spatial_stereo_gains(params: VoiceSpatialParams) -> StereoGains {
    let volume = finite_unit(params.volume);
    let blend = finite_unit(params.spatial_blend);
    let centered = std::f32::consts::FRAC_1_SQRT_2;
    if blend == 0.0 {
        return StereoGains {
            left: volume * centered,
            right: volume * centered,
        };
    }

    let listener_position = finite_vec3(params.listener.position);
    let emitter_position = finite_vec3(params.emitter.position);
    let to_emitter = sub(emitter_position, listener_position);
    let distance = length(to_emitter);
    let attenuation = attenuation_gain(
        distance,
        params.min_distance,
        params.max_distance,
        params.rolloff,
    );

    let forward = normalize_or(params.listener.forward, [0.0, 0.0, -1.0]);
    let up = normalize_or(params.listener.up, [0.0, 1.0, 0.0]);
    let right = normalize_or(cross(forward, up), [1.0, 0.0, 0.0]);
    let direction = normalize_or(to_emitter, forward);
    let pan = dot(direction, right).clamp(-1.0, 1.0);
    let angle = (pan + 1.0) * FRAC_PI_4;
    let positional_left = angle.cos() * attenuation;
    let positional_right = angle.sin() * attenuation;

    StereoGains {
        left: volume * lerp(centered, positional_left, blend),
        right: volume * lerp(centered, positional_right, blend),
    }
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn finite_vec3(value: [f32; 3]) -> [f32; 3] {
    value.map(|component| if component.is_finite() { component } else { 0.0 })
}

fn sub(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn length(value: [f32; 3]) -> f32 {
    dot(value, value).sqrt()
}

fn normalize_or(value: [f32; 3], fallback: [f32; 3]) -> [f32; 3] {
    let value = finite_vec3(value);
    let length = length(value);
    if length.is_finite() && length > f32::EPSILON {
        [value[0] / length, value[1] / length, value[2] / length]
    } else {
        fallback
    }
}

fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_near(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1.0e-5,
            "expected {expected}, found {actual}"
        );
    }

    #[test]
    fn two_dimensional_blend_is_position_independent() {
        let near = spatial_stereo_gains(VoiceSpatialParams {
            spatial_blend: 0.0,
            emitter: AudioEmitterPose {
                position: [1.0, 2.0, 3.0],
            },
            ..VoiceSpatialParams::default()
        });
        let far = spatial_stereo_gains(VoiceSpatialParams {
            spatial_blend: 0.0,
            emitter: AudioEmitterPose {
                position: [100_000.0, -25.0, 80.0],
            },
            ..VoiceSpatialParams::default()
        });
        assert_eq!(near, far);
    }

    #[test]
    fn listener_orientation_produces_equal_power_right_pan() {
        let gains = spatial_stereo_gains(VoiceSpatialParams {
            emitter: AudioEmitterPose {
                position: [1.0, 0.0, 0.0],
            },
            min_distance: 10.0,
            max_distance: 20.0,
            ..VoiceSpatialParams::default()
        });
        assert_near(gains.left, 0.0);
        assert_near(gains.right, 1.0);
    }

    #[test]
    fn linear_and_inverse_rolloff_share_exact_boundaries() {
        for rolloff in [SpatialRolloff::Linear, SpatialRolloff::Inverse] {
            assert_near(attenuation_gain(1.0, 1.0, 11.0, rolloff), 1.0);
            assert_near(attenuation_gain(11.0, 1.0, 11.0, rolloff), 0.0);
            let middle = attenuation_gain(6.0, 1.0, 11.0, rolloff);
            assert!(middle > 0.0 && middle < 1.0);
        }
    }

    #[test]
    fn intermediate_blend_is_continuous_between_2d_and_positional() {
        let two_d = spatial_stereo_gains(VoiceSpatialParams {
            spatial_blend: 0.0,
            emitter: AudioEmitterPose {
                position: [1.0, 0.0, 0.0],
            },
            min_distance: 10.0,
            max_distance: 20.0,
            ..VoiceSpatialParams::default()
        });
        let three_d = spatial_stereo_gains(VoiceSpatialParams {
            spatial_blend: 1.0,
            emitter: AudioEmitterPose {
                position: [1.0, 0.0, 0.0],
            },
            min_distance: 10.0,
            max_distance: 20.0,
            ..VoiceSpatialParams::default()
        });
        let half = spatial_stereo_gains(VoiceSpatialParams {
            spatial_blend: 0.5,
            emitter: AudioEmitterPose {
                position: [1.0, 0.0, 0.0],
            },
            min_distance: 10.0,
            max_distance: 20.0,
            ..VoiceSpatialParams::default()
        });
        assert_near(half.left, (two_d.left + three_d.left) * 0.5);
        assert_near(half.right, (two_d.right + three_d.right) * 0.5);
    }

    #[test]
    fn invalid_inputs_never_produce_non_finite_gains() {
        let gains = spatial_stereo_gains(VoiceSpatialParams {
            emitter: AudioEmitterPose {
                position: [f32::NAN, f32::INFINITY, f32::NEG_INFINITY],
            },
            listener: AudioListenerPose {
                position: [f32::NAN, 0.0, 0.0],
                forward: [0.0, 0.0, 0.0],
                up: [0.0, 0.0, 0.0],
            },
            volume: f32::NAN,
            spatial_blend: f32::INFINITY,
            min_distance: f32::NAN,
            max_distance: f32::NEG_INFINITY,
            rolloff: SpatialRolloff::Inverse,
        });
        assert!(gains.left.is_finite());
        assert!(gains.right.is_finite());
    }
}
