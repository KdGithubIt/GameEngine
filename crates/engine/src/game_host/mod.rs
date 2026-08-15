//! Thin extension layer over the established game-host compiler.
//!
//! The existing host implementation remains unchanged in `game_host/core.rs`.
//! This layer only supplies the engine-owned physics snapshot requested through
//! the reserved read-only game resource `engine.physics_world`.

mod core;

pub use core::{
    apply_game_output, GameDeferredEffects, GameHostApplyError, GameHostCompileError,
    GameHostFrame, GameHostRuntime, GameSystemMetrics,
};

use crate::camera::{Camera3D, ViewportSize};
use crate::collision::{Collider, CollisionLayers, PhysicsBody, TriggerVolume, WorldShape};
use crate::game_io::{
    validate_game_input_bytes, GameEntityHandle, GameInvocation, GameSystemAccess,
};
use crate::input::MouseInput;
use crate::transform::GlobalTransform;
use engine_authoring::Value;
use engine_ecs::{Entity, World};
use std::collections::BTreeMap;

const PHYSICS_WORLD_RESOURCE_ID: &str = "engine.physics_world";

/// Compiles one project invocation and injects the requested live physics view.
///
/// Systems that do not declare `PhysicsQuery` take the original compiler path
/// without collecting colliders or cameras.
///
/// # Errors
///
/// Returns [`GameHostCompileError`] under the original compiler conditions, or
/// when the injected snapshot exceeds the bounded invocation size.
pub fn compile_game_invocation(
    world: &World,
    system_id: &str,
    access: &GameSystemAccess,
    frame: &GameHostFrame,
) -> Result<GameInvocation, GameHostCompileError> {
    let requests_physics = access
        .resources
        .iter()
        .any(|resource| resource.id == PHYSICS_WORLD_RESOURCE_ID);

    let fallback_frame =
        if requests_physics && !frame.resources.contains_key(PHYSICS_WORLD_RESOURCE_ID) {
            let mut copied = frame.clone();
            copied.resources.insert(
                PHYSICS_WORLD_RESOURCE_ID.to_owned(),
                Value::Object(BTreeMap::new()),
            );
            Some(copied)
        } else {
            None
        };
    let frame = fallback_frame.as_ref().unwrap_or(frame);

    let mut invocation = core::compile_game_invocation(world, system_id, access, frame)?;
    if requests_physics {
        invocation.resources.insert(
            PHYSICS_WORLD_RESOURCE_ID.to_owned(),
            physics_world_value(world),
        );
        invocation
            .validate_collection_limits()
            .map_err(GameHostCompileError::Limit)?;
        let encoded = serde_json::to_vec(&invocation).map_err(GameHostCompileError::Serialize)?;
        validate_game_input_bytes(encoded.len()).map_err(GameHostCompileError::Limit)?;
    }
    Ok(invocation)
}

fn physics_world_value(world: &World) -> Value {
    let mut entities = world.entities().collect::<Vec<_>>();
    entities.sort_by_key(|entity| (entity.id(), entity.generation()));

    let colliders = entities
        .iter()
        .filter_map(|entity| physics_collider_value(world, *entity))
        .collect();
    let cameras = entities
        .iter()
        .filter_map(|entity| physics_camera_value(world, *entity))
        .collect();

    let viewport = world
        .get_resource::<ViewportSize>()
        .map_or_else(ViewportSize::default, Clone::clone);
    let mouse_position = world
        .get_resource::<MouseInput>()
        .map_or((0.0, 0.0), |mouse| mouse.position);

    Value::Object(BTreeMap::from([
        ("cameras".to_owned(), Value::Array(cameras)),
        ("colliders".to_owned(), Value::Array(colliders)),
        (
            "mouse_position".to_owned(),
            vec2_value([mouse_position.0, mouse_position.1]),
        ),
        (
            "viewport".to_owned(),
            vec2_value([viewport.width as f32, viewport.height as f32]),
        ),
    ]))
}

fn physics_collider_value(world: &World, entity: Entity) -> Option<Value> {
    let collider = world.get_component::<Collider>(entity)?;
    let body = world.get_component::<PhysicsBody>(entity)?;
    let transform = world.get_component::<GlobalTransform>(entity)?;
    let layers = world
        .get_component::<CollisionLayers>(entity)
        .cloned()
        .unwrap_or_default();
    let body = match body {
        PhysicsBody::Static => "static",
        PhysicsBody::Kinematic => "kinematic",
        PhysicsBody::Dynamic => "dynamic",
    };

    Some(Value::Object(BTreeMap::from([
        ("body".to_owned(), Value::String(body.to_owned())),
        ("entity".to_owned(), entity_handle_value(entity)),
        (
            "is_trigger".to_owned(),
            Value::Bool(world.has_component::<TriggerVolume>(entity)),
        ),
        ("mask".to_owned(), Value::U64(u64::from(layers.mask))),
        (
            "membership".to_owned(),
            Value::U64(u64::from(layers.membership)),
        ),
        (
            "shape".to_owned(),
            world_shape_value(collider.world_shape(transform)),
        ),
    ])))
}

fn physics_camera_value(world: &World, entity: Entity) -> Option<Value> {
    let camera = world.get_component::<Camera3D>(entity)?;
    let transform = world.get_component::<GlobalTransform>(entity)?;
    Some(Value::Object(BTreeMap::from([
        (
            "camera".to_owned(),
            Value::Object(BTreeMap::from([
                ("aspect".to_owned(), Value::F64(f64::from(camera.aspect))),
                ("enabled".to_owned(), Value::Bool(camera.enabled)),
                ("far".to_owned(), Value::F64(f64::from(camera.far))),
                (
                    "fov_y_radians".to_owned(),
                    Value::F64(f64::from(camera.fov_y_radians)),
                ),
                ("near".to_owned(), Value::F64(f64::from(camera.near))),
                (
                    "priority".to_owned(),
                    Value::I64(i64::from(camera.priority)),
                ),
            ])),
        ),
        ("entity".to_owned(), entity_handle_value(entity)),
        (
            "world_matrix".to_owned(),
            matrix_value(transform.matrix().to_cols_array()),
        ),
    ])))
}

fn world_shape_value(shape: WorldShape) -> Value {
    match shape {
        WorldShape::Aabb(aabb) => Value::Object(BTreeMap::from([
            ("center".to_owned(), vec3_value(aabb.center.to_array())),
            (
                "half_extents".to_owned(),
                vec3_value(aabb.half_extents.to_array()),
            ),
            ("kind".to_owned(), Value::String("aabb".to_owned())),
        ])),
        WorldShape::Sphere(sphere) => Value::Object(BTreeMap::from([
            ("center".to_owned(), vec3_value(sphere.center.to_array())),
            ("kind".to_owned(), Value::String("sphere".to_owned())),
            ("radius".to_owned(), Value::F64(f64::from(sphere.radius))),
        ])),
        WorldShape::CapsuleY(capsule) => Value::Object(BTreeMap::from([
            ("kind".to_owned(), Value::String("capsule_y".to_owned())),
            ("radius".to_owned(), Value::F64(f64::from(capsule.radius))),
            (
                "segment_a".to_owned(),
                vec3_value(capsule.segment_a.to_array()),
            ),
            (
                "segment_b".to_owned(),
                vec3_value(capsule.segment_b.to_array()),
            ),
        ])),
    }
}

fn entity_handle_value(entity: Entity) -> Value {
    let handle = GameEntityHandle {
        id: entity.id(),
        generation: entity.generation(),
    };
    Value::Object(BTreeMap::from([
        (
            "generation".to_owned(),
            Value::U64(u64::from(handle.generation)),
        ),
        ("id".to_owned(), Value::U64(u64::from(handle.id))),
    ]))
}

fn vec2_value(value: [f32; 2]) -> Value {
    Value::Array(
        value
            .into_iter()
            .map(|part| Value::F64(f64::from(part)))
            .collect(),
    )
}

fn vec3_value(value: [f32; 3]) -> Value {
    Value::Array(
        value
            .into_iter()
            .map(|part| Value::F64(f64::from(part)))
            .collect(),
    )
}

fn matrix_value(value: [f32; 16]) -> Value {
    Value::Array(
        value
            .into_iter()
            .map(|part| Value::F64(f64::from(part)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::Transform;

    #[test]
    fn physics_resource_is_only_injected_when_declared() {
        let mut world = World::new();
        let entity = world.spawn().unwrap();
        world.add_component(entity, Collider::sphere(1.0)).unwrap();
        world.add_component(entity, PhysicsBody::Static).unwrap();
        world
            .add_component(entity, GlobalTransform::default())
            .unwrap();
        world.add_component(entity, Transform::default()).unwrap();
        let camera = Camera3D {
            enabled: false,
            priority: 7,
            ..Camera3D::default()
        };
        world.add_component(entity, camera).unwrap();

        let access = GameSystemAccess {
            resources: vec![crate::game_io::GameResourceAccess {
                id: PHYSICS_WORLD_RESOURCE_ID.to_owned(),
                mode: crate::game_io::GameAccessMode::Read,
            }],
            ..GameSystemAccess::default()
        };
        let invocation = compile_game_invocation(
            &world,
            "game.physics_reader",
            &access,
            &GameHostFrame::default(),
        )
        .unwrap();
        let Value::Object(snapshot) = &invocation.resources[PHYSICS_WORLD_RESOURCE_ID] else {
            panic!("physics snapshot must be an object");
        };
        let Value::Array(colliders) = &snapshot["colliders"] else {
            panic!("physics colliders must be an array");
        };
        assert_eq!(colliders.len(), 1);
        let Value::Array(cameras) = &snapshot["cameras"] else {
            panic!("physics cameras must be an array");
        };
        let Value::Object(camera) = &cameras[0] else {
            panic!("physics camera must be an object");
        };
        let Value::Object(projection) = &camera["camera"] else {
            panic!("physics camera projection must be an object");
        };
        assert_eq!(projection["enabled"], Value::Bool(false));
        assert_eq!(projection["priority"], Value::I64(7));
    }
}
