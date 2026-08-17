//! Backend-neutral spatial-audio contracts and deterministic mixer math.
//!
//! This module contains no platform, scene, transform, or audio-device types.
//! The final engine composition layer extracts world-space poses into these
//! copied values, while the native audio backend consumes only the resulting
//! runtime voice identifier and stereo gains.

/// Process-local identifier for one managed sound-effect voice.
///
/// The identifier is valid only for the lifetime of the owning audio backend.
/// It must never be serialized into authoring data or exposed as a persisted ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioVoiceId(pub(crate) u64);

/// Distance attenuation curve used by positional sound-effect voices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AudioRolloffMode {
    /// Falls linearly from full gain at `min_distance` to silence at `max_distance`.
    #[default]
    Linear,
    /// Uses an inverse-distance shape normalized to the same two endpoints.
    Inverse,
}

/// Backend-neutral world-space listener pose used by spatial mixing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioListenerPose {
    /// Listener world-space position.
    pub position: [f32; 3],
    /// Listener world-space right direction used to derive stereo pan.
    pub right: [f32; 3],
}

/// Backend-neutral world-space emitter pose used by spatial mixing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioEmitterPose {
    /// Emitter world-space position.
    pub position: [f32; 3],
}

/// Spatial settings applied to one active sound-effect voice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioVoiceSpatialSettings {
    /// Per-emitter linear gain before the sound-effect bus and master gain.
    pub volume: f32,
    /// Blend from fully 2D (`0.0`) to fully positional (`1.0`).
    pub spatial_blend: f32,
    /// Distance at which attenuation begins.
    pub min_distance: f32,
    /// Distance at which attenuation reaches silence.
    pub max_distance: f32,
    /// Distance attenuation curve.
    pub rolloff: AudioRolloffMode,
}

/// Per-channel linear gain produced by the engine-owned spatial mixer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StereoGains {
    /// Left-channel linear gain.
    pub left: f32,
    /// Right-channel linear gain.
    pub right: f32,
}

impl StereoGains {
    /// Returns silence on both channels.
    pub const fn silent() -> Self {
        Self {
            left: 0.0,
            right: 0.0,
        }
    }
}

/// Computes finite stereo gains from copied listener/emitter poses and settings.
///
/// `spatial_blend == 0.0` is exactly 2D: pan and distance attenuation are
/// bypassed. Fully positional audio uses equal-power stereo pan. Invalid
/// non-finite inputs are sanitized to a finite result so a transient runtime
/// anomaly cannot inject NaNs into the audio backend; authoring validation is
/// still responsible for reporting invalid persisted values.
pub fn spatial_stereo_gains(
    listener: AudioListenerPose,
    emitter: AudioEmitterPose,
    settings: AudioVoiceSpatialSettings,
) -> StereoGains {
    let volume = finite_unit(settings.volume);
    let blend = finite_unit(settings.spatial_blend);
    if blend == 0.0 {
        return StereoGains {
            left: volume,
            right: volume,
        };
    }

    let offset = subtract(emitter.position, listener.position);
    let distance = length(offset);
    let attenuation = distance_attenuation(
        distance,
        settings.min_distance,
        settings.max_distance,
        settings.rolloff,
    );
    let direction = normalize(offset).unwrap_or([0.0, 0.0, 0.0]);
    let right = normalize(listener.right).unwrap_or([1.0, 0.0, 0.0]);
    let pan = dot(direction, right).clamp(-1.0, 1.0);
    let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
    let positional_left = angle.cos() * attenuation;
    let positional_right = angle.sin() * attenuation;

    StereoGains {
        left: volume * lerp(1.0, positional_left, blend),
        right: volume * lerp(1.0, positional_right, blend),
    }
}

/// Computes the finite distance attenuation for one rolloff mode.
///
/// Both supported curves return `1.0` at or before `min_distance` and `0.0`
/// at or beyond `max_distance`. A non-finite distance is treated as inaudible.
/// Invalid distance bounds collapse to a finite step rather than returning NaN;
/// persisted authoring values are rejected separately by schema validation.
pub fn distance_attenuation(
    distance: f32,
    min_distance: f32,
    max_distance: f32,
    rolloff: AudioRolloffMode,
) -> f32 {
    if !distance.is_finite() {
        return 0.0;
    }
    let distance = distance.max(0.0);
    let min_distance = finite_non_negative(min_distance);
    let max_distance = if max_distance.is_finite() {
        max_distance.max(min_distance)
    } else {
        min_distance
    };

    if distance <= min_distance {
        return 1.0;
    }
    if distance >= max_distance || max_distance <= min_distance {
        return 0.0;
    }

    let progress = (distance - min_distance) / (max_distance - min_distance);
    match rolloff {
        AudioRolloffMode::Linear => 1.0 - progress,
        AudioRolloffMode::Inverse => {
            if min_distance <= f32::EPSILON {
                1.0 - progress
            } else {
                let raw = min_distance / distance;
                let floor = min_distance / max_distance;
                ((raw - floor) / (1.0 - floor)).clamp(0.0, 1.0)
            }
        }
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

fn subtract(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[0] - right[0],
        left[1] - right[1],
        left[2] - right[2],
    ]
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn length(value: [f32; 3]) -> f32 {
    dot(value, value).sqrt()
}

fn normalize(value: [f32; 3]) -> Option<[f32; 3]> {
    let length = length(value);
    if !length.is_finite() || length <= f32::EPSILON {
        return None;
    }
    Some([value[0] / length, value[1] / length, value[2] / length])
}

fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener() -> AudioListenerPose {
        AudioListenerPose {
            position: [0.0, 0.0, 0.0],
            right: [1.0, 0.0, 0.0],
        }
    }

    fn settings(rolloff: AudioRolloffMode) -> AudioVoiceSpatialSettings {
        AudioVoiceSpatialSettings {
            volume: 1.0,
            spatial_blend: 1.0,
            min_distance: 1.0,
            max_distance: 10.0,
            rolloff,
        }
    }

    #[test]
    fn two_dimensional_mix_bypasses_pan_and_attenuation_exactly() {
        let gains = spatial_stereo_gains(
            listener(),
            AudioEmitterPose {
                position: [100.0, 0.0, 0.0],
            },
            AudioVoiceSpatialSettings {
                volume: 0.75,
                spatial_blend: 0.0,
                min_distance: 1.0,
                max_distance: 2.0,
                rolloff: AudioRolloffMode::Inverse,
            },
        );

        assert_eq!(
            gains,
            StereoGains {
                left: 0.75,
                right: 0.75,
            }
        );
    }

    #[test]
    fn positional_pan_is_equal_power_at_center_and_hard_sides() {
        let mut settings = settings(AudioRolloffMode::Linear);
        settings.min_distance = 10.0;
        settings.max_distance = 20.0;
        let center = spatial_stereo_gains(
            listener(),
            AudioEmitterPose {
                position: [0.0, 0.0, -1.0],
            },
            settings,
        );
        let right = spatial_stereo_gains(
            listener(),
            AudioEmitterPose {
                position: [1.0, 0.0, 0.0],
            },
            settings,
        );
        let left = spatial_stereo_gains(
            listener(),
            AudioEmitterPose {
                position: [-1.0, 0.0, 0.0],
            },
            settings,
        );

        let center_gain = std::f32::consts::FRAC_1_SQRT_2;
        assert!((center.left - center_gain).abs() < 1.0e-6);
        assert!((center.right - center_gain).abs() < 1.0e-6);
        assert!(right.left.abs() < 1.0e-6);
        assert!((right.right - 1.0).abs() < 1.0e-6);
        assert!((left.left - 1.0).abs() < 1.0e-6);
        assert!(left.right.abs() < 1.0e-6);
    }

    #[test]
    fn moving_listener_changes_pan_for_a_stationary_emitter() {
        let mut settings = settings(AudioRolloffMode::Linear);
        settings.min_distance = 10.0;
        settings.max_distance = 20.0;
        let emitter = AudioEmitterPose { position: [1.0, 0.0, 0.0] };
        let from_left = spatial_stereo_gains(
            AudioListenerPose { position: [0.0, 0.0, 0.0], right: [1.0, 0.0, 0.0] },
            emitter,
            settings,
        );
        let from_right = spatial_stereo_gains(
            AudioListenerPose { position: [2.0, 0.0, 0.0], right: [1.0, 0.0, 0.0] },
            emitter,
            settings,
        );
        assert!(from_left.right > from_left.left);
        assert!(from_right.left > from_right.right);
    }

    #[test]
    fn linear_and_inverse_rolloff_share_finite_endpoints() {
        for rolloff in [AudioRolloffMode::Linear, AudioRolloffMode::Inverse] {
            assert_eq!(distance_attenuation(0.0, 1.0, 10.0, rolloff), 1.0);
            assert_eq!(distance_attenuation(1.0, 1.0, 10.0, rolloff), 1.0);
            assert_eq!(distance_attenuation(10.0, 1.0, 10.0, rolloff), 0.0);
            let middle = distance_attenuation(5.0, 1.0, 10.0, rolloff);
            assert!(middle.is_finite());
            assert!((0.0..=1.0).contains(&middle));
        }
    }

    #[test]
    fn spatial_mix_sanitizes_non_finite_inputs() {
        let gains = spatial_stereo_gains(
            AudioListenerPose {
                position: [f32::NAN, 0.0, 0.0],
                right: [f32::INFINITY, 0.0, 0.0],
            },
            AudioEmitterPose {
                position: [0.0, 0.0, 0.0],
            },
            AudioVoiceSpatialSettings {
                volume: f32::NAN,
                spatial_blend: 1.0,
                min_distance: f32::NAN,
                max_distance: f32::INFINITY,
                rolloff: AudioRolloffMode::Inverse,
            },
        );

        assert!(gains.left.is_finite());
        assert!(gains.right.is_finite());
        assert_eq!(gains, StereoGains::silent());
    }
}
