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
    Camera2dDiagnostic, Native2dRenderMetrics, ResolvedSpriteRegion2d, ResolvedTileCell2d,
    ResolvedTileChunkRender2d, ResolvedTileMap2d, SpriteBatch2d, SpriteInstance2d,
    TileChunkBounds2d, TileMap2d, ViewRect2d, ViewportFit2d, VisibleTileChunk2d,
};
pub use engine_authoring::{
    SpriteBlendMode, SpriteRenderer2d, SpriteRef, TileLayerId, TileMapDocument, TileSetDocument,
};

use crate::transform::{GlobalTransform, Parent, Transform};
use engine_authoring::{
    AssetId, Project2dSettings, SpriteAnimationDocument, TileChunkCoord, TileCollisionShape,
};
use engine_ecs::{Entity, Query, Res, ResMut};
use glam::{Quat, Vec2};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

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

/// Compiled backend-neutral Tile Map collision source attached during scene conversion.
#[derive(Debug, Clone)]
pub struct TileMapPhysicsSource2d {
    pub(crate) map: Arc<CompiledTileMap>,
    pub(crate) tile_set: Arc<CompiledTileSet>,
}

/// Applies persisted project 2D settings to one runtime host.
///
/// Editor Play and the packaged Player call this same function after loading
/// [`Project2dSettings`], preventing host-specific gravity interpretation.
pub fn apply_project_2d_settings(app: &mut crate::App, settings: &Project2dSettings) {
    app.insert_resource(settings.clone());
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

type TilePhysics2dQuery<'a> = (&'a TileMapPhysicsSource2d, &'a GlobalTransform);

fn tile_static_chunk_key(owner: u64, layer: &TileLayerId, coord: TileChunkCoord) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in owner
        .to_le_bytes()
        .into_iter()
        .chain(layer.as_str().bytes())
        .chain(coord.x.to_le_bytes())
        .chain(coord.y.to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn tile_collider_shape_2d(shape: &TileCollisionShape) -> ColliderShape2d {
    match shape {
        TileCollisionShape::Box { half_extents } => ColliderShape2d::Box {
            half_extents: *half_extents,
        },
        TileCollisionShape::Circle { radius } => ColliderShape2d::Circle { radius: *radius },
        TileCollisionShape::Polygon { points } => ColliderShape2d::Polygon {
            points: points.clone(),
        },
    }
}

/// Synchronizes ECS components into the dedicated 2D world, steps it, and
/// writes root dynamic poses back through the existing Transform authority.
pub fn physics_2d_fixed_system(
    gravity: Res<Gravity2d>,
    fixed_time: Res<crate::time::FixedTime>,
    mut runtime: ResMut<PhysicsRuntime2d>,
    mut diagnostics: ResMut<Physics2dDiagnostics>,
    mut query: Query<Physics2dQuery<'_>>,
    tile_maps: Query<TilePhysics2dQuery<'_>>,
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

    let mut active_static_chunks = BTreeSet::new();
    for (entity, (source, global)) in tile_maps.iter() {
        let owner = runtime_key(entity);
        let map_matrix = global.matrix();
        if let Err(error) = project_planar_transform(map_matrix) {
            diagnostics.entries.push(Physics2dDiagnostic {
                entity,
                kind: Physics2dDiagnosticKind::InvalidPlanarPose(error),
            });
            continue;
        }
        let chunk_size = i64::from(source.map.chunk_size.max(1));
        for chunk in &source.map.chunks {
            let enabled = source
                .map
                .layers
                .iter()
                .find(|layer| layer.id == chunk.layer)
                .is_some_and(|layer| layer.enabled);
            if !enabled {
                continue;
            }
            let key = tile_static_chunk_key(owner, &chunk.layer, chunk.coord);
            let mut colliders = Vec::new();
            for (local_x, local_y, tile_id) in &chunk.cells {
                let Some(tile) = source.tile_set.tile(tile_id) else {
                    continue;
                };
                if tile.collision.is_empty() {
                    continue;
                }
                let cell_x = i64::from(chunk.coord.x) * chunk_size + i64::from(*local_x);
                let cell_y = i64::from(chunk.coord.y) * chunk_size + i64::from(*local_y);
                let cell_model = map_matrix
                    * glam::Mat4::from_translation(glam::Vec3::new(
                        cell_x as f32 + 0.5,
                        cell_y as f32 + 0.5,
                        0.0,
                    ));
                let pose = match project_planar_transform(cell_model) {
                    Ok(pose) => pose,
                    Err(error) => {
                        diagnostics.entries.push(Physics2dDiagnostic {
                            entity,
                            kind: Physics2dDiagnosticKind::InvalidPlanarPose(error),
                        });
                        continue;
                    }
                };
                for shape in &tile.collision {
                    let mut collider = Collider2d {
                        shape: tile_collider_shape_2d(shape),
                        one_way: tile.one_way,
                        ..Collider2d::default()
                    };
                    if let Some(material) = tile.collision_material {
                        collider.friction = material.friction;
                        collider.restitution = material.restitution;
                    }
                    colliders.push(StaticColliderPart2d { pose, collider });
                }
            }
            if colliders.is_empty() {
                continue;
            }
            active_static_chunks.insert(key);
            runtime.world.upsert_static_chunk(StaticColliderChunk2d {
                key,
                owner,
                colliders,
            });
        }
    }

    runtime.world.retain_entities(&active);
    runtime.world.retain_static_chunks(&active_static_chunks);
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

/// Runtime registry of immutable Sprite Animation documents addressable by stable AssetId.
///
/// Project Rust clip-selection commands use this cache instead of exposing runtime handles.
#[derive(Debug, Default)]
pub struct SpriteAnimationClipRegistry2d {
    clips: BTreeMap<AssetId, Arc<SpriteAnimationDocument>>,
}

impl SpriteAnimationClipRegistry2d {
    /// Inserts or replaces one immutable clip under its stable asset identity.
    pub fn insert(&mut self, asset: AssetId, clip: Arc<SpriteAnimationDocument>) {
        self.clips.insert(asset, clip);
    }

    /// Resolves one loaded immutable clip by stable asset identity.
    pub fn get(&self, asset: &AssetId) -> Option<Arc<SpriteAnimationDocument>> {
        self.clips.get(asset).cloned()
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
        assert_eq!(
            app.world().get_resource::<Project2dSettings>().unwrap(),
            &settings
        );
    }
}
