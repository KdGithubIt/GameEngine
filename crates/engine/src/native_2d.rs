//! Cross-domain Native 2D physics composition (ADR 0127).

pub use engine_animation::sprite_2d::{
    SpriteAnimationRuntimeError, SpriteAnimationState2d, SpriteAnimatorRuntime2d, SpriteFrameEvent2d,
};
pub use engine_assets::native_2d::{
    compile_sprite_atlas, compile_tile_map, compile_tile_set, CompiledSpriteAtlas,
    CompiledSpriteRegion, CompiledTile, CompiledTileChunk, CompiledTileLayer, CompiledTileMap,
    CompiledTileSet, Native2dCompileError,
};
pub use engine_physics::native_2d::*;
pub use engine_render_runtime::native_2d::{
    cull_tile_chunks, sort_and_batch_sprites, validate_camera_transform, Camera2d,
    Camera2dDiagnostic, Native2dRenderMetrics, SpriteBatch2d, SpriteInstance2d,
    TileChunkBounds2d, ViewRect2d, ViewportFit2d, VisibleTileChunk2d,
};
pub use engine_authoring::{
    SpriteBlendMode, SpriteRenderer2d, SpriteRef, TileLayerId, TileMapDocument, TileSetDocument,
};

use crate::transform::{GlobalTransform, Parent, Transform};
use engine_authoring::Project2dSettings;
use engine_ecs::{Entity, Query, Res, ResMut};
use glam::{Quat, Vec2};
use std::collections::BTreeSet;

/// One structured reason an authored Transform could not participate in 2D physics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Physics2dDiagnosticKind {
    /// The effective world transform cannot be represented by the planar contract.
    InvalidPlanarPose(PlanarPoseError),
    /// Dynamic writeback through a parent hierarchy is not silently approximated.
    ParentedDynamicBody,
}

/// Runtime diagnostic for one Native 2D physics entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Physics2dDiagnostic {
    /// Runtime entity carrying the invalid physics state.
    pub entity: Entity,
    /// Structured diagnostic classification.
    pub kind: Physics2dDiagnosticKind,
}

/// Latest fixed-step Native 2D diagnostics.
#[derive(Debug, Default)]
pub struct Physics2dDiagnostics {
    entries: Vec<Physics2dDiagnostic>,
}

impl Physics2dDiagnostics {
    /// Iterates diagnostics emitted by the most recent fixed step.
    pub fn iter(&self) -> impl Iterator<Item = &Physics2dDiagnostic> {
        self.entries.iter()
    }
}

/// Dedicated 2D solver state and latest transition events.
#[derive(Debug, Default)]
pub struct PhysicsRuntime2d {
    world: PhysicsWorld2d,
    events: Vec<ContactEvent2d>,
}

impl PhysicsRuntime2d {
    /// Returns the dedicated 2D world for read-only gameplay queries.
    pub fn world(&self) -> &PhysicsWorld2d {
        &self.world
    }

    /// Returns transition events emitted by the latest fixed step.
    pub fn events(&self) -> &[ContactEvent2d] {
        &self.events
    }
}

/// Applies persisted project 2D settings to one runtime host.
///
/// Editor Play and the packaged Player call this same function after loading
/// [`Project2dSettings`], preventing host-specific gravity interpretation.
pub fn apply_project_2d_settings(app: &mut crate::App, settings: &Project2dSettings) {
    app.insert_resource(Gravity2d(Vec2::new(
        settings.gravity[0] as f32,
        settings.gravity[1] as f32,
    )));
}

fn runtime_key(entity: Entity) -> u64 {
    (u64::from(entity.generation()) << 32) | u64::from(entity.id())
}

type Physics2dQuery<'a> = (
    &'a mut Transform,
    &'a GlobalTransform,
    Option<&'a Parent>,
    Option<&'a mut RigidBody2d>,
    &'a Collider2d,
);

/// Synchronizes ECS components into the dedicated 2D world, steps it, and
/// writes root dynamic poses back through the existing Transform authority.
pub fn physics_2d_fixed_system(
    gravity: Res<Gravity2d>,
    fixed_time: Res<crate::time::FixedTime>,
    mut runtime: ResMut<PhysicsRuntime2d>,
    mut diagnostics: ResMut<Physics2dDiagnostics>,
    mut query: Query<Physics2dQuery<'_>>,
) {
    diagnostics.entries.clear();
    let mut active = BTreeSet::new();

    for (entity, (transform, global, parent, body, collider)) in query.iter_mut() {
        let key = runtime_key(entity);
        let authored_body = body
            .as_deref()
            .copied()
            .unwrap_or_else(RigidBody2d::default);
        if authored_body.mode == RigidBodyMode2d::Dynamic && parent.is_some() {
            diagnostics.entries.push(Physics2dDiagnostic {
                entity,
                kind: Physics2dDiagnosticKind::ParentedDynamicBody,
            });
            continue;
        }
        let matrix = if parent.is_some() {
            global.matrix()
        } else {
            transform.to_matrix()
        };
        let pose = match project_planar_transform(matrix) {
            Ok(pose) => pose,
            Err(error) => {
                diagnostics.entries.push(Physics2dDiagnostic {
                    entity,
                    kind: Physics2dDiagnosticKind::InvalidPlanarPose(error),
                });
                continue;
            }
        };
        runtime.world.upsert(BodyEntry2d {
            entity: key,
            pose,
            body: authored_body,
            collider: collider.clone(),
        });
        active.insert(key);
    }

    runtime.world.retain_entities(&active);
    runtime.events = runtime.world.step(fixed_time.fixed_delta, gravity.0);

    for (entity, (transform, _, parent, body, _)) in query.iter_mut() {
        let Some(body) = body else {
            continue;
        };
        if body.mode != RigidBodyMode2d::Dynamic || parent.is_some() {
            continue;
        }
        let Some(resolved) = runtime.world.body(runtime_key(entity)) else {
            continue;
        };
        transform.translation.x = resolved.pose.translation.x;
        transform.translation.y = resolved.pose.translation.y;
        transform.rotation = Quat::from_rotation_z(resolved.pose.rotation);
        body.velocity = resolved.body.velocity;
        body.angular_velocity = resolved.body.angular_velocity;
    }
}

/// One named frame event emitted by SpriteAnimator2D in the current fixed step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpriteAnimationEvent2d {
    /// Runtime entity whose playback entered the frame.
    pub entity: Entity,
    /// Stable Sprite Animation asset used by the animator.
    pub clip: engine_authoring::AssetId,
    /// Frame index entered by deterministic playback.
    pub frame_index: usize,
    /// Authored event name.
    pub name: String,
}

/// Fixed-step Sprite Animation event stream visible to later gameplay systems.
#[derive(Debug, Default)]
pub struct SpriteAnimationEvents2d {
    events: Vec<SpriteAnimationEvent2d>,
}

impl SpriteAnimationEvents2d {
    /// Iterates events emitted by the most recent SpriteAnimator2D evaluation.
    pub fn iter(&self) -> impl Iterator<Item = &SpriteAnimationEvent2d> {
        self.events.iter()
    }
}

/// Advances per-entity Sprite Animation state and writes only the current SpriteRef to rendering.
pub fn sprite_animation_2d_fixed_system(
    fixed_time: Res<crate::time::FixedTime>,
    mut events: ResMut<SpriteAnimationEvents2d>,
    mut query: Query<(&mut SpriteAnimatorRuntime2d, &mut SpriteRenderer2d)>,
) {
    events.events.clear();
    let seconds = f64::from(fixed_time.fixed_delta.max(0.0));
    for (entity, (animator, renderer)) in query.iter_mut() {
        let clip = animator.clip.clone();
        let emitted = animator
            .state
            .advance_fixed_seconds(clip.as_ref(), seconds, animator.looping_override);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_settings_apply_same_typed_gravity_resource() {
        let mut app = crate::App::new();
        let settings = Project2dSettings {
            gravity: [2.5, -7.0],
            ..Project2dSettings::default()
        };
        apply_project_2d_settings(&mut app, &settings);
        assert_eq!(
            app.world().get_resource::<Gravity2d>().unwrap().0,
            Vec2::new(2.5, -7.0)
        );
    }
}
