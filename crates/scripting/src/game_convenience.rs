//! Host-independent typed gameplay convenience helpers.

use crate::game_api::{
    AuthoringIdentityView, CollisionEvent, CollisionEventPhase, Commands, GameApiError,
    GlobalTransformView, Query, QueryRow, QuerySpec, SaveKey, TransformView,
};
use crate::game_contracts::GameField;
use crate::game_each;
use crate::game_io::{GameEntityHandle, GameHitboxShape};
use engine_rig::transform::Transform;
use glam::{Mat4, Quat, Vec3};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Returns a process-local request ID for prefab and timer result correlation.
///
/// IDs are monotonically allocated for the lifetime of the loaded project
/// module. Runtime system ordering is deterministic, so identical schedules
/// allocate IDs in the same order.
pub fn next_request_id() -> u64 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// Reports that a query expected exactly one matching row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryCardinalityError {
    /// Number of rows actually present.
    pub actual: usize,
}

impl fmt::Display for QueryCardinalityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "query expected exactly one row but contained {}",
            self.actual
        )
    }
}

impl std::error::Error for QueryCardinalityError {}

/// Search and cardinality helpers for typed project queries.
pub trait QueryExt<S: QuerySpec> {
    /// Returns the number of matching rows.
    fn len(&self) -> usize;
    /// Returns whether the query contains no rows.
    fn is_empty(&self) -> bool;
    /// Returns the first row in deterministic runtime order.
    fn first(&self) -> Option<&QueryRow<S>>;
    /// Returns the only row, or a cardinality error.
    fn single(&self) -> Result<&QueryRow<S>, QueryCardinalityError>;
    /// Finds a row by generation-checked runtime entity handle.
    fn find_entity(&self, entity: GameEntityHandle) -> Option<&QueryRow<S>>;
    /// Finds a row by authored entity name.
    fn find_name(&self, name: &str) -> Result<Option<&QueryRow<S>>, GameApiError>;
    /// Finds the first row containing an authored tag.
    fn find_tag(&self, tag: &str) -> Result<Option<&QueryRow<S>>, GameApiError>;
    /// Finds the first row assigned to an authored team.
    fn find_team(&self, team: &str) -> Result<Option<&QueryRow<S>>, GameApiError>;
    /// Finds the row whose copied global position is nearest to `position`.
    fn nearest_to(&self, position: Vec3) -> Result<Option<&QueryRow<S>>, GameApiError>;
}

impl<S: QuerySpec> QueryExt<S> for Query<S> {
    fn len(&self) -> usize {
        self.rows().len()
    }

    fn is_empty(&self) -> bool {
        self.rows().is_empty()
    }

    fn first(&self) -> Option<&QueryRow<S>> {
        self.rows().first()
    }

    fn single(&self) -> Result<&QueryRow<S>, QueryCardinalityError> {
        if self.rows().len() == 1 {
            Ok(&self.rows()[0])
        } else {
            Err(QueryCardinalityError {
                actual: self.rows().len(),
            })
        }
    }

    fn find_entity(&self, entity: GameEntityHandle) -> Option<&QueryRow<S>> {
        self.iter().find(|row| row.entity() == entity)
    }

    fn find_name(&self, name: &str) -> Result<Option<&QueryRow<S>>, GameApiError> {
        for row in self.iter() {
            if row.view::<AuthoringIdentityView>()?.name == name {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    fn find_tag(&self, tag: &str) -> Result<Option<&QueryRow<S>>, GameApiError> {
        for row in self.iter() {
            if row
                .view::<AuthoringIdentityView>()?
                .tags
                .iter()
                .any(|candidate| candidate == tag)
            {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    fn find_team(&self, team: &str) -> Result<Option<&QueryRow<S>>, GameApiError> {
        for row in self.iter() {
            if row.view::<AuthoringIdentityView>()?.team == team {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    fn nearest_to(&self, position: Vec3) -> Result<Option<&QueryRow<S>>, GameApiError> {
        let mut nearest = None;
        let mut nearest_distance = f32::INFINITY;
        for row in self.iter() {
            let world = row.view::<GlobalTransformView>()?;
            let distance = world.translation.distance_squared(position);
            if distance < nearest_distance {
                nearest_distance = distance;
                nearest = Some(row);
            }
        }
        Ok(nearest)
    }
}

/// Intent-level helpers for one typed collision transition.
pub trait CollisionEventExt {
    /// Returns whether `entity` participates in this pair.
    fn involves(&self, entity: GameEntityHandle) -> bool;
    /// Returns the other entity in this pair from `entity`'s perspective.
    fn other(&self, entity: GameEntityHandle) -> Option<GameEntityHandle>;
    /// Returns the separation vector that moves `entity` out of the other body.
    fn push_for(&self, entity: GameEntityHandle) -> Option<Vec3>;
    /// Returns whether this is an enter transition.
    fn is_enter(&self) -> bool;
    /// Returns whether this is a stay transition.
    fn is_stay(&self) -> bool;
    /// Returns whether this is an exit transition.
    fn is_exit(&self) -> bool;
    /// Returns whether a trigger pair just entered.
    fn is_trigger_enter(&self) -> bool;
    /// Returns whether a trigger pair just exited.
    fn is_trigger_exit(&self) -> bool;
}

impl CollisionEventExt for CollisionEvent {
    fn involves(&self, entity: GameEntityHandle) -> bool {
        self.entity_a == entity || self.entity_b == entity
    }

    fn other(&self, entity: GameEntityHandle) -> Option<GameEntityHandle> {
        if self.entity_a == entity {
            Some(self.entity_b)
        } else if self.entity_b == entity {
            Some(self.entity_a)
        } else {
            None
        }
    }

    fn push_for(&self, entity: GameEntityHandle) -> Option<Vec3> {
        if self.entity_a == entity {
            Some(self.push_out)
        } else if self.entity_b == entity {
            Some(-self.push_out)
        } else {
            None
        }
    }

    fn is_enter(&self) -> bool {
        self.phase == CollisionEventPhase::Enter
    }

    fn is_stay(&self) -> bool {
        self.phase == CollisionEventPhase::Stay
    }

    fn is_exit(&self) -> bool {
        self.phase == CollisionEventPhase::Exit
    }

    fn is_trigger_enter(&self) -> bool {
        self.is_trigger && self.is_enter()
    }

    fn is_trigger_exit(&self) -> bool {
        self.is_trigger && self.is_exit()
    }
}

/// Shared transform direction and point-conversion helpers.
pub trait TransformExt {
    /// Returns the complete local or world matrix represented by this value.
    fn matrix(&self) -> Mat4;
    /// Returns the rotated local forward direction (`-Z`).
    fn forward(&self) -> Vec3;
    /// Returns the rotated local right direction (`+X`).
    fn right(&self) -> Vec3;
    /// Returns the rotated local up direction (`+Y`).
    fn up(&self) -> Vec3;
    /// Converts a local point into the represented coordinate space.
    fn transform_point(&self, point: Vec3) -> Vec3;
    /// Converts a represented-space point back into local coordinates.
    fn inverse_transform_point(&self, point: Vec3) -> Vec3;
    /// Converts a local direction without applying translation.
    fn transform_direction(&self, direction: Vec3) -> Vec3;
    /// Converts a represented-space direction back into local coordinates.
    fn inverse_transform_direction(&self, direction: Vec3) -> Vec3;
}

fn transform_matrix(translation: Vec3, rotation: Quat, scale: Vec3) -> Mat4 {
    Mat4::from_scale_rotation_translation(scale, rotation, translation)
}

fn forward(rotation: Quat) -> Vec3 {
    (rotation * Vec3::NEG_Z).normalize_or_zero()
}

fn right(rotation: Quat) -> Vec3 {
    (rotation * Vec3::X).normalize_or_zero()
}

fn up(rotation: Quat) -> Vec3 {
    (rotation * Vec3::Y).normalize_or_zero()
}

macro_rules! impl_transform_ext {
    ($type:ty) => {
        impl TransformExt for $type {
            fn matrix(&self) -> Mat4 {
                transform_matrix(self.translation, self.rotation, self.scale)
            }

            fn forward(&self) -> Vec3 {
                forward(self.rotation)
            }

            fn right(&self) -> Vec3 {
                right(self.rotation)
            }

            fn up(&self) -> Vec3 {
                up(self.rotation)
            }

            fn transform_point(&self, point: Vec3) -> Vec3 {
                self.matrix().transform_point3(point)
            }

            fn inverse_transform_point(&self, point: Vec3) -> Vec3 {
                self.matrix().inverse().transform_point3(point)
            }

            fn transform_direction(&self, direction: Vec3) -> Vec3 {
                self.matrix().transform_vector3(direction)
            }

            fn inverse_transform_direction(&self, direction: Vec3) -> Vec3 {
                self.matrix().inverse().transform_vector3(direction)
            }
        }
    };
}

impl_transform_ext!(Transform);
impl_transform_ext!(TransformView);
impl_transform_ext!(game_each::Transform);

/// Typed marker for one project-relative prefab asset.
pub trait PrefabAsset {
    /// Project-relative `*.prefab.json` path.
    const PATH: &'static str;
}

/// Typed marker for one project-relative scene asset.
pub trait SceneAsset {
    /// Project-relative `*.scene.json` path.
    const PATH: &'static str;
}

/// Typed marker for one registered audio asset.
pub trait AudioAsset {
    /// Stable manifest asset ID.
    const ID: &'static str;
}

/// Typed marker for one gameplay timer ID.
pub trait TimerId {
    /// Stable timer ID.
    const ID: &'static str;
}

/// Save value types that can be emitted through the active-save command service.
pub trait SaveWritable: GameField {
    /// Writes this value under `key` using the matching host command.
    fn write_save(self, commands: &mut Commands, key: &'static str);
}

impl SaveWritable for String {
    fn write_save(self, commands: &mut Commands, key: &'static str) {
        commands.set_save_text(key, self);
    }
}

impl SaveWritable for bool {
    fn write_save(self, commands: &mut Commands, key: &'static str) {
        commands.set_save_flag(key, self);
    }
}

macro_rules! impl_numeric_save {
    ($($type:ty),* $(,)?) => {
        $(
            impl SaveWritable for $type {
                fn write_save(self, commands: &mut Commands, key: &'static str) {
                    commands.set_save_number(key, self as f64);
                }
            }
        )*
    };
}

impl_numeric_save!(f32, f64, i64, u64);

/// Builder for one command-created attack hitbox.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HitboxSpec {
    shape: GameHitboxShape,
    team: i32,
    damage: f32,
    membership: u32,
    mask: u32,
    one_hit_per_target: bool,
    knockback: Option<Vec3>,
}

impl HitboxSpec {
    /// Creates an axis-aligned box hitbox specification.
    pub fn aabb(half_extents: Vec3) -> Self {
        Self::new(GameHitboxShape::Aabb {
            half_extents: half_extents.to_array(),
        })
    }

    /// Creates a sphere hitbox specification.
    pub fn sphere(radius: f32) -> Self {
        Self::new(GameHitboxShape::Sphere { radius })
    }

    /// Creates a Y-axis capsule hitbox specification.
    pub fn capsule_y(half_height: f32, radius: f32) -> Self {
        Self::new(GameHitboxShape::CapsuleY {
            half_height,
            radius,
        })
    }

    /// Creates a specification from an explicit engine hitbox shape.
    pub fn new(shape: GameHitboxShape) -> Self {
        Self {
            shape,
            team: 0,
            damage: 0.0,
            membership: 1,
            mask: u32::MAX,
            one_hit_per_target: true,
            knockback: None,
        }
    }

    /// Sets the gameplay team used by hit filtering.
    pub fn team(mut self, team: i32) -> Self {
        self.team = team;
        self
    }

    /// Sets damage applied by an accepted hit.
    pub fn damage(mut self, damage: f32) -> Self {
        self.damage = damage;
        self
    }

    /// Sets collision-layer membership and mask.
    pub fn layers(mut self, membership: u32, mask: u32) -> Self {
        self.membership = membership;
        self.mask = mask;
        self
    }

    /// Sets whether each target may be hit only once per activation.
    pub fn one_hit_per_target(mut self, enabled: bool) -> Self {
        self.one_hit_per_target = enabled;
        self
    }

    /// Adds a world-space knockback request to accepted hits.
    pub fn knockback(mut self, knockback: Vec3) -> Self {
        self.knockback = Some(knockback);
        self
    }
}

/// High-level methods layered over the existing validated command service.
pub trait CommandsExt {
    /// Spawns a prefab and automatically allocates its result request ID.
    fn spawn_prefab_auto(&mut self, path: impl Into<String>, position: Vec3) -> u64;
    /// Spawns a typed prefab asset and returns its result request ID.
    fn spawn_prefab_asset<A: PrefabAsset>(&mut self, position: Vec3) -> u64;
    /// Requests a typed scene transition.
    fn request_scene_asset<A: SceneAsset>(&mut self);
    /// Plays a typed sound-effect asset.
    fn play_sound_effect_asset<A: AudioAsset>(&mut self);
    /// Starts typed background music.
    fn play_background_music_asset<A: AudioAsset>(&mut self);
    /// Crossfades to typed background music.
    fn crossfade_background_music_asset<A: AudioAsset>(&mut self, fade_seconds: f32);
    /// Starts or replaces a typed gameplay timer.
    fn set_typed_timer<T: TimerId>(&mut self, duration_seconds: f32);
    /// Cancels a typed gameplay timer.
    fn cancel_typed_timer<T: TimerId>(&mut self);
    /// Queries a typed timer and automatically allocates a result request ID.
    fn query_typed_timer<T: TimerId>(&mut self) -> u64;
    /// Writes a typed save-key value.
    fn set_typed_save<K>(&mut self, value: K::Value)
    where
        K: SaveKey,
        K::Value: SaveWritable;
    /// Removes a typed save-key value.
    fn remove_typed_save<K: SaveKey>(&mut self);
    /// Creates a hitbox from one builder specification.
    fn create_hitbox_spec(
        &mut self,
        target: GameEntityHandle,
        owner: GameEntityHandle,
        spec: HitboxSpec,
    );
}

impl CommandsExt for Commands {
    fn spawn_prefab_auto(&mut self, path: impl Into<String>, position: Vec3) -> u64 {
        let request_id = next_request_id();
        self.spawn_prefab(path, position, request_id);
        request_id
    }

    fn spawn_prefab_asset<A: PrefabAsset>(&mut self, position: Vec3) -> u64 {
        self.spawn_prefab_auto(A::PATH, position)
    }

    fn request_scene_asset<A: SceneAsset>(&mut self) {
        self.request_scene(A::PATH);
    }

    fn play_sound_effect_asset<A: AudioAsset>(&mut self) {
        self.play_sound_effect(A::ID);
    }

    fn play_background_music_asset<A: AudioAsset>(&mut self) {
        self.play_background_music(A::ID);
    }

    fn crossfade_background_music_asset<A: AudioAsset>(&mut self, fade_seconds: f32) {
        self.crossfade_background_music(A::ID, fade_seconds);
    }

    fn set_typed_timer<T: TimerId>(&mut self, duration_seconds: f32) {
        self.set_timer(T::ID, duration_seconds);
    }

    fn cancel_typed_timer<T: TimerId>(&mut self) {
        self.cancel_timer(T::ID);
    }

    fn query_typed_timer<T: TimerId>(&mut self) -> u64 {
        let request_id = next_request_id();
        self.query_timer(T::ID, request_id);
        request_id
    }

    fn set_typed_save<K>(&mut self, value: K::Value)
    where
        K: SaveKey,
        K::Value: SaveWritable,
    {
        value.write_save(self, K::NAME);
    }

    fn remove_typed_save<K: SaveKey>(&mut self) {
        self.remove_save_value(K::NAME);
    }

    fn create_hitbox_spec(
        &mut self,
        target: GameEntityHandle,
        owner: GameEntityHandle,
        spec: HitboxSpec,
    ) {
        if let Some(knockback) = spec.knockback {
            self.create_hitbox_with_knockback(
                target,
                owner,
                spec.shape,
                spec.team,
                spec.damage,
                spec.membership,
                spec.mask,
                spec.one_hit_per_target,
                knockback,
            );
        } else {
            self.create_hitbox(
                target,
                owner,
                spec.shape,
                spec.team,
                spec.damage,
                spec.membership,
                spec.mask,
                spec.one_hit_per_target,
            );
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_helpers_return_other_entity_and_perspective_push() {
        let first = GameEntityHandle {
            id: 1,
            generation: 0,
        };
        let second = GameEntityHandle {
            id: 2,
            generation: 0,
        };
        let event = CollisionEvent {
            phase: CollisionEventPhase::Enter,
            entity_a: first,
            entity_b: second,
            push_out: Vec3::X,
            is_trigger: true,
        };
        assert_eq!(event.other(first), Some(second));
        assert_eq!(event.push_for(second), Some(-Vec3::X));
        assert!(event.is_trigger_enter());
    }

    #[test]
    fn hitbox_builder_keeps_defaults_and_overrides() {
        let spec = HitboxSpec::sphere(0.5)
            .team(2)
            .damage(10.0)
            .layers(4, 8)
            .knockback(Vec3::Z);
        assert_eq!(spec.team, 2);
        assert_eq!(spec.damage, 10.0);
        assert_eq!(spec.membership, 4);
        assert_eq!(spec.mask, 8);
        assert_eq!(spec.knockback, Some(Vec3::Z));
    }

}
