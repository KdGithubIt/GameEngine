//! Deterministic per-entity Sprite Animation playback (ADR 0127).

use engine_authoring::{AssetId, SpriteAnimationDocument, SpriteRef};
use std::fmt;
use std::sync::Arc;

/// Runtime SpriteAnimator2D component sharing immutable clip data.
#[derive(Debug, Clone)]
pub struct SpriteAnimatorRuntime2d {
    /// Stable authored Sprite Animation asset identity.
    pub clip_asset: AssetId,
    /// Shared immutable clip resolved during scene conversion.
    pub clip: Arc<SpriteAnimationDocument>,
    /// Independent playback state owned by this entity.
    pub state: SpriteAnimationState2d,
    /// Optional per-instance looping override.
    pub looping_override: Option<bool>,
}

/// Sprite Animation runtime construction failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpriteAnimationRuntimeError {
    /// The clip failed its persisted semantic validation.
    InvalidClip(Vec<String>),
    /// The requested initial frame was outside the immutable frame sequence.
    InitialFrameOutOfRange {
        /// Requested zero-based frame index.
        frame: usize,
        /// Number of frames in the clip.
        frame_count: usize,
    },
    /// A per-instance speed was negative or non-finite.
    InvalidSpeed,
}

impl fmt::Display for SpriteAnimationRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidClip(errors) => write!(formatter, "invalid Sprite Animation clip: {}", errors.join("; ")),
            Self::InitialFrameOutOfRange { frame, frame_count } => write!(formatter, "initial Sprite Animation frame {frame} is outside {frame_count} frames"),
            Self::InvalidSpeed => formatter.write_str("Sprite Animation speed must be finite and non-negative"),
        }
    }
}

impl std::error::Error for SpriteAnimationRuntimeError {}

impl SpriteAnimatorRuntime2d {
    /// Creates one runtime animator from persisted playback settings.
    pub fn new(
        clip_asset: AssetId,
        clip: Arc<SpriteAnimationDocument>,
        autoplay: bool,
        speed: f32,
        looping_override: Option<bool>,
        initial_frame: usize,
    ) -> Result<Self, SpriteAnimationRuntimeError> {
        let errors = clip.validate();
        if !errors.is_empty() {
            return Err(SpriteAnimationRuntimeError::InvalidClip(errors));
        }
        if !speed.is_finite() || speed < 0.0 {
            return Err(SpriteAnimationRuntimeError::InvalidSpeed);
        }
        if initial_frame >= clip.frames.len() {
            return Err(SpriteAnimationRuntimeError::InitialFrameOutOfRange {
                frame: initial_frame,
                frame_count: clip.frames.len(),
            });
        }
        Ok(Self {
            clip_asset,
            state: SpriteAnimationState2d {
                playing: autoplay,
                frame_index: initial_frame,
                tick_in_frame: 0,
                speed,
                fractional_ticks: 0.0,
            },
            clip,
            looping_override,
        })
    }

    /// Replaces the immutable clip and resets independent state to a valid frame.
    pub fn select_clip(
        &mut self,
        clip_asset: AssetId,
        clip: Arc<SpriteAnimationDocument>,
        initial_frame: usize,
    ) -> Result<(), SpriteAnimationRuntimeError> {
        let errors = clip.validate();
        if !errors.is_empty() {
            return Err(SpriteAnimationRuntimeError::InvalidClip(errors));
        }
        if initial_frame >= clip.frames.len() {
            return Err(SpriteAnimationRuntimeError::InitialFrameOutOfRange {
                frame: initial_frame,
                frame_count: clip.frames.len(),
            });
        }
        self.clip_asset = clip_asset;
        self.clip = clip;
        self.state.frame_index = initial_frame;
        self.state.tick_in_frame = 0;
        self.state.fractional_ticks = 0.0;
        Ok(())
    }
}

/// Independent per-entity runtime playback state for an immutable clip.
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteAnimationState2d {
    /// Whether this playback instance currently advances.
    pub playing: bool,
    /// Current frame index.
    pub frame_index: usize,
    /// Integer ticks already consumed inside the current frame.
    pub tick_in_frame: u32,
    /// Non-negative per-instance playback speed multiplier.
    pub speed: f32,
    fractional_ticks: f64,
}

impl Default for SpriteAnimationState2d {
    fn default() -> Self {
        Self {
            playing: true,
            frame_index: 0,
            tick_in_frame: 0,
            speed: 1.0,
            fractional_ticks: 0.0,
        }
    }
}

/// Frame event emitted by exact integer-tick progression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpriteFrameEvent2d {
    /// Frame index entered when the event was emitted.
    pub frame_index: usize,
    /// Authored event name.
    pub name: String,
}

impl SpriteAnimationState2d {
    /// Resumes playback from the current frame and tick.
    pub fn play(&mut self) {
        self.playing = true;
    }

    /// Pauses playback without changing current frame/tick.
    pub fn pause(&mut self) {
        self.playing = false;
    }

    /// Stops playback and rewinds to frame zero.
    pub fn stop(&mut self) {
        self.playing = false;
        self.frame_index = 0;
        self.tick_in_frame = 0;
        self.fractional_ticks = 0.0;
    }

    /// Sets a finite non-negative instance speed.
    pub fn set_speed(&mut self, speed: f32) -> Result<(), SpriteAnimationRuntimeError> {
        if !speed.is_finite() || speed < 0.0 {
            return Err(SpriteAnimationRuntimeError::InvalidSpeed);
        }
        self.speed = speed;
        Ok(())
    }

    /// Advances by an exact whole number of clip ticks.
    ///
    /// This is the canonical deterministic progression path. Fixed-step hosts
    /// should accumulate their time conversion per entity and call this method.
    pub fn advance_ticks(
        &mut self,
        clip: &SpriteAnimationDocument,
        ticks: u64,
        looping_override: Option<bool>,
    ) -> Vec<SpriteFrameEvent2d> {
        if !self.playing || clip.frames.is_empty() || ticks == 0 {
            return Vec::new();
        }
        let looping = looping_override.unwrap_or(clip.looping);
        let mut events = Vec::new();
        for _ in 0..ticks {
            let duration = clip.frames[self.frame_index].duration_ticks.max(1);
            self.tick_in_frame += 1;
            if self.tick_in_frame < duration {
                continue;
            }
            self.tick_in_frame = 0;
            if self.frame_index + 1 < clip.frames.len() {
                self.frame_index += 1;
            } else if looping {
                self.frame_index = 0;
            } else {
                self.frame_index = clip.frames.len() - 1;
                self.playing = false;
                break;
            }
            if let Some(name) = &clip.frames[self.frame_index].event {
                events.push(SpriteFrameEvent2d { frame_index: self.frame_index, name: name.clone() });
            }
        }
        events
    }

    /// Converts one fixed-step duration to the clip integer tick domain.
    ///
    /// Fractional ticks stay private to the entity, so entities sharing the same
    /// immutable clip never share mutable playback timing.
    pub fn advance_fixed_seconds(
        &mut self,
        clip: &SpriteAnimationDocument,
        seconds: f64,
        looping_override: Option<bool>,
    ) -> Vec<SpriteFrameEvent2d> {
        if !self.playing
            || clip.ticks_per_second == 0
            || !seconds.is_finite()
            || seconds <= 0.0
            || !self.speed.is_finite()
            || self.speed <= 0.0
        {
            return Vec::new();
        }
        let scaled = seconds
            * f64::from(clip.ticks_per_second)
            * f64::from(clip.default_speed)
            * f64::from(self.speed);
        self.fractional_ticks += scaled;
        let whole = self.fractional_ticks.floor() as u64;
        self.fractional_ticks -= whole as f64;
        self.advance_ticks(clip, whole, looping_override)
    }

    /// Returns the SpriteRef displayed by the current frame.
    pub fn current_sprite<'a>(&self, clip: &'a SpriteAnimationDocument) -> Option<&'a SpriteRef> {
        clip.frames.get(self.frame_index).map(|frame| &frame.sprite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{
        SpriteAnimationFrame, SpriteId, SPRITE_ANIMATION_SCHEMA_VERSION,
    };

    fn clip() -> SpriteAnimationDocument {
        let atlas = AssetId::generate();
        SpriteAnimationDocument {
            schema_version: SPRITE_ANIMATION_SCHEMA_VERSION,
            ticks_per_second: 60,
            looping: true,
            default_speed: 1.0,
            frames: vec![
                SpriteAnimationFrame {
                    sprite: SpriteRef { atlas: atlas.clone(), sprite: SpriteId::generate() },
                    duration_ticks: 2,
                    event: None,
                },
                SpriteAnimationFrame {
                    sprite: SpriteRef { atlas, sprite: SpriteId::generate() },
                    duration_ticks: 2,
                    event: Some("step".to_owned()),
                },
            ],
        }
    }

    #[test]
    fn shared_clip_keeps_independent_entity_state() {
        let clip = Arc::new(clip());
        let mut a = SpriteAnimatorRuntime2d::new(AssetId::generate(), Arc::clone(&clip), true, 1.0, None, 0).unwrap();
        let b = SpriteAnimatorRuntime2d::new(AssetId::generate(), Arc::clone(&clip), true, 1.0, None, 0).unwrap();
        let events = a.state.advance_ticks(&clip, 2, None);
        assert_eq!(a.state.frame_index, 1);
        assert_eq!(b.state.frame_index, 0);
        assert_eq!(events[0].name, "step");
    }

    #[test]
    fn fixed_step_fractional_conversion_matches_one_exact_tick() {
        let clip = clip();
        let mut state = SpriteAnimationState2d::default();
        state.advance_fixed_seconds(&clip, 1.0 / 120.0, None);
        assert_eq!(state.tick_in_frame, 0);
        state.advance_fixed_seconds(&clip, 1.0 / 120.0, None);
        assert_eq!(state.tick_in_frame, 1);
    }
}
