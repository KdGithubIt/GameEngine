//! Cross-domain Native 2D facade (ADR 0127).

pub use engine_animation::sprite_2d::{SpriteAnimationState2d, SpriteFrameEvent2d};
pub use engine_assets::native_2d::{compile_tile_map, validate_2d_asset_references, CompiledSpriteAtlas, CompiledSpriteRegion, CompiledTile, CompiledTileChunk, CompiledTileMap};
pub use engine_authoring::native_2d::*;
pub use engine_physics::native_2d::{project_planar_transform, BodyEntry2d, CharacterController2d, Collider2d, ColliderShape2d, CollisionFilter2d, ContactEvent2d, ContactPhase2d, Joint2d, PhysicsWorld2d, PlanarPose2d, PlanarPoseError, QueryHit2d, RigidBody2d, RigidBodyMode2d};
pub use engine_render_runtime::native_2d::{cull_tile_chunks, select_active_camera, sort_and_batch_sprites, validate_camera_pose, ActiveCameraCandidate, ActiveCameraSelection, Camera2d, Camera2dDiagnostic, CameraKind, SpriteBatch2d, SpriteInstance2d, ViewRect2d, ViewportFit2d};

/// Performance counters recorded by the proving scenario and Editor diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Native2dPerformanceStats { pub sprite_count:u32, pub sprite_batches:u32, pub visible_tile_chunks:u32, pub rebuilt_tile_chunks:u32, pub tile_edit_millis:f32, pub physics_step_millis:f32 }

/// First-release acceptance budget used by the proving project.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Native2dAcceptanceBudget { pub max_sprite_batches:u32, pub max_rebuilt_chunks_per_stroke:u32, pub max_tile_edit_millis:f32, pub max_physics_step_millis:f32 }
impl Native2dAcceptanceBudget { pub fn validate(self,stats:Native2dPerformanceStats)->Vec<&'static str>{let mut out=Vec::new();if stats.sprite_batches>self.max_sprite_batches{out.push("sprite batch budget exceeded");}if stats.rebuilt_tile_chunks>self.max_rebuilt_chunks_per_stroke{out.push("tile chunk rebuild budget exceeded");}if stats.tile_edit_millis>self.max_tile_edit_millis{out.push("tile edit latency budget exceeded");}if stats.physics_step_millis>self.max_physics_step_millis{out.push("2D physics step budget exceeded");}out}}
