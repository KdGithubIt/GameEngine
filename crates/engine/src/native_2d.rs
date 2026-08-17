//! Cross-domain Native 2D facade (ADR 0127).

pub use engine_animation::sprite_2d::{
    SpriteAnimationState2d, SpriteAnimatorRuntime2d, SpriteFrameEvent2d,
};
pub use engine_assets::native_2d::{compile_tile_map, validate_2d_asset_references, CompiledSpriteAtlas, CompiledSpriteRegion, CompiledTile, CompiledTileChunk, CompiledTileMap};
pub use engine_authoring::native_2d::*;
pub use engine_physics::native_2d::{project_planar_transform, BodyEntry2d, CharacterController2d, Collider2d, ColliderShape2d, CollisionFilter2d, ContactEvent2d, ContactPhase2d, Joint2d, PhysicsWorld2d, PlanarPose2d, PlanarPoseError, QueryHit2d, RigidBody2d, RigidBodyMode2d};
pub use engine_render_runtime::native_2d::{cull_tile_chunks, select_active_camera, sort_and_batch_sprites, validate_camera_pose, ActiveCameraCandidate, ActiveCameraSelection, Camera2d, Camera2dDiagnostic, CameraKind, SpriteBatch2d, SpriteInstance2d, ViewRect2d, ViewportFit2d};

use crate::time::FixedTime;
use engine_authoring::AssetId;
use engine_ecs::{Entity, Query, Res, ResMut};

/// One frame event emitted by SpriteAnimator2D during the current fixed step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpriteAnimationEvent2d {
    /// Runtime entity whose animator entered the frame.
    pub entity: Entity,
    /// Stable Sprite Animation asset used by the animator.
    pub clip: AssetId,
    /// Frame index entered by deterministic playback.
    pub frame_index: usize,
    /// Authored frame-event name.
    pub name: String,
}

/// Fixed-step Sprite Animation events visible to later gameplay systems in the same step.
#[derive(Debug, Default)]
pub struct SpriteAnimationEvents2d {
    events: Vec<SpriteAnimationEvent2d>,
}

impl SpriteAnimationEvents2d {
    /// Returns events emitted by the most recent SpriteAnimator2D fixed-step evaluation.
    pub fn iter(&self) -> impl Iterator<Item = &SpriteAnimationEvent2d> {
        self.events.iter()
    }
}

/// Advances SpriteAnimator2D instances and writes only the current SpriteRef into SpriteRenderer2D.
pub fn sprite_animation_2d_fixed_system(
    fixed_time: Res<FixedTime>,
    mut events: ResMut<SpriteAnimationEvents2d>,
    mut query: Query<(&mut SpriteAnimatorRuntime2d, &mut SpriteRenderer2d)>,
) {
    events.events.clear();
    let seconds = f64::from(fixed_time.fixed_delta.max(0.0));
    for (entity, (animator, renderer)) in &mut query {
        let clip = std::sync::Arc::clone(&animator.clip);
        let looping_override = animator.looping_override;
        let emitted = animator
            .state
            .advance(clip.as_ref(), seconds, looping_override);
        if let Some(sprite) = animator.state.current_sprite(clip.as_ref()) {
            renderer.sprite = sprite.clone();
        }
        events.events.extend(emitted.into_iter().map(|event| SpriteAnimationEvent2d {
            entity,
            clip: animator.clip_asset.clone(),
            frame_index: event.frame_index,
            name: event.name,
        }));
    }
}

/// Performance counters recorded by the proving scenario and Editor diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Native2dPerformanceStats {
    /// Number of visible sprite instances in the measured frame.
    pub sprite_count: u32,
    /// Number of contiguous sprite batches submitted in the measured frame.
    pub sprite_batches: u32,
    /// Number of Tile Map chunks visible to the active Camera2D.
    pub visible_tile_chunks: u32,
    /// Number of Tile Map chunks rebuilt by the measured authoring gesture.
    pub rebuilt_tile_chunks: u32,
    /// End-to-end Tile Map gesture latency in milliseconds.
    pub tile_edit_millis: f32,
    /// Dedicated 2D fixed-step simulation time in milliseconds.
    pub physics_step_millis: f32,
}

/// First-release acceptance budget used by the proving project.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Native2dAcceptanceBudget {
    /// Maximum accepted sprite batches for the proving frame.
    pub max_sprite_batches: u32,
    /// Maximum chunks allowed to rebuild for one bounded tile stroke.
    pub max_rebuilt_chunks_per_stroke: u32,
    /// Maximum accepted Tile Map edit latency in milliseconds.
    pub max_tile_edit_millis: f32,
    /// Maximum accepted 2D physics step time in milliseconds.
    pub max_physics_step_millis: f32,
}

impl Native2dAcceptanceBudget {
    /// Returns every acceptance-budget violation represented by `stats`.
    pub fn validate(self, stats: Native2dPerformanceStats) -> Vec<&'static str> {
        let mut out = Vec::new();
        if stats.sprite_batches > self.max_sprite_batches {
            out.push("sprite batch budget exceeded");
        }
        if stats.rebuilt_tile_chunks > self.max_rebuilt_chunks_per_stroke {
            out.push("tile chunk rebuild budget exceeded");
        }
        if stats.tile_edit_millis > self.max_tile_edit_millis {
            out.push("tile edit latency budget exceeded");
        }
        if stats.physics_step_millis > self.max_physics_step_millis {
            out.push("2D physics step budget exceeded");
        }
        out
    }
}
