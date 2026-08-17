//! Deterministic Sprite Animation playback (ADR 0127).

use engine_authoring::{SpriteAnimationDocument, SpriteRef};

/// Independent per-entity runtime playback state for an immutable Sprite Animation asset.
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteAnimationState2d {
    /// Whether this playback instance currently advances.
    pub playing: bool,
    /// Current frame index in the immutable clip.
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
    /// Resumes deterministic playback from the current frame and tick.
    pub fn play(&mut self) {
        self.playing = true;
    }

    /// Pauses playback without changing the current frame or tick.
    pub fn pause(&mut self) {
        self.playing = false;
    }

    /// Stops playback and rewinds to the beginning of the clip.
    pub fn stop(&mut self) {
        self.playing = false;
        self.frame_index = 0;
        self.tick_in_frame = 0;
        self.fractional_ticks = 0.0;
    }
    /// Advances from elapsed seconds by converting to the clip's exact integer tick domain.
    pub fn advance(&mut self, clip: &SpriteAnimationDocument, seconds: f64, looping_override: Option<bool>) -> Vec<SpriteFrameEvent2d> {
        if !self.playing || clip.frames.is_empty() || clip.ticks_per_second == 0 || seconds <= 0.0 || !seconds.is_finite() { return Vec::new(); }
        let scaled = seconds * f64::from(clip.ticks_per_second) * f64::from(self.speed.max(0.0));
        self.fractional_ticks += scaled;
        let whole = self.fractional_ticks.floor() as u64;
        self.fractional_ticks -= whole as f64;
        let looping = looping_override.unwrap_or(clip.looping);
        let mut events = Vec::new();
        for _ in 0..whole {
            let duration = clip.frames[self.frame_index].duration_ticks.max(1);
            self.tick_in_frame += 1;
            if self.tick_in_frame >= duration {
                self.tick_in_frame = 0;
                if self.frame_index + 1 < clip.frames.len() { self.frame_index += 1; } else if looping { self.frame_index = 0; } else { self.frame_index = clip.frames.len() - 1; self.playing = false; break; }
                if let Some(name) = &clip.frames[self.frame_index].event { events.push(SpriteFrameEvent2d { frame_index: self.frame_index, name: name.clone() }); }
            }
        }
        events
    }
    /// Returns the sprite referenced by the current playback frame.
    pub fn current_sprite<'a>(
        &self,
        clip: &'a SpriteAnimationDocument,
    ) -> Option<&'a SpriteRef> {
        clip.frames.get(self.frame_index).map(|frame| &frame.sprite)
    }
}

#[cfg(test)] mod tests { use super::*; use engine_authoring::{AssetId, SpriteAnimationFrame, SpriteId};
    fn clip() -> SpriteAnimationDocument { let atlas=AssetId::generate(); SpriteAnimationDocument { schema_version:1,ticks_per_second:60,looping:true,frames:vec![SpriteAnimationFrame{sprite:SpriteRef{atlas:atlas.clone(),sprite:SpriteId::generate()},duration_ticks:2,event:None},SpriteAnimationFrame{sprite:SpriteRef{atlas,sprite:SpriteId::generate()},duration_ticks:2,event:Some("step".into())}] } }
    #[test] fn shared_clip_has_independent_state(){let clip=clip();let mut a=SpriteAnimationState2d::default();let b=SpriteAnimationState2d::default();a.advance(&clip,2.0/60.0,None);assert_eq!(a.frame_index,1);assert_eq!(b.frame_index,0);}
}
