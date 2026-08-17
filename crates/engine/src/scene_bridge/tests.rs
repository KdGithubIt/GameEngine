//! Authoring-to-runtime conversion tests.

use super::*;

use engine_authoring::command::AuthoringCommand;
use engine_authoring::scene::AuthoringScene;
use engine_authoring::test_fixtures::load_scene_fixture;
use engine_authoring::transaction::Transaction;
use engine_authoring::{ComponentTypeId, EntityId, Value};
use engine_ecs::{IntoSystem, System, With, World};

/// Inline scene used for JSON-pipeline integration tests.
const TEST_SCENE_JSON: &str = r#"{
    "entities": [
        {
            "id": "entity_01JP0000000000000000000001",
            "name": "player",
            "display_name": "Player",
            "description": "The player entity.",
            "components": {
                "engine.player_marker": {},
                "engine.transform": { "x": -0.5, "y": 0.0, "z": 0.0 }
            }
        },
        {
            "id": "entity_01JP0000000000000000000002",
            "name": "obstacle",
            "display_name": "Obstacle",
            "description": "A static obstacle.",
            "components": {
                "engine.transform": { "x": 0.5, "y": 0.0, "z": 0.0 }
            }
        }
    ]
}"#;

fn test_wav_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&38_u32.to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&44_100_u32.to_le_bytes());
    bytes.extend_from_slice(&88_200_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&2_u32.to_le_bytes());
    bytes.extend_from_slice(&0_i16.to_le_bytes());
    bytes
}

fn make_scene_with_player_and_npc() -> (AuthoringScene, EntityId, EntityId) {
    let mut scene = AuthoringScene::new();
    let player_id = EntityId::generate();
    let npc_id = EntityId::generate();

    let mut tx = Transaction::begin(&scene);
    tx.apply(AuthoringCommand::CreateEntity {
        id: player_id.clone(),
        name: "player".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::AddComponent {
        entity: player_id.clone(),
        component_type: ComponentTypeId::new(PLAYER_MARKER_COMPONENT),
        value: Value::Object(std::collections::BTreeMap::new()),
    });
    tx.apply(AuthoringCommand::CreateEntity {
        id: npc_id.clone(),
        name: "npc".into(),
        parent: None,
    });
    tx.commit(&mut scene)
        .expect("setup transaction must commit");

    (scene, player_id, npc_id)
}

#[test]
fn bridge_spawns_one_entity_per_authoring_entity() {
    let mut scene = AuthoringScene::new();
    let id_a = EntityId::generate();
    let id_b = EntityId::generate();

    let mut tx = Transaction::begin(&scene);
    tx.apply(AuthoringCommand::CreateEntity {
        id: id_a.clone(),
        name: "alpha".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::CreateEntity {
        id: id_b.clone(),
        name: "beta".into(),
        parent: None,
    });
    tx.commit(&mut scene)
        .expect("setup transaction must commit");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");

    assert!(bridge.get(&id_a).is_some());
    assert!(bridge.get(&id_b).is_some());
    assert_ne!(bridge.get(&id_a), bridge.get(&id_b));
    assert_eq!(world.entity_count(), 2);
}

#[test]
fn native_2d_camera_and_sprite_components_bridge_into_runtime_ecs() {
    let mut scene = AuthoringScene::new();
    let id = EntityId::generate();
    let atlas = AssetId::generate();
    let sprite_id = SpriteId::generate();
    let mut camera = std::collections::BTreeMap::new();
    camera.insert("enabled".into(), Value::Bool(true));
    camera.insert("priority".into(), Value::I64(7));
    camera.insert("orthographic_height".into(), Value::F64(12.0));
    camera.insert("zoom".into(), Value::F64(2.0));
    camera.insert("near".into(), Value::F64(-100.0));
    camera.insert("far".into(), Value::F64(100.0));
    camera.insert("pixel_perfect".into(), Value::Bool(true));
    camera.insert("reference_pixels_per_unit".into(), Value::F64(100.0));
    camera.insert("reference_width".into(), Value::I64(320));
    camera.insert("reference_height".into(), Value::I64(180));
    camera.insert("fit".into(), Value::String("fit".into()));
    let mut sprite = std::collections::BTreeMap::new();
    sprite.insert("atlas".into(), Value::AssetRef(atlas.clone()));
    sprite.insert("sprite_id".into(), Value::String(sprite_id.as_str().into()));
    sprite.insert("tint_r".into(), Value::F64(1.0));
    sprite.insert("tint_g".into(), Value::F64(0.5));
    sprite.insert("tint_b".into(), Value::F64(0.25));
    sprite.insert("tint_a".into(), Value::F64(1.0));
    sprite.insert("flip_x".into(), Value::Bool(true));
    sprite.insert("flip_y".into(), Value::Bool(false));
    sprite.insert("sorting_layer".into(), Value::String("sorting_layer_00000000000000000000000000".into()));
    sprite.insert("order_in_layer".into(), Value::I64(3));
    sprite.insert("visible".into(), Value::Bool(true));

    let mut tx = Transaction::begin(&scene);
    tx.apply(AuthoringCommand::CreateEntity { id: id.clone(), name: "native_2d".into(), parent: None });
    tx.apply(AuthoringCommand::AddComponent { entity: id.clone(), component_type: ComponentTypeId::new(CAMERA_2D_COMPONENT), value: Value::Object(camera) });
    tx.apply(AuthoringCommand::AddComponent { entity: id.clone(), component_type: ComponentTypeId::new(SPRITE_RENDERER_2D_COMPONENT), value: Value::Object(sprite) });
    tx.commit(&mut scene).expect("Native 2D setup must commit");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("Native 2D scene must bridge");
    let entity = bridge.get(&id).expect("runtime entity");
    let runtime_camera = world.get_component::<Camera2d>(entity).expect("Camera2D runtime component");
    assert_eq!(runtime_camera.priority, 7);
    assert!(runtime_camera.pixel_perfect);
    let runtime_sprite = world.get_component::<SpriteRenderer2d>(entity).expect("SpriteRenderer2D runtime component");
    assert_eq!(runtime_sprite.sprite.atlas, atlas);
    assert_eq!(runtime_sprite.sprite.sprite, sprite_id);
    assert_eq!(runtime_sprite.order_in_layer, 3);
    assert!(runtime_sprite.flip_x);
}

fn secondary_motion_rig(id: AssetId, name: &str) -> SecondaryMotionRigAsset {
    SecondaryMotionRigAsset {
        schema_version: crate::secondary_motion::SECONDARY_MOTION_RIG_SCHEMA_VERSION,
        id,
        name: name.to_owned(),
        skeleton: None,
        skeleton_identity: None,
        bodies: Vec::new(),
        joints: Vec::new(),
    }
}

#[test]
fn rollback_restores_previous_secondary_motion_registry_entry() {
    let id = AssetId::generate();
    let previous = secondary_motion_rig(id.clone(), "previous");
    let replacement = secondary_motion_rig(id.clone(), "replacement");
    let mut registry = SecondaryMotionRigRegistry::new();
    registry.insert(replacement);
    let mut world = World::new();
    world.insert_resource(registry);
    let assets = BridgeAssetState {
        secondary_motion_rig_rollbacks: vec![(id.clone(), Some(previous.clone()))],
        ..BridgeAssetState::default()
    };
    let mut spawned = Vec::new();

    let errors = rollback_bridge_changes(&mut world, &mut spawned, &assets);

    assert!(errors.is_empty());
    assert_eq!(
        world
            .get_resource::<SecondaryMotionRigRegistry>()
            .and_then(|registry| registry.get(&id)),
        Some(&previous)
    );
}

#[test]
fn rollback_removes_secondary_motion_registry_created_by_conversion() {
    let mut world = World::new();
    world.insert_resource(SecondaryMotionRigRegistry::new());
    let assets = BridgeAssetState {
        remove_secondary_motion_registry_store: true,
        ..BridgeAssetState::default()
    };
    let mut spawned = Vec::new();

    let errors = rollback_bridge_changes(&mut world, &mut spawned, &assets);

    assert!(errors.is_empty());
    assert!(world
        .get_resource::<SecondaryMotionRigRegistry>()
        .is_none());
}

#[test]
fn disabled_entities_and_their_descendants_are_not_spawned() {
    let mut scene = AuthoringScene::new();
    let root_id = EntityId::generate();
    let child_id = EntityId::generate();
    let sibling_id = EntityId::generate();

    let mut tx = Transaction::begin(&scene);
    tx.apply(AuthoringCommand::CreateEntity {
        id: root_id.clone(),
        name: "disabled_root".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::CreateEntity {
        id: child_id.clone(),
        name: "child_of_disabled".into(),
        parent: Some(root_id.clone()),
    });
    tx.apply(AuthoringCommand::CreateEntity {
        id: sibling_id.clone(),
        name: "enabled_sibling".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::SetEntityEnabled {
        entity: root_id.clone(),
        enabled: false,
    });
    tx.commit(&mut scene)
        .expect("setup transaction must commit");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");

    assert!(
        bridge.get(&root_id).is_none(),
        "disabled entity must not spawn"
    );
    assert!(
        bridge.get(&child_id).is_none(),
        "descendants of a disabled entity must not spawn"
    );
    assert!(
        bridge.get(&sibling_id).is_some(),
        "unrelated enabled entities keep spawning"
    );
    assert_eq!(world.entity_count(), 1);
}

fn make_scene_with_parent_and_child() -> (AuthoringScene, EntityId, EntityId) {
    let mut scene = AuthoringScene::new();
    let parent_id = EntityId::generate();
    let child_id = EntityId::generate();

    let mut parent_translation = std::collections::BTreeMap::new();
    parent_translation.insert("x".into(), Value::F64(10.0));
    parent_translation.insert("y".into(), Value::F64(0.0));
    parent_translation.insert("z".into(), Value::F64(0.0));
    let mut child_translation = std::collections::BTreeMap::new();
    child_translation.insert("x".into(), Value::F64(0.0));
    child_translation.insert("y".into(), Value::F64(5.0));
    child_translation.insert("z".into(), Value::F64(0.0));

    let mut tx = Transaction::begin(&scene);
    tx.apply(AuthoringCommand::CreateEntity {
        id: parent_id.clone(),
        name: "parent".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::AddComponent {
        entity: parent_id.clone(),
        component_type: ComponentTypeId::new(TRANSFORM_COMPONENT),
        value: Value::Object(parent_translation),
    });
    tx.apply(AuthoringCommand::CreateEntity {
        id: child_id.clone(),
        name: "child".into(),
        parent: Some(parent_id.clone()),
    });
    tx.apply(AuthoringCommand::AddComponent {
        entity: child_id.clone(),
        component_type: ComponentTypeId::new(TRANSFORM_COMPONENT),
        value: Value::Object(child_translation),
    });
    tx.commit(&mut scene)
        .expect("setup transaction must commit");

    (scene, parent_id, child_id)
}

#[test]
fn bridge_attaches_parent_and_children_from_authoring_hierarchy() {
    let (scene, parent_id, child_id) = make_scene_with_parent_and_child();
    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");

    let parent = bridge.get(&parent_id).expect("parent must be in bridge");
    let child = bridge.get(&child_id).expect("child must be in bridge");

    let runtime_parent = world
        .get_component::<Parent>(child)
        .expect("child must receive Parent");
    assert_eq!(runtime_parent.0, parent);
    let children = world
        .get_component::<Children>(parent)
        .expect("parent must receive Children");
    assert_eq!(children.0, vec![child]);
    assert!(world.get_component::<Parent>(parent).is_none());
}

#[test]
fn spawned_child_follows_parent_during_transform_propagation() {
    let (scene, parent_id, child_id) = make_scene_with_parent_and_child();
    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");

    crate::transform::transform_propagation_system(engine_ecs::Query::new(&mut world));

    let parent = bridge.get(&parent_id).expect("parent must be in bridge");
    let child = bridge.get(&child_id).expect("child must be in bridge");
    let parent_global = world
        .get_component::<GlobalTransform>(parent)
        .expect("parent must keep GlobalTransform")
        .matrix()
        .col(3)
        .truncate();
    let child_global = world
        .get_component::<GlobalTransform>(child)
        .expect("child must keep GlobalTransform")
        .matrix()
        .col(3)
        .truncate();

    assert_eq!(parent_global, Vec3::new(10.0, 0.0, 0.0));
    assert_eq!(child_global, Vec3::new(10.0, 5.0, 0.0));
}

#[test]
fn player_marker_component_is_added_for_marked_entities() {
    let (scene, player_id, npc_id) = make_scene_with_player_and_npc();
    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");

    let player = bridge.get(&player_id).expect("player must be in bridge");
    let npc = bridge.get(&npc_id).expect("npc must be in bridge");

    assert!(world.has_component::<PlayerMarker>(player));
    assert!(!world.has_component::<PlayerMarker>(npc));
}

#[test]
fn with_player_marker_filter_excludes_unmarked_entities() {
    let (scene, _, _) = make_scene_with_player_and_npc();
    let mut world = World::new();
    let _bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");

    // Both entities have Transform, but only the player has PlayerMarker.
    // With<PlayerMarker> must yield exactly one result.
    let q = world
        .query_filtered::<&Transform, With<PlayerMarker>>()
        .expect("query must build");
    assert_eq!(
        q.iter().count(),
        1,
        "only the player entity must match With<PlayerMarker>"
    );
}

#[test]
fn engine_transform_component_sets_translation() {
    let mut scene = AuthoringScene::new();
    let id = EntityId::generate();

    let mut obj = std::collections::BTreeMap::new();
    obj.insert("x".into(), Value::F64(1.5));
    obj.insert("y".into(), Value::F64(2.0));
    obj.insert("z".into(), Value::F64(-0.5));

    let mut tx = Transaction::begin(&scene);
    tx.apply(AuthoringCommand::CreateEntity {
        id: id.clone(),
        name: "positioned".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::AddComponent {
        entity: id.clone(),
        component_type: ComponentTypeId::new(TRANSFORM_COMPONENT),
        value: Value::Object(obj),
    });
    tx.commit(&mut scene)
        .expect("setup transaction must commit");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");

    let entity = bridge.get(&id).expect("entity must be in bridge");
    let transform = world
        .get_component::<Transform>(entity)
        .expect("entity must have Transform");

    assert!((transform.translation.x - 1.5).abs() < f32::EPSILON);
    assert!((transform.translation.y - 2.0).abs() < f32::EPSILON);
    assert!((transform.translation.z - (-0.5)).abs() < f32::EPSILON);
}

#[test]
fn engine_transform_component_sets_rotation_and_scale_and_defaults_the_optional_fields() {
    let mut scene = AuthoringScene::new();
    let id = EntityId::generate();
    let mut obj = std::collections::BTreeMap::from([
        ("x".into(), Value::F64(1.0)),
        ("y".into(), Value::F64(2.0)),
        ("z".into(), Value::F64(3.0)),
        ("rotation_y_degrees".into(), Value::F64(90.0)),
        ("scale_x".into(), Value::F64(2.0)),
        ("scale_y".into(), Value::F64(3.0)),
        ("scale_z".into(), Value::F64(4.0)),
    ]);
    let mut tx = Transaction::begin(&scene);
    tx.apply(AuthoringCommand::CreateEntity {
        id: id.clone(),
        name: "complete_transform".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::AddComponent {
        entity: id.clone(),
        component_type: ComponentTypeId::new(TRANSFORM_COMPONENT),
        value: Value::Object(std::mem::take(&mut obj)),
    });
    tx.commit(&mut scene).expect("setup transaction");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid transform");
    let runtime = world
        .get_component::<Transform>(bridge.get(&id).expect("bridged entity"))
        .expect("runtime transform");

    assert_eq!(runtime.translation, Vec3::new(1.0, 2.0, 3.0));
    assert_eq!(runtime.scale, Vec3::new(2.0, 3.0, 4.0));
    assert!(runtime
        .rotation
        .abs_diff_eq(Quat::from_rotation_y(90.0_f32.to_radians()), 1.0e-5));

    // Rotation and scale are optional in the current Transform schema, so a
    // position-only value stays valid and keeps the identity pose.
    let position_only = extract_transform_value(
        scene.entity(&id).expect("authoring entity"),
        &ComponentTypeId::new(TRANSFORM_COMPONENT),
        &Value::Object(std::collections::BTreeMap::from([
            ("x".into(), Value::F64(0.0)),
            ("y".into(), Value::F64(0.0)),
            ("z".into(), Value::F64(0.0)),
        ])),
    )
    .expect("position-only transform must remain valid");
    assert_eq!(position_only.rotation, Quat::IDENTITY);
    assert_eq!(position_only.scale, Vec3::ONE);
}

#[test]
fn engine_transform_u64_coordinate_is_converted_not_zeroed() {
    let mut scene = AuthoringScene::new();
    let id = EntityId::generate();

    // Value::U64 only arises for values > i64::MAX in authoring data.
    let large_x = (i64::MAX as u64) + 1;

    let mut obj = std::collections::BTreeMap::new();
    obj.insert("x".into(), Value::U64(large_x));
    obj.insert("y".into(), Value::F64(0.0));
    obj.insert("z".into(), Value::F64(0.0));

    let mut tx = Transaction::begin(&scene);
    tx.apply(AuthoringCommand::CreateEntity {
        id: id.clone(),
        name: "large_coord".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::AddComponent {
        entity: id.clone(),
        component_type: ComponentTypeId::new(TRANSFORM_COMPONENT),
        value: Value::Object(obj),
    });
    tx.commit(&mut scene)
        .expect("setup transaction must commit");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");

    let entity = bridge.get(&id).expect("entity must be in bridge");
    let transform = world
        .get_component::<Transform>(entity)
        .expect("entity must have Transform");

    assert_ne!(
        transform.translation.x, 0.0,
        "Value::U64 coordinate must not silently collapse to 0.0"
    );
    assert!(
        (transform.translation.x - large_x as f32).abs() < 1.0,
        "Value::U64 must convert to f32; expected ~{}, got {}",
        large_x as f32,
        transform.translation.x
    );
}

#[test]
fn engine_transform_non_numeric_coordinate_returns_typed_error() {
    let mut scene = AuthoringScene::new();
    let id = EntityId::generate();

    let mut obj = std::collections::BTreeMap::new();
    // Bool is not a valid numeric coordinate.
    obj.insert("x".into(), Value::Bool(true));
    obj.insert("y".into(), Value::F64(3.0));
    obj.insert("z".into(), Value::F64(0.0));

    let mut tx = Transaction::begin(&scene);
    tx.apply(AuthoringCommand::CreateEntity {
        id: id.clone(),
        name: "bad_coord".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::AddComponent {
        entity: id.clone(),
        component_type: ComponentTypeId::new(TRANSFORM_COMPONENT),
        value: Value::Object(obj),
    });
    tx.commit(&mut scene)
        .expect("setup transaction must commit");

    let mut world = World::new();
    let error = spawn_from_authoring_scene(&mut world, &scene)
        .expect_err("malformed transform must not silently fall back");

    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue {
            component_type,
            ..
        } if component_type.as_str() == TRANSFORM_COMPONENT
    ));
    assert_eq!(world.entity_count(), 0);
}

#[test]
fn player_marker_requires_empty_object_value() {
    let invalid_values = [
        Value::Null,
        Value::I64(1),
        Value::String("player".into()),
        Value::Array(Vec::new()),
        Value::Object(std::collections::BTreeMap::from([(
            "unexpected".into(),
            Value::Bool(true),
        )])),
    ];

    for value in invalid_values {
        let mut scene = AuthoringScene::new();
        let id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: "invalid_marker".into(),
            parent: None,
        });
        tx.apply(AuthoringCommand::AddComponent {
            entity: id,
            component_type: ComponentTypeId::new(PLAYER_MARKER_COMPONENT),
            value,
        });
        tx.commit(&mut scene)
            .expect("schema-free authoring transaction must commit");

        let mut world = World::new();
        let error = spawn_from_authoring_scene(&mut world, &scene)
            .expect_err("invalid player marker must fail conversion");

        assert!(matches!(
            error,
            SceneBridgeError::InvalidComponentValue {
                component_type,
                ..
            } if component_type.as_str() == PLAYER_MARKER_COMPONENT
        ));
        assert_eq!(world.entity_count(), 0);
        assert!(world.get_resource::<Assets<Mesh>>().is_none());
        assert!(world.get_resource::<Assets<Material>>().is_none());
    }
}

#[test]
fn malformed_second_entity_leaves_world_and_asset_stores_unchanged() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "valid_first",
                    "components": {
                        "engine.mesh": {
                            "$type": "asset_ref",
                            "id": "asset_01JP0000000000000000000101"
                        }
                    }
                },
                {
                    "id": "entity_01JP0000000000000000000002",
                    "name": "invalid_second",
                    "components": {
                        "engine.transform": { "x": false, "y": 0.0, "z": 0.0 }
                    }
                }
            ]
        }"#,
    )
    .expect("test scene JSON must load");

    let mut world = World::new();
    let error = spawn_from_authoring_scene(&mut world, &scene)
        .expect_err("malformed second entity must fail conversion");

    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue {
            component_type,
            ..
        } if component_type.as_str() == TRANSFORM_COMPONENT
    ));
    assert_eq!(world.entity_count(), 0);
    assert!(world.get_resource::<Assets<Mesh>>().is_none());
    assert!(world.get_resource::<Assets<Material>>().is_none());
}

#[test]
fn best_effort_conversion_skips_invalid_component_and_keeps_the_scene() {
    // The surface's `source` is present but not an AssetRef, so the component
    // value is corrupted rather than merely unassigned.
    let scene = load_scene_fixture(
        r#"{
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "valid_mesh",
                    "components": {
                        "engine.mesh": {
                            "$type": "asset_ref",
                            "id": "asset_01JP0000000000000000000101"
                        }
                    }
                },
                {
                    "id": "entity_01JP0000000000000000000002",
                    "name": "corrupted_surface",
                    "components": {
                        "engine.nav_mesh_surface": {"source": 5}
                    }
                }
            ]
        }"#,
    )
    .expect("test scene JSON must load");

    let mut world = World::new();
    let result = spawn_from_authoring_scene_best_effort(&mut world, &scene)
        .expect("an invalid component must not abort best-effort conversion");

    assert_eq!(
        world.entity_count(),
        2,
        "both entities must survive the skipped component"
    );
    assert_eq!(result.entities.len(), 2);
    let skipped: Vec<_> = result
        .asset_diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == COMPONENT_SKIPPED_DIAGNOSTIC)
        .collect();
    assert_eq!(skipped.len(), 1, "exactly one component must be skipped");
    assert!(!skipped[0].is_blocking());
    assert!(skipped[0].message.contains("engine.nav_mesh_surface"));
    assert!(skipped[0]
        .message
        .contains("entity_01JP0000000000000000000002"));
    assert!(
        world
            .get_resource::<crate::navmesh::NavMeshQuery>()
            .is_none(),
        "the skipped surface must not reach the runtime world"
    );
}

#[test]
fn an_unassigned_required_reference_converts_to_an_inactive_component() {
    // The surface matches the editor state right after Add Component: every
    // defaulted field is present but the required source is not.
    let scene = load_scene_fixture(
        r#"{
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "incomplete_surface",
                    "components": {
                        "engine.nav_mesh_surface": {}
                    }
                }
            ]
        }"#,
    )
    .expect("test scene JSON must load");

    let mut world = World::new();
    let result = spawn_from_authoring_scene(&mut world, &scene)
        .expect("an unassigned reference must not abort strict conversion");

    assert_eq!(world.entity_count(), 1, "the entity itself must spawn");
    let inactive: Vec<_> = result
        .asset_diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == COMPONENT_INACTIVE_DIAGNOSTIC)
        .collect();
    assert_eq!(inactive.len(), 1, "one inactive warning must be reported");
    assert!(!inactive[0].is_blocking());
    assert!(inactive[0].message.contains("engine.nav_mesh_surface"));
    assert!(inactive[0].message.contains("source"));
    let animators = engine_ecs::Query::<&Animator>::new(&mut world);
    assert_eq!(
        animators.iter().count(),
        0,
        "the inactive animator must not reach the runtime world"
    );
}

#[test]
fn unassigned_audio_clip_converts_to_inactive_emitter_in_strict_mode() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "incomplete_emitter",
                    "components": {
                        "engine.transform": { "x": 0.0, "y": 0.0, "z": 0.0 },
                        "engine.audio_emitter": {
                            "volume": 1.0,
                            "spatial_blend": 1.0,
                            "min_distance": 1.0,
                            "max_distance": 30.0,
                            "autoplay": false,
                            "looping": false
                        }
                    }
                }
            ]
        }"#,
    )
    .expect("test scene JSON must load");

    let mut world = World::new();
    let result = spawn_from_authoring_scene(&mut world, &scene)
        .expect("an unassigned clip must not abort strict conversion");

    assert_eq!(world.entity_count(), 1);
    assert!(result
        .asset_diagnostics
        .iter()
        .any(
            |diagnostic| diagnostic.code == COMPONENT_INACTIVE_DIAGNOSTIC
                && diagnostic.message.contains("engine.audio_emitter")
        ));
}

#[test]
fn best_effort_conversion_still_rejects_blocking_scene_validation() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "orphan",
                    "parent": "entity_01JP0000000000000000000099",
                    "components": {}
                }
            ]
        }"#,
    )
    .expect("test scene JSON must load");

    let mut world = World::new();
    let error = spawn_from_authoring_scene_best_effort(&mut world, &scene)
        .expect_err("a blocking scene validation error must still abort");

    assert!(matches!(error, SceneBridgeError::InvalidScene { .. }));
    assert_eq!(world.entity_count(), 0);
}

#[test]
fn unregistered_asset_spawns_entity_with_fallback_and_warning() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "unknown_asset",
                    "components": {
                        "engine.static_mesh_renderer": {
                            "mesh": {
                                "$type": "asset_ref",
                                "id": "asset_01JP0000000000000000000999"
                            },
                            "material": {
                                "$type": "asset_ref",
                                "id": "asset_01JP0000000000000000000203"
                            },
                            "material_slots": []
                        }
                    }
                }
            ]
        }"#,
    )
    .expect("test scene JSON must load");

    let mut world = World::new();
    let result = spawn_from_authoring_scene(&mut world, &scene)
        .expect("unregistered asset must not block scene conversion");

    assert_eq!(
        world.entity_count(),
        1,
        "entity must be spawned with fallback"
    );
    assert_eq!(result.asset_diagnostics.len(), 1);
    assert_eq!(
        result.asset_diagnostics[0].code, "asset.unregistered_file",
        "unregistered asset must emit asset.unregistered_file warning"
    );
    assert!(!result.asset_diagnostics[0].is_blocking());
}

#[test]
fn unknown_id_returns_none_from_bridge() {
    let scene = AuthoringScene::new();
    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");

    let unknown = EntityId::generate();
    assert!(bridge.get(&unknown).is_none());
}

// --- JSON pipeline integration tests ---

#[test]
fn json_scene_spawns_correct_number_of_runtime_entities() {
    let scene = load_scene_fixture(TEST_SCENE_JSON).expect("test JSON must be valid");
    let mut world = World::new();
    spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");
    assert_eq!(world.entity_count(), 2);
}

#[test]
fn player_marker_from_json_is_added_to_runtime_entity() {
    let scene = load_scene_fixture(TEST_SCENE_JSON).expect("test JSON must be valid");

    let player_id = scene
        .entities()
        .find(|(_, e)| e.name == "player")
        .map(|(id, _)| id.clone())
        .expect("player entity must exist");
    let obstacle_id = scene
        .entities()
        .find(|(_, e)| e.name == "obstacle")
        .map(|(id, _)| id.clone())
        .expect("obstacle entity must exist");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");

    let player = bridge.get(&player_id).expect("player must be in bridge");
    let obstacle = bridge
        .get(&obstacle_id)
        .expect("obstacle must be in bridge");

    assert!(
        world.has_component::<PlayerMarker>(player),
        "player from JSON must have PlayerMarker"
    );
    assert!(
        !world.has_component::<PlayerMarker>(obstacle),
        "obstacle from JSON must not have PlayerMarker"
    );
}

#[test]
fn transform_translation_from_json_sets_runtime_position() {
    let scene = load_scene_fixture(TEST_SCENE_JSON).expect("test JSON must be valid");

    let player_id = scene
        .entities()
        .find(|(_, e)| e.name == "player")
        .map(|(id, _)| id.clone())
        .expect("player entity must exist");
    let obstacle_id = scene
        .entities()
        .find(|(_, e)| e.name == "obstacle")
        .map(|(id, _)| id.clone())
        .expect("obstacle entity must exist");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");

    let player = bridge.get(&player_id).expect("player must be in bridge");
    let obstacle = bridge
        .get(&obstacle_id)
        .expect("obstacle must be in bridge");

    let player_t = world
        .get_component::<Transform>(player)
        .expect("player must have Transform");
    let obstacle_t = world
        .get_component::<Transform>(obstacle)
        .expect("obstacle must have Transform");

    assert!(
        (player_t.translation.x - (-0.5)).abs() < f32::EPSILON,
        "player x must be -0.5, got {}",
        player_t.translation.x
    );
    assert!(
        (obstacle_t.translation.x - 0.5).abs() < f32::EPSILON,
        "obstacle x must be 0.5, got {}",
        obstacle_t.translation.x
    );
}

#[test]
fn json_with_player_marker_filter_yields_only_player() {
    let scene = load_scene_fixture(TEST_SCENE_JSON).expect("test JSON must be valid");
    let mut world = World::new();
    let _bridge = spawn_from_authoring_scene(&mut world, &scene).expect("valid scene must bridge");

    let q = world
        .query_filtered::<&Transform, With<PlayerMarker>>()
        .expect("query must build");
    assert_eq!(
        q.iter().count(),
        1,
        "only the player from JSON must match With<PlayerMarker>"
    );
}

#[test]
fn nav_agent_and_runtime_metadata_convert_from_authoring_defaults() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "ally_one",
                "components": {
                    "engine.transform": {"x": 0.0, "y": 0.0, "z": 0.0},
                    "engine.nav_mesh_agent": {
                        "speed": 4.0,
                        "stopping_distance": 0.25,
                        "has_target": true,
                        "target_x": 3.0,
                        "target_y": 0.0,
                        "target_z": -2.0
                    },
                    "engine.runtime_metadata": {
                        "name": "",
                        "tags": ["ally", "fighter"],
                        "team": "heroes"
                    }
                }
            }]
        }"#,
    )
    .expect("component fixture must load");
    let mut world = World::new();

    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("authorable navigation and metadata must convert");
    let authoring_id = scene
        .entities()
        .next()
        .map(|(id, _)| id)
        .expect("fixture entity");
    let entity = bridge.get(authoring_id).expect("runtime entity mapping");
    let agent = world
        .get_component::<NavMeshAgent>(entity)
        .expect("navigation agent");
    assert_eq!(agent.speed, 4.0);
    assert_eq!(agent.stopping_distance, 0.25);
    assert_eq!(agent.target, Some(Vec3::new(3.0, 0.0, -2.0)));
    let metadata = world
        .get_component::<RuntimeMetadata>(entity)
        .expect("runtime metadata");
    assert_eq!(metadata.name, "ally_one");
    assert_eq!(metadata.tags, ["ally", "fighter"]);
    assert_eq!(metadata.team, "heroes");

    world.insert_resource(crate::time::FixedTime::with_delta(0.5));
    let nav_mesh = crate::navmesh::bake_from_obstacles(
        &[],
        &crate::navmesh::NavMeshSettings {
            cell_size: 1.0,
            agent_radius: 0.0,
            world_min: Vec3::new(-5.0, 0.0, -5.0),
            world_max: Vec3::new(5.0, 0.0, 5.0),
            ..crate::navmesh::NavMeshSettings::default()
        },
    );
    world.insert_resource(crate::navmesh::NavMeshQuery::new(nav_mesh));
    let mut system = crate::navmesh::nav_mesh_agent_system
        .into_system()
        .expect("navigation system");
    system.run(&mut world).expect("navigation update");
    assert_ne!(
        world
            .get_component::<Transform>(entity)
            .expect("transform")
            .translation,
        Vec3::ZERO,
        "the owning runtime system must execute the authorable component"
    );
}

#[test]
fn unified_animation_controller_expands_to_runtime_rig_animator_and_graph() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let source_path = directory.path().join("character.gltf");
    std::fs::write(&source_path, engine_import::test_fixtures::SKINNED_GLTF)
        .expect("glTF fixture must be written");
    let source = AssetId::generate();
    let imported = crate::gltf_import::import_gltf_path(&source, &source_path, &[])
        .expect("fixture catalog must import");
    let skeleton = imported.skins[0].skeleton_id.clone();
    let graph = AssetId::generate();
    let animation_set_id = AssetId::generate();
    let motion_slot = engine_authoring::MotionSlotId::generate();
    std::fs::write(
        directory.path().join("locomotion.graph.json"),
        engine_animation::test_fixtures::valid_graph_json_for_motion_slot(&motion_slot),
    )
    .expect("animation graph fixture must be written");
    let mut animation_set = engine_authoring::AnimationSet::new(graph.clone());
    animation_set.bindings.insert(
        motion_slot,
        engine_authoring::AnimationBinding {
            name: "spin".to_owned(),
            clip: engine_authoring::MotionSourceRef::native(imported.animations[0].id.clone()),
            overlays: Vec::new(),
            events: vec![engine_authoring::AnimationSetEvent {
                time: 0.1,
                name: "attack.active".to_owned(),
            }],
        },
    );
    std::fs::write(
        directory.path().join("locomotion.animset.json"),
        animation_set
            .to_canonical_json()
            .expect("animation set fixture must serialize"),
    )
    .expect("animation set fixture must be written");
    let mut manifest = AssetManifest::default();
    manifest.insert(
        source.clone(),
        crate::asset::ManifestEntry {
            path: "character.gltf".into(),
            name: Some("character".into()),
            import_settings: crate::asset::ImportSettings {
                sub_assets: imported.imported_sub_assets(),
                skeleton_records: imported.skeleton_records,
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        graph.clone(),
        crate::asset::ManifestEntry {
            path: "locomotion.graph.json".into(),
            name: Some("locomotion".into()),
            import_settings: crate::asset::ImportSettings::default(),
        },
    );
    manifest.insert(
        animation_set_id.clone(),
        crate::asset::ManifestEntry {
            path: "locomotion.animset.json".into(),
            name: Some("locomotion_set".into()),
            import_settings: crate::asset::ImportSettings::default(),
        },
    );
    let scene_json = serde_json::json!({
        "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "animated_character",
            "components": {
                (SKINNED_MODEL_COMPONENT): {
                    "skeleton": {"$type": "asset_ref", "id": skeleton.as_str()}
                },
                (ANIMATION_CONTROLLER_COMPONENT): {
                    "animation_set": {"$type": "asset_ref", "id": animation_set_id.as_str()},
                    "graph": {"$type": "asset_ref", "id": graph.as_str()},
                    "looping": true,
                    "playback_speed": 1.5,
                    "completion_event": "",
                    "root_motion_mode": "extracted_only",
                    "fade_duration": 0.15,
                    "parameters": {"running": false}
                }
            }
        }]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture must load");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("unified animation controller must convert");
    let authoring_id = scene.entities().next().expect("fixture entity").0;
    let entity = bridge.get(authoring_id).expect("runtime entity mapping");
    assert!(world.has_component::<crate::skinning::Skeleton>(entity));
    assert!(world.has_component::<Animator>(entity));
    assert!(world.has_component::<AnimGraphPlayer>(entity));
    let animator = world.get_component::<Animator>(entity).expect("animator");
    assert_eq!(animator.playback_speed, 1.5);
    assert_eq!(animator.clip_events(animator.clip).len(), 1);
    let player = world
        .get_component::<AnimGraphPlayer>(entity)
        .expect("graph player");
    assert_eq!(
        player.parameter_value("running"),
        Some(crate::AnimationParameterValue::Bool(false))
    );
}

#[test]
fn incomplete_animation_controller_keeps_its_runtime_skeleton_in_best_effort_preview() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let source_path = directory.path().join("character.gltf");
    std::fs::write(&source_path, engine_import::test_fixtures::SKINNED_GLTF)
        .expect("glTF fixture must be written");
    let source = AssetId::generate();
    let imported = crate::gltf_import::import_gltf_path(&source, &source_path, &[])
        .expect("fixture catalog must import");
    let skeleton = imported.skins[0].skeleton_id.clone();
    let graph = AssetId::generate();
    let mut manifest = AssetManifest::default();
    manifest.insert(
        source.clone(),
        crate::asset::ManifestEntry {
            path: "character.gltf".into(),
            name: Some("character".into()),
            import_settings: crate::asset::ImportSettings {
                sub_assets: imported.imported_sub_assets(),
                skeleton_records: imported.skeleton_records,
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    let scene_json = serde_json::json!({
        "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "incomplete_character",
            "components": {
                (SKINNED_MODEL_COMPONENT): {
                    "skeleton": {"$type": "asset_ref", "id": skeleton.as_str()}
                },
                (ANIMATION_CONTROLLER_COMPONENT): {
                    "graph": {"$type": "asset_ref", "id": graph.as_str()},
                    "looping": true,
                    "playback_speed": 1.0,
                    "completion_event": "animation.completed",
                    "root_motion_mode": "disabled",
                    "fade_duration": 0.2,
                    "parameters": {}
                }
            }
        }]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture must load");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene_best_effort(&mut world, &scene)
        .expect("best-effort preview must preserve the valid rig");
    let authoring_id = scene.entities().next().expect("fixture entity").0;
    let entity = bridge.get(authoring_id).expect("runtime entity mapping");

    assert!(world.has_component::<crate::skinning::Skeleton>(entity));
    assert!(!world.has_component::<Animator>(entity));
    assert!(!world.has_component::<AnimGraphPlayer>(entity));
    assert!(bridge.asset_diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "scene_bridge.component_skipped"
            && diagnostic.message.contains("Animation Set")
    }));
}

/// Builds two independent imports of the same fixture GLB under different
/// source IDs, so they resolve to two distinct (but structurally identical)
/// [`crate::skeleton_asset::SkeletonAsset`]s — enough to exercise ADR 0079's
/// cross-skeleton resolution end to end without needing two handmade rigs.
fn independently_imported_fixture_pair(
    directory: &std::path::Path,
) -> (
    AssetId,
    crate::model_import::GltfImportResult,
    std::path::PathBuf,
    AssetId,
    crate::model_import::GltfImportResult,
    std::path::PathBuf,
) {
    let fixture = engine_import::test_fixtures::three_clip_character_glb();
    let hero_path = directory.join("hero.glb");
    let villain_path = directory.join("villain.glb");
    std::fs::write(&hero_path, &fixture).expect("hero fixture must write");
    std::fs::write(&villain_path, &fixture).expect("villain fixture must write");

    let hero_source = AssetId::generate();
    let villain_source = AssetId::generate();
    let hero_imported = crate::gltf_import::import_gltf_path(&hero_source, &hero_path, &[])
        .expect("hero fixture must import");
    let villain_imported =
        crate::gltf_import::import_gltf_path(&villain_source, &villain_path, &[])
            .expect("villain fixture must import");

    (
        hero_source,
        hero_imported,
        hero_path,
        villain_source,
        villain_imported,
        villain_path,
    )
}

/// Builds a villain-rigged character that plays one clip imported from the
/// hero source, so conversion has to resolve a cross-skeleton clip (ADR 0079).
///
/// The graph and Animation Set that carry the binding are written next to the
/// model fixtures and registered in `manifest`, because a Graph plus an
/// Animation Set is the only way to name a clip on a controller (ADR 0085).
fn cross_skeleton_scene_json(
    directory: &std::path::Path,
    manifest: &mut AssetManifest,
    hero_imported: &crate::model_import::GltfImportResult,
    villain_imported: &crate::model_import::GltfImportResult,
) -> serde_json::Value {
    let graph_id = AssetId::generate();
    let set_id = AssetId::generate();
    let motion_slot = engine_authoring::MotionSlotId::generate();
    std::fs::write(
        directory.join("cross_skeleton.graph.json"),
        engine_animation::test_fixtures::valid_graph_json_for_motion_slot(&motion_slot),
    )
    .expect("cross-skeleton graph fixture must write");
    let mut animation_set = engine_authoring::AnimationSet::new(graph_id.clone());
    animation_set.bindings.insert(
        motion_slot,
        engine_authoring::AnimationBinding {
            name: "hero_attack".to_owned(),
            clip: engine_authoring::MotionSourceRef::native(hero_imported.animations[1].id.clone()),
            overlays: Vec::new(),
            events: Vec::new(),
        },
    );
    std::fs::write(
        directory.join("cross_skeleton.animset.json"),
        animation_set
            .to_canonical_json()
            .expect("cross-skeleton animation set must serialize"),
    )
    .expect("cross-skeleton animation set fixture must write");
    for (id, path, name) in [
        (
            &graph_id,
            "cross_skeleton.graph.json",
            "cross_skeleton_graph",
        ),
        (&set_id, "cross_skeleton.animset.json", "cross_skeleton_set"),
    ] {
        manifest.insert(
            id.clone(),
            crate::asset::ManifestEntry {
                path: path.to_owned(),
                name: Some(name.to_owned()),
                import_settings: crate::asset::ImportSettings::default(),
            },
        );
    }

    serde_json::json!({
        "entities": [
            {
                "id": "entity_01JP0000000000000000000001",
                "name": "villain",
                "components": {
                    (SKINNED_MODEL_COMPONENT): {
                        "skeleton": {"$type": "asset_ref", "id": villain_imported.skins[0].skeleton_id.as_str()}
                    },
                    (ANIMATION_CONTROLLER_COMPONENT): {
                        "animation_set": {"$type": "asset_ref", "id": set_id.as_str()},
                        "graph": {"$type": "asset_ref", "id": graph_id.as_str()},
                        "looping": false,
                        "playback_speed": 1.0,
                            "completion_event": "",
                        "root_motion_mode": "disabled",
                        "fade_duration": 0.2,
                        "parameters": {}
                    }
                }
            },
            {
                "id": "entity_01JP0000000000000000000002",
                "name": "villain_mesh",
                "parent": "entity_01JP0000000000000000000001",
                "components": {
                    (SKINNED_MESH_RENDERER_COMPONENT): {
                        "mesh": {"$type": "asset_ref", "id": villain_imported.meshes[0].id.as_str()},
                        "model": {"$type": "entity_ref", "id": "entity_01JP0000000000000000000001"},
                        "material": {"$type": "asset_ref", "id": BUILTIN_WHITE_MATERIAL_ASSET_ID},
                        "material_slots": []
                    }
                }
            }
        ]
    })
}

#[test]
fn retarget_skeleton_lookup_imports_manifest_source_without_scene_entity() {
    let directory = tempfile::tempdir().expect("temp asset root");
    let (source_id, imported, _source_path, _, _, _) =
        independently_imported_fixture_pair(directory.path());
    let expected = imported.skins[0].skeleton.clone();

    let mut manifest = AssetManifest::default();
    manifest.insert(
        source_id.clone(),
        crate::asset::ManifestEntry {
            path: "hero.glb".into(),
            name: Some("hero".into()),
            import_settings: crate::asset::ImportSettings {
                sub_assets: imported.imported_sub_assets(),
                skeleton_records: imported.skeleton_records.clone(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );

    let mut asset_state = BridgeAssetState::default();
    let resolved = resolve_retarget_skeleton_asset(
        &expected.id,
        Some(directory.path()),
        &manifest,
        &mut asset_state,
    )
    .expect("the manifest skeleton ledger must locate and import the source model");

    assert_eq!(resolved.id, expected.id);
    assert!(
        asset_state.gltf_imports.contains_key(&source_id),
        "the on-demand source import must be cached for the rest of conversion"
    );
}

#[test]
fn cross_skeleton_resolution_rebinds_stale_asset_id_when_identity_matches_target() {
    let directory = tempfile::tempdir().expect("temp asset root");
    let (source_id, imported, _, _, _, _) =
        independently_imported_fixture_pair(directory.path());
    let target_skeleton = imported.skins[0].skeleton.clone();

    let stale_skeleton_id = AssetId::generate();
    let mut clip = imported.animations[0].clip.clone();
    clip.skeleton = Some(stale_skeleton_id.clone());
    clip.skeleton_identity = Some(target_skeleton.identity);

    let mut clips = Assets::<AnimationClip>::new();
    let clip_handle = clips.add(clip.clone());
    let mut asset_state = BridgeAssetState::default();
    asset_state
        .gltf_imports
        .insert(source_id, std::sync::Arc::new(imported));

    let resolved = resolve_cross_skeleton_clip(
        &clip,
        &clip_handle,
        &stale_skeleton_id,
        &target_skeleton.id,
        Some(directory.path()),
        &AssetManifest::default(),
        &mut asset_state,
        None,
    )
    .expect("matching skeleton identities must rebind without a retarget map");

    assert_eq!(resolved.skeleton.as_ref(), Some(&target_skeleton.id));
    assert_eq!(
        resolved.skeleton_identity,
        Some(target_skeleton.identity)
    );
    assert_eq!(resolved.channels.len(), clip.channels.len());
}

#[test]
fn animator_clip_from_a_different_skeleton_resolves_through_a_registered_retarget_map() {
    let directory = tempfile::tempdir().expect("temp asset root");
    let (hero_source, hero_imported, hero_path, villain_source, villain_imported, villain_path) =
        independently_imported_fixture_pair(directory.path());

    let hero_skeleton = hero_imported.skins[0].skeleton.clone();
    let villain_skeleton = villain_imported.skins[0].skeleton.clone();
    assert_ne!(
        hero_skeleton.id, villain_skeleton.id,
        "independently imported sources must not dedupe to the same skeleton id"
    );

    let mut map = crate::retarget::generate_retarget_map(&hero_skeleton, &villain_skeleton);
    assert!(
        !map.bone_pairs.is_empty(),
        "identical rigs must produce a full name-matched mapping"
    );
    // The fixture's clips carry no root translation channel (no `root_bone`
    // is auto-detected), so `TranslationMode::None` is used here: this test
    // exercises the ADR 0079 §4 resolution *path* end to end, and the
    // HipHeightRatio math itself already has dedicated coverage in
    // `crate::retarget`'s tests.
    map.translation = crate::retarget::TranslationPolicy {
        mode: crate::retarget::TranslationMode::None,
        scale: crate::retarget::TranslationScale::Manual(1.0),
    };
    std::fs::write(
        directory.path().join("hero_to_villain.retarget.json"),
        map.to_json().expect("map must serialize"),
    )
    .expect("map fixture must write");

    let hero_fingerprint =
        crate::gltf_import::fingerprint_gltf_source(&hero_path, &[]).expect("hero fingerprint");
    let villain_fingerprint = crate::gltf_import::fingerprint_gltf_source(&villain_path, &[])
        .expect("villain fingerprint");

    let mut manifest = AssetManifest::default();
    manifest.insert(
        hero_source.clone(),
        crate::asset::ManifestEntry {
            path: "hero.glb".into(),
            name: Some("hero".into()),
            import_settings: crate::asset::ImportSettings {
                source_fingerprint: Some(hero_fingerprint),
                sub_assets: hero_imported.imported_sub_assets(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        villain_source.clone(),
        crate::asset::ManifestEntry {
            path: "villain.glb".into(),
            name: Some("villain".into()),
            import_settings: crate::asset::ImportSettings {
                source_fingerprint: Some(villain_fingerprint),
                sub_assets: villain_imported.imported_sub_assets(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        AssetId::generate(),
        crate::asset::ManifestEntry {
            path: "hero_to_villain.retarget.json".into(),
            name: Some("hero_to_villain".into()),
            import_settings: crate::asset::ImportSettings::default(),
        },
    );

    let scene_json = cross_skeleton_scene_json(
        directory.path(),
        &mut manifest,
        &hero_imported,
        &villain_imported,
    );
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture must load");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("cross-skeleton animator must still convert (clip resolved via retarget map)");
    assert!(
        !bridge
            .asset_diagnostics
            .iter()
            .any(|d| d.code == crate::retarget::RETARGET_MAP_MISSING_DIAGNOSTIC),
        "a registered retarget map must resolve the clip: {:?}",
        bridge.asset_diagnostics
    );
    let entity = bridge
        .get(scene.entities().next().expect("authoring entity").0)
        .expect("runtime entity");
    let animator = world
        .get_component::<Animator>(entity)
        .expect("animator must remain attached after a successful retarget");
    let clip = world
        .get_resource::<Assets<AnimationClip>>()
        .and_then(|clips| clips.get(&animator.clip))
        .expect("resolved clip must exist");
    assert_eq!(
        clip.skeleton,
        Some(villain_skeleton.id),
        "the resolved clip must be bound to the entity's own (villain) skeleton"
    );
}

#[test]
fn animation_set_resolves_and_retargets_clips_from_multiple_model_sources() {
    let directory = tempfile::tempdir().expect("temp asset root");
    let (hero_source, hero_imported, hero_path, villain_source, villain_imported, villain_path) =
        independently_imported_fixture_pair(directory.path());
    let hero_skeleton = hero_imported.skins[0].skeleton.clone();
    let villain_skeleton = villain_imported.skins[0].skeleton.clone();

    let mut map = crate::retarget::generate_retarget_map(&hero_skeleton, &villain_skeleton);
    map.translation = crate::retarget::TranslationPolicy {
        mode: crate::retarget::TranslationMode::None,
        scale: crate::retarget::TranslationScale::Manual(1.0),
    };
    std::fs::write(
        directory.path().join("hero_to_villain.retarget.json"),
        map.to_json().expect("map must serialize"),
    )
    .expect("map fixture must write");

    let graph_id = AssetId::generate();
    let set_id = AssetId::generate();
    let idle_slot = engine_authoring::MotionSlotId::generate();
    let attack_slot = engine_authoring::MotionSlotId::generate();
    std::fs::write(
        directory.path().join("cross_source.graph.json"),
        engine_animation::test_fixtures::valid_graph_json_for_motion_slots(&idle_slot, &attack_slot),
    )
    .expect("graph fixture must write");
    let mut animation_set = engine_authoring::AnimationSet::new(graph_id.clone());
    animation_set.bindings.insert(
        idle_slot,
        engine_authoring::AnimationBinding {
            name: "villain_idle".to_owned(),
            clip: engine_authoring::MotionSourceRef::native(villain_imported.animations[0].id.clone()),
            overlays: Vec::new(),
            events: Vec::new(),
        },
    );
    animation_set.bindings.insert(
        attack_slot,
        engine_authoring::AnimationBinding {
            name: "hero_attack".to_owned(),
            clip: engine_authoring::MotionSourceRef::native(hero_imported.animations[1].id.clone()),
            overlays: Vec::new(),
            events: Vec::new(),
        },
    );
    std::fs::write(
        directory.path().join("cross_source.animset.json"),
        animation_set
            .to_canonical_json()
            .expect("animation set must serialize"),
    )
    .expect("animation set fixture must write");

    let mut manifest = AssetManifest::default();
    for (source, imported, path, name) in [
        (&hero_source, &hero_imported, &hero_path, "hero"),
        (&villain_source, &villain_imported, &villain_path, "villain"),
    ] {
        manifest.insert(
            source.clone(),
            crate::asset::ManifestEntry {
                path: path
                    .file_name()
                    .expect("fixture has a file name")
                    .to_string_lossy()
                    .into_owned(),
                name: Some(name.to_owned()),
                import_settings: crate::asset::ImportSettings {
                    source_fingerprint: Some(
                        crate::gltf_import::fingerprint_gltf_source(path, &[])
                            .expect("source fingerprint"),
                    ),
                    sub_assets: imported.imported_sub_assets(),
                    skeleton_records: imported.skeleton_records.clone(),
                    ..crate::asset::ImportSettings::default()
                },
            },
        );
    }
    for (id, path, name) in [
        (&graph_id, "cross_source.graph.json", "cross_source_graph"),
        (&set_id, "cross_source.animset.json", "cross_source_set"),
    ] {
        manifest.insert(
            id.clone(),
            crate::asset::ManifestEntry {
                path: path.to_owned(),
                name: Some(name.to_owned()),
                import_settings: crate::asset::ImportSettings::default(),
            },
        );
    }
    manifest.insert(
        AssetId::generate(),
        crate::asset::ManifestEntry {
            path: "hero_to_villain.retarget.json".into(),
            name: Some("hero_to_villain".into()),
            import_settings: crate::asset::ImportSettings::default(),
        },
    );

    let scene_json = serde_json::json!({
        "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "cross_source_character",
            "components": {
                (SKINNED_MODEL_COMPONENT): {
                    "skeleton": {"$type": "asset_ref", "id": villain_imported.skins[0].skeleton_id.as_str()}
                },
                (ANIMATION_CONTROLLER_COMPONENT): {
                    "animation_set": {"$type": "asset_ref", "id": set_id.as_str()},
                    "graph": {"$type": "asset_ref", "id": graph_id.as_str()},
                    "looping": true,
                    "playback_speed": 1.0,
                    "completion_event": "",
                    "root_motion_mode": "disabled",
                    "fade_duration": 0.2,
                    "parameters": {"switch": false}
                }
            }
        }]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture must load");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("cross-source Animation Set must convert");
    let entity = bridge
        .get(scene.entities().next().expect("authoring entity").0)
        .expect("runtime entity");
    let handles = world
        .get_component::<AnimGraphPlayer>(entity)
        .expect("graph player")
        .clip_bindings()
        .map(|(_, handle)| handle)
        .collect::<Vec<_>>();
    assert_eq!(handles.len(), 2, "both source clips must be bound");
    let clips = world
        .get_resource::<Assets<AnimationClip>>()
        .expect("animation assets");
    assert!(handles.iter().all(|handle| {
        clips.get(handle).and_then(|clip| clip.skeleton.as_ref()) == Some(&villain_skeleton.id)
    }));
}

#[test]
fn animator_clip_from_a_different_skeleton_without_a_retarget_map_is_not_applied() {
    let directory = tempfile::tempdir().expect("temp asset root");
    let (hero_source, hero_imported, hero_path, villain_source, villain_imported, villain_path) =
        independently_imported_fixture_pair(directory.path());

    let hero_fingerprint =
        crate::gltf_import::fingerprint_gltf_source(&hero_path, &[]).expect("hero fingerprint");
    let villain_fingerprint = crate::gltf_import::fingerprint_gltf_source(&villain_path, &[])
        .expect("villain fingerprint");

    let mut manifest = AssetManifest::default();
    manifest.insert(
        hero_source.clone(),
        crate::asset::ManifestEntry {
            path: "hero.glb".into(),
            name: Some("hero".into()),
            import_settings: crate::asset::ImportSettings {
                source_fingerprint: Some(hero_fingerprint),
                sub_assets: hero_imported.imported_sub_assets(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        villain_source.clone(),
        crate::asset::ManifestEntry {
            path: "villain.glb".into(),
            name: Some("villain".into()),
            import_settings: crate::asset::ImportSettings {
                source_fingerprint: Some(villain_fingerprint),
                sub_assets: villain_imported.imported_sub_assets(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    // Deliberately no `*.retarget.json` registered for this pair.

    let scene_json = cross_skeleton_scene_json(
        directory.path(),
        &mut manifest,
        &hero_imported,
        &villain_imported,
    );
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture must load");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("a missing retarget map must not abort scene conversion");
    assert!(
        bridge
            .asset_diagnostics
            .iter()
            .any(|d| d.code == crate::retarget::RETARGET_MAP_MISSING_DIAGNOSTIC),
        "a missing map must be reported: {:?}",
        bridge.asset_diagnostics
    );
    let entity = bridge
        .get(scene.entities().next().expect("authoring entity").0)
        .expect("runtime entity");
    assert!(
        world.get_component::<Animator>(entity).is_none(),
        "the Animator must be removed rather than play a clip authored for a different skeleton"
    );
}

#[test]
fn animator_clip_from_a_source_without_a_fingerprint_blocks_retarget_resolution() {
    let directory = tempfile::tempdir().expect("temp asset root");
    let (hero_source, hero_imported, _hero_path, villain_source, villain_imported, villain_path) =
        independently_imported_fixture_pair(directory.path());

    let hero_skeleton = hero_imported.skins[0].skeleton.clone();
    let villain_skeleton = villain_imported.skins[0].skeleton.clone();

    let map = crate::retarget::generate_retarget_map(&hero_skeleton, &villain_skeleton);
    std::fs::write(
        directory.path().join("hero_to_villain.retarget.json"),
        map.to_json().expect("map must serialize"),
    )
    .expect("map fixture must write");

    let villain_fingerprint = crate::gltf_import::fingerprint_gltf_source(&villain_path, &[])
        .expect("villain fingerprint");

    let mut manifest = AssetManifest::default();
    manifest.insert(
        hero_source.clone(),
        crate::asset::ManifestEntry {
            path: "hero.glb".into(),
            name: Some("hero".into()),
            import_settings: crate::asset::ImportSettings {
                // Deliberately no `source_fingerprint`, simulating a source
                // imported before fingerprints were recorded (AP-6).
                source_fingerprint: None,
                sub_assets: hero_imported.imported_sub_assets(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        villain_source.clone(),
        crate::asset::ManifestEntry {
            path: "villain.glb".into(),
            name: Some("villain".into()),
            import_settings: crate::asset::ImportSettings {
                source_fingerprint: Some(villain_fingerprint),
                sub_assets: villain_imported.imported_sub_assets(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        AssetId::generate(),
        crate::asset::ManifestEntry {
            path: "hero_to_villain.retarget.json".into(),
            name: Some("hero_to_villain".into()),
            import_settings: crate::asset::ImportSettings::default(),
        },
    );

    let scene_json = cross_skeleton_scene_json(
        directory.path(),
        &mut manifest,
        &hero_imported,
        &villain_imported,
    );
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture must load");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("an unfingerprinted source must not abort scene conversion");
    assert!(
        bridge
            .asset_diagnostics
            .iter()
            .any(|d| d.code == crate::retarget::RETARGET_SOURCE_UNFINGERPRINTED_DIAGNOSTIC),
        "a missing fingerprint must block resolution with a diagnostic: {:?}",
        bridge.asset_diagnostics
    );
    let entity = bridge
        .get(scene.entities().next().expect("authoring entity").0)
        .expect("runtime entity");
    assert!(
        world.get_component::<Animator>(entity).is_none(),
        "the Animator must be removed rather than resolve under a same-session-only cache key"
    );
    assert!(
        !directory.path().join(".engine").join("cache").exists(),
        "an unfingerprinted source must never bake into the derived-clip cache"
    );
}

/// Sets up the same hero/villain rig pair, retarget map, and manifest as
/// [`animator_clip_from_a_different_skeleton_resolves_through_a_registered_retarget_map`],
/// but rooted so `<directory>/assets` is the assets root and `<directory>`
/// itself plays the part of a package root — letting a
/// [`crate::retarget::PackagedBakedClips`] test point `baked_anim` at
/// `<directory>/baked_anim` and check `<directory>/.engine/cache` for an
/// (absent) cache write, mirroring the real `<package_root>/assets` +
/// `<package_root>/.engine/cache` + `<package_root>/baked_anim` layout.
#[allow(clippy::type_complexity)]
fn packaged_fixture(
    directory: &std::path::Path,
) -> (
    std::path::PathBuf,
    AssetId,
    crate::model_import::GltfImportResult,
    AssetId,
    crate::model_import::GltfImportResult,
    crate::retarget::RetargetMap,
    AssetManifest,
    String,
) {
    let assets_root = directory.join("assets");
    std::fs::create_dir_all(&assets_root).expect("assets dir must be creatable");
    let (hero_source, hero_imported, hero_path, villain_source, villain_imported, villain_path) =
        independently_imported_fixture_pair(&assets_root);

    let hero_skeleton = hero_imported.skins[0].skeleton.clone();
    let villain_skeleton = villain_imported.skins[0].skeleton.clone();
    assert_ne!(
        hero_skeleton.id, villain_skeleton.id,
        "independently imported sources must not dedupe to the same skeleton id"
    );

    let mut map = crate::retarget::generate_retarget_map(&hero_skeleton, &villain_skeleton);
    assert!(!map.bone_pairs.is_empty());
    // Sidestep the fixture's lack of a root translation channel; see the
    // identical note on the non-packaged retarget resolution test above.
    map.translation = crate::retarget::TranslationPolicy {
        mode: crate::retarget::TranslationMode::None,
        scale: crate::retarget::TranslationScale::Manual(1.0),
    };
    std::fs::write(
        assets_root.join("hero_to_villain.retarget.json"),
        map.to_json().expect("map must serialize"),
    )
    .expect("map fixture must write");

    let hero_fingerprint =
        crate::gltf_import::fingerprint_gltf_source(&hero_path, &[]).expect("hero fingerprint");
    let villain_fingerprint = crate::gltf_import::fingerprint_gltf_source(&villain_path, &[])
        .expect("villain fingerprint");

    let mut manifest = AssetManifest::default();
    manifest.insert(
        hero_source.clone(),
        crate::asset::ManifestEntry {
            path: "hero.glb".into(),
            name: Some("hero".into()),
            import_settings: crate::asset::ImportSettings {
                source_fingerprint: Some(hero_fingerprint.clone()),
                sub_assets: hero_imported.imported_sub_assets(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        villain_source.clone(),
        crate::asset::ManifestEntry {
            path: "villain.glb".into(),
            name: Some("villain".into()),
            import_settings: crate::asset::ImportSettings {
                source_fingerprint: Some(villain_fingerprint),
                sub_assets: villain_imported.imported_sub_assets(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        AssetId::generate(),
        crate::asset::ManifestEntry {
            path: "hero_to_villain.retarget.json".into(),
            name: Some("hero_to_villain".into()),
            import_settings: crate::asset::ImportSettings::default(),
        },
    );

    (
        assets_root,
        hero_source,
        hero_imported,
        villain_source,
        villain_imported,
        map,
        manifest,
        hero_fingerprint,
    )
}

#[test]
fn packaged_baked_clips_resolves_a_staged_clip_without_baking_or_caching() {
    let directory = tempfile::tempdir().expect("temp package root");
    let (
        assets_root,
        _hero_source,
        hero_imported,
        _villain_source,
        villain_imported,
        map,
        mut manifest,
        hero_fingerprint,
    ) = packaged_fixture(directory.path());

    let hero_skeleton = hero_imported.skins[0].skeleton.clone();
    let villain_skeleton = villain_imported.skins[0].skeleton.clone();
    let hero_attack = hero_imported
        .animations
        .iter()
        .find(|animation| animation.name == "attack")
        .expect("fixture must contain an `attack` clip")
        .clone();

    // Bake through the same pure function the bake-or-cache path uses, but
    // write the result directly under `baked_anim/` (never through
    // `resolve_or_bake_retargeted_clip`, which would also write the derived
    // cache) — exactly what packaging's bake walk stages.
    let baked = crate::retarget::retarget_clip(
        &hero_attack.clip,
        &hero_skeleton,
        &villain_skeleton,
        &map,
        &[],
    )
    .expect("retarget must succeed for a name-matched rig pair");
    let key = crate::retarget::cache_key_for_retargeted_clip(
        &hero_fingerprint,
        hero_attack.id.as_str(),
        hero_skeleton.identity,
        villain_skeleton.identity,
        &map,
        &[],
    )
    .expect("cache key must compute");
    let baked_anim_dir = directory.path().join("baked_anim");
    std::fs::create_dir_all(&baked_anim_dir).expect("baked_anim dir must be creatable");
    let file_name = format!(
        "{}.{}",
        key.file_stem(),
        crate::retarget::BAKED_CLIP_FILE_EXTENSION
    );
    std::fs::write(
        baked_anim_dir.join(&file_name),
        crate::retarget::serialize_baked_clip(&baked).expect("baked clip must serialize"),
    )
    .expect("baked clip fixture must write");

    let scene_json = cross_skeleton_scene_json(
        &assets_root,
        &mut manifest,
        &hero_imported,
        &villain_imported,
    );
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture must load");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(&assets_root));
    world.insert_resource(manifest);
    world.insert_resource(crate::retarget::PackagedBakedClips {
        root: baked_anim_dir,
    });

    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("a staged packaged clip must resolve without error");
    assert!(
        !bridge.asset_diagnostics.iter().any(|d| {
            d.code == crate::retarget::RETARGET_BAKE_MISSING_FROM_PACKAGE_DIAGNOSTIC
                || d.code == crate::retarget::RETARGET_MAP_MISSING_DIAGNOSTIC
        }),
        "a staged baked clip must resolve cleanly: {:?}",
        bridge.asset_diagnostics
    );
    let entity = bridge
        .get(scene.entities().next().expect("authoring entity").0)
        .expect("runtime entity");
    let animator = world
        .get_component::<Animator>(entity)
        .expect("animator must remain attached after a successful packaged resolution");
    let clip = world
        .get_resource::<Assets<AnimationClip>>()
        .and_then(|clips| clips.get(&animator.clip))
        .expect("resolved clip must exist");
    assert_eq!(
        clip.skeleton,
        Some(villain_skeleton.id),
        "the resolved clip must be the retargeted (villain-bound) clip loaded from the package"
    );
    assert!(
        !directory.path().join(".engine").join("cache").exists(),
        "the packaged resolution path must never write to the derived cache"
    );
}

#[test]
fn packaged_baked_clips_reports_a_missing_baked_clip_diagnostic() {
    let directory = tempfile::tempdir().expect("temp package root");
    let (
        assets_root,
        _hero_source,
        hero_imported,
        _villain_source,
        villain_imported,
        _map,
        mut manifest,
        _hero_fingerprint,
    ) = packaged_fixture(directory.path());

    // Deliberately do not stage any file under `baked_anim/`: the package
    // is incomplete (AP-7's reachability trace or `always_package` is wrong).
    let baked_anim_dir = directory.path().join("baked_anim");

    let scene_json = cross_skeleton_scene_json(
        &assets_root,
        &mut manifest,
        &hero_imported,
        &villain_imported,
    );
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture must load");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(&assets_root));
    world.insert_resource(manifest);
    world.insert_resource(crate::retarget::PackagedBakedClips {
        root: baked_anim_dir,
    });

    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("a missing packaged clip must not abort scene conversion");
    assert!(
        bridge
            .asset_diagnostics
            .iter()
            .any(|d| d.code == crate::retarget::RETARGET_BAKE_MISSING_FROM_PACKAGE_DIAGNOSTIC),
        "a missing packaged clip must be reported: {:?}",
        bridge.asset_diagnostics
    );
    let entity = bridge
        .get(scene.entities().next().expect("authoring entity").0)
        .expect("runtime entity");
    assert!(
        world.get_component::<Animator>(entity).is_none(),
        "the Animator must be removed rather than play a clip that could not be loaded from the package"
    );
}

#[test]
fn authorable_behavior_tree_preserves_blackboard_and_executes() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let service = BehaviorTreeAuthoringService::new();
    let example = service.example().expect("Behavior Tree example must build");
    let graph_json = example
        .graph
        .to_canonical_json(service.domain())
        .expect("Behavior Tree graph must serialize");
    std::fs::write(directory.path().join("enemy.graph.json"), graph_json)
        .expect("Behavior Tree fixture must be written");
    let graph_asset = AssetId::generate();
    let mut manifest = AssetManifest::default();
    manifest.insert(
        graph_asset.clone(),
        crate::asset::ManifestEntry {
            path: "enemy.graph.json".into(),
            name: Some("enemy_behavior".into()),
            import_settings: crate::asset::ImportSettings::default(),
        },
    );
    let scene_json = serde_json::json!({
        "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "enemy",
            "components": {
                (BEHAVIOR_TREE_RUNNER_COMPONENT): {
                    "graph": {"$type": "asset_ref", "id": graph_asset.as_str()},
                    "blackboard": {"home_x": 4.0, "alert": false},
                    "enabled": true
                }
            }
        }]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture must load");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("Behavior Tree asset and runner must convert");
    let authoring_id = scene.entities().next().expect("fixture entity").0;
    let entity = bridge.get(authoring_id).expect("runtime entity mapping");
    let runner = world
        .get_component::<BehaviorTreeRunner>(entity)
        .expect("runtime Behavior Tree runner");
    assert_eq!(runner.blackboard().get("home_x"), Some(&Value::F64(4.0)));
    assert_eq!(runner.blackboard().get("alert"), Some(&Value::Bool(false)));

    let mut registry = crate::behavior_tree::BehaviorTreeBehaviorRegistry::default();
    registry
        .set_condition(
            "player_visible",
            crate::behavior_tree::BehaviorStatus::Success,
        )
        .set_action(
            "chase_player",
            crate::behavior_tree::BehaviorStatus::Success,
        )
        .set_action("patrol", crate::behavior_tree::BehaviorStatus::Success);
    world.insert_resource(registry);
    let mut system = crate::behavior_tree::behavior_tree_tick_system
        .into_system()
        .expect("Behavior Tree system");
    system.run(&mut world).expect("Behavior Tree update");
    assert_eq!(
        world
            .get_component::<BehaviorTreeRunner>(entity)
            .expect("runner after tick")
            .last_status(),
        Some(crate::behavior_tree::BehaviorStatus::Success)
    );
}

#[test]
fn authorable_audio_components_decode_once_and_execute_headless_state() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    std::fs::write(
        directory.path().join("tone.wav"),
        test_wav_bytes(),
    )
    .expect("WAV fixture must be written");
    let audio_asset = AssetId::generate();
    let mut manifest = AssetManifest::default();
    manifest.insert(
        audio_asset.clone(),
        crate::asset::ManifestEntry {
            path: "tone.wav".into(),
            name: Some("tone".into()),
            import_settings: crate::asset::ImportSettings::default(),
        },
    );
    let scene_json = serde_json::json!({
        "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "audio_host",
            "components": {
                (TRANSFORM_COMPONENT): {"x": 0.0, "y": 1.0, "z": 0.0},
                (AUDIO_EMITTER_COMPONENT): {
                    "clip": {"$type": "asset_ref", "id": audio_asset.as_str()},
                    "volume": 0.8,
                    "spatial_blend": 1.0,
                    "min_distance": 1.0,
                    "max_distance": 12.0,
                    "rolloff": "linear",
                    "looping": false,
                    "autoplay": true
                },
                (AUDIO_LISTENER_COMPONENT): {"enabled": true, "priority": 0},
                (MUSIC_CONTROLLER_COMPONENT): {
                    "clip": {"$type": "asset_ref", "id": audio_asset.as_str()},
                    "volume": 0.6,
                    "fade_in_seconds": 0.25,
                    "autoplay": true
                }
            }
        }]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture must load");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("audio assets and components must convert");
    let authoring_id = scene.entities().next().expect("fixture entity").0;
    let entity = bridge.get(authoring_id).expect("runtime entity mapping");
    assert!(bridge.asset(&audio_asset).is_some());
    assert!(
        world
            .get_component::<AudioListener>(entity)
            .expect("audio listener")
            .enabled
    );
    assert_eq!(world.get_resource::<Assets<AudioAsset>>().unwrap().len(), 1);

    let mut system = crate::audio::authored_audio_system
        .into_system()
        .expect("authored audio system");
    system.run(&mut world).expect("headless audio update");
    assert_eq!(
        world
            .get_component::<AudioEmitter>(entity)
            .expect("audio emitter")
            .state(),
        &crate::audio::AuthoredAudioState::Unavailable
    );
    assert_eq!(
        world
            .get_component::<MusicController>(entity)
            .expect("music controller")
            .state(),
        &crate::audio::AuthoredAudioState::Unavailable
    );
}

#[test]
fn registered_material_loads_color_and_decodes_texture_without_a_gpu() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let texture_asset = AssetId::generate();
    let material_asset = AssetId::generate();
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        1,
        image::Rgba([20, 40, 60, 255]),
    ));
    let mut png = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("PNG fixture");
    std::fs::write(directory.path().join("albedo.png"), png.into_inner()).expect("texture fixture");
    let material = engine_authoring::MaterialAsset {
        base_color: engine_authoring::LinearRgba {
            r: 0.25,
            g: 0.5,
            b: 0.75,
            a: 1.0,
        },
        base_color_texture: Some(texture_asset.clone()),
        normal_texture: Some(texture_asset.clone()),
        emissive_texture: Some(texture_asset.clone()),
        emissive_color: engine_authoring::LinearRgba {
            r: 2.0,
            g: 0.25,
            b: 0.1,
            a: 1.0,
        },
        roughness: 0.2,
        metallic: 0.8,
        alpha_mode: engine_authoring::MaterialAlphaMode::Mask,
        alpha_cutoff: 0.35,
        cull_mode: engine_authoring::MaterialCullMode::Front,
        shading_model: engine_authoring::MaterialShadingModel::Unlit,
        ..engine_authoring::MaterialAsset::default()
    };
    std::fs::write(
        directory.path().join("hero.material.json"),
        material.to_json().expect("material fixture JSON"),
    )
    .expect("material fixture");
    let mut manifest = AssetManifest::default();
    for (id, path) in [
        (texture_asset, "albedo.png"),
        (material_asset.clone(), "hero.material.json"),
    ] {
        manifest.insert(
            id,
            crate::asset::ManifestEntry {
                path: path.into(),
                name: None,
                import_settings: crate::asset::ImportSettings::default(),
            },
        );
    }
    let scene_json = serde_json::json!({
        "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "textured",
            "components": {
                (STATIC_MESH_RENDERER_COMPONENT): {
                    "mesh": {"$type": "asset_ref", "id": BUILTIN_TRIANGLE_ASSET_ID},
                    "material": {"$type": "asset_ref", "id": material_asset.as_str()},
                    "material_slots": []
                }
            }
        }]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("material conversion");
    let entity = bridge
        .get(scene.entities().next().expect("entity").0)
        .expect("mapped entity");
    let runtime = world
        .get_component::<Material>(entity)
        .expect("runtime material");

    assert_eq!(runtime.color, [0.25, 0.5, 0.75, 1.0]);
    assert_eq!(runtime.emissive_color, [2.0, 0.25, 0.1]);
    assert_eq!(runtime.roughness, 0.2);
    assert_eq!(runtime.metallic, 0.8);
    assert_eq!(runtime.alpha_mode, AlphaMode::Mask);
    assert_eq!(runtime.alpha_cutoff, 0.35);
    assert_eq!(runtime.cull_mode, CullMode::Front);
    assert_eq!(runtime.shading_model, ShadingModel::Unlit);
    assert!(runtime.texture.is_none());
    let decoded = runtime
        .pending_texture
        .as_ref()
        .expect("headless conversion keeps decoded pixels");
    assert_eq!((decoded.width, decoded.height), (2, 1));
    assert_eq!(decoded.rgba8.len(), 8);
    assert_eq!(
        runtime
            .pending_normal_texture
            .as_ref()
            .expect("normal pixels must remain deferred")
            .rgba8
            .len(),
        8
    );
    assert_eq!(
        runtime
            .pending_emissive_texture
            .as_ref()
            .expect("emissive pixels must remain deferred")
            .rgba8
            .len(),
        8
    );
}

#[test]
fn authored_lod_group_resolves_levels_and_switches_runtime_mesh() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "lod_mesh",
                "components": {
                    "engine.transform": {"x": 0.0, "y": 0.0, "z": -50.0},
                    "engine.lod_group": {"levels": [
                        {"distance": 10.0, "mesh": {"$type":"asset_ref","id":"asset_01JP0000000000000000000101"}},
                        {"distance": 100.0, "mesh": {"$type":"asset_ref","id":"asset_01JP0000000000000000000102"}}
                    ]}
                }
            }]
        }"#,
    )
    .expect("LOD scene fixture");
    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("LOD conversion");
    let entity = bridge
        .get(scene.entities().next().expect("fixture entity").0)
        .expect("runtime entity");
    let lod = world
        .get_component::<LodGroup>(entity)
        .expect("runtime LOD group");
    assert_eq!(lod.levels.len(), 2);
    let far_mesh = lod.levels[1].mesh;
    *world
        .get_component_mut::<GlobalTransform>(entity)
        .expect("runtime global transform") = GlobalTransform(glam::Mat4::from_translation(
        glam::Vec3::new(0.0, 0.0, -50.0),
    ));

    let camera = world.spawn().expect("camera entity");
    world
        .add_component(camera, Camera3D::default())
        .expect("camera component");
    world
        .add_component(camera, GlobalTransform::default())
        .expect("camera transform");
    let mut system = crate::lod::lod_selection_system
        .into_system()
        .expect("LOD system");
    system.run(&mut world).expect("LOD update");
    assert_eq!(
        *world
            .get_component::<Handle<Mesh>>(entity)
            .expect("selected mesh"),
        far_mesh
    );
}

/// Writes the skinned glTF fixture and registers it with the sub-asset
/// catalog a real import would produce.
/// Parses one of the fixed fixture entity IDs used by these tests.
fn entity_id(id: &str) -> EntityId {
    EntityId::from_stable_id(engine_authoring::StableId::new(id))
        .expect("fixture entity IDs are valid stable IDs")
}

fn skinned_fixture_manifest(
    directory: &std::path::Path,
    source: &AssetId,
) -> (AssetManifest, crate::model_import::GltfImportResult) {
    let path = directory.join("character.gltf");
    std::fs::write(&path, engine_import::test_fixtures::SKINNED_GLTF).expect("glTF fixture");
    let imported = crate::gltf_import::import_gltf_path(source, &path, &[])
        .expect("fixture import for catalog");
    let mut manifest = AssetManifest::default();
    manifest.insert(
        source.clone(),
        crate::asset::ManifestEntry {
            path: "character.gltf".into(),
            name: Some("character".into()),
            import_settings: crate::asset::ImportSettings {
                sub_assets: imported.imported_sub_assets(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    (manifest, imported)
}

/// Builds a scene with one model and one entity attached to `bone`.
fn bone_attachment_scene(skeleton: &AssetId, bone: i64) -> serde_json::Value {
    serde_json::json!({
        "entities": [
            {
                "id": "entity_01JP0000000000000000000001",
                "name": "character",
                "components": {
                    (SKINNED_MODEL_COMPONENT): {
                        "skeleton": {"$type": "asset_ref", "id": skeleton.as_str()}
                    }
                }
            },
            {
                "id": "entity_01JP0000000000000000000004",
                "name": "sword",
                "components": {
                    (BONE_ATTACHMENT_COMPONENT): {
                        "rig": {"$type": "entity_ref", "id": "entity_01JP0000000000000000000001"},
                        "bone": bone,
                        "bone_name": "tip_joint"
                    },
                    (TRANSFORM_COMPONENT): {"x": 0.0, "y": 0.5, "z": 0.0}
                }
            }
        ]
    })
}

#[test]
fn a_bone_attachment_reparents_its_entity_onto_the_joint() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let source = AssetId::generate();
    let (manifest, imported) = skinned_fixture_manifest(directory.path(), &source);
    let bone = imported.skins[0].skeleton.bones[1].id;
    let scene = load_scene_fixture(
        &bone_attachment_scene(&imported.skins[0].skeleton_id, i64::from(bone.0)).to_string(),
    )
    .expect("attachment scene");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("conversion");
    let model = bridge
        .get(&entity_id("entity_01JP0000000000000000000001"))
        .expect("model entity");
    let sword = bridge
        .get(&entity_id("entity_01JP0000000000000000000004"))
        .expect("sword entity");

    let joint = world
        .get_component::<crate::skinning::Skeleton>(model)
        .expect("model owns a rig")
        .joint_of(bone)
        .expect("the rig carries the bone");
    assert_eq!(
        world
            .get_component::<Parent>(sword)
            .expect("attachment must reparent")
            .0,
        joint
    );
    assert!(world
        .get_component::<Children>(joint)
        .expect("joint gains a child")
        .0
        .contains(&sword));

    // The entity's own transform is now an offset from the bone, so
    // propagation must place it relative to the joint rather than the world.
    crate::transform::transform_propagation_system(engine_ecs::Query::new(&mut world));
    let joint_world = world
        .get_component::<GlobalTransform>(joint)
        .expect("joint global transform")
        .matrix()
        .col(3)
        .truncate();
    let sword_world = world
        .get_component::<GlobalTransform>(sword)
        .expect("sword global transform")
        .matrix()
        .col(3)
        .truncate();
    assert!((sword_world - joint_world - Vec3::new(0.0, 0.5, 0.0)).length() < 1.0e-5);
}

#[test]
fn a_bone_the_rig_does_not_have_leaves_the_attached_entity_alone() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let source = AssetId::generate();
    let (manifest, imported) = skinned_fixture_manifest(directory.path(), &source);
    let scene = load_scene_fixture(
        &bone_attachment_scene(&imported.skins[0].skeleton_id, 9999).to_string(),
    )
    .expect("attachment scene");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("conversion");
    let sword = bridge
        .get(&entity_id("entity_01JP0000000000000000000004"))
        .expect("sword entity");

    assert!(
        world.get_component::<Parent>(sword).is_none(),
        "an unresolvable attachment must not move the entity"
    );
    assert!(bridge
        .asset_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "scene_bridge.bone_attachment_unresolved_bone"));
}

#[test]
fn an_unassigned_bone_is_an_editing_state_not_an_error() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let source = AssetId::generate();
    let (manifest, imported) = skinned_fixture_manifest(directory.path(), &source);
    let scene = load_scene_fixture(
        &bone_attachment_scene(&imported.skins[0].skeleton_id, -1).to_string(),
    )
    .expect("attachment scene");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("conversion");
    let sword = bridge
        .get(&entity_id("entity_01JP0000000000000000000004"))
        .expect("sword entity");

    assert!(world.get_component::<Parent>(sword).is_none());
    assert!(
        bridge
            .asset_diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.is_blocking()),
        "a component added but not yet pointed at a bone must not block Play"
    );
}

#[test]
fn a_skinned_renderer_uses_the_model_it_references() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let source = AssetId::generate();
    let (manifest, imported) = skinned_fixture_manifest(directory.path(), &source);
    let scene_json = serde_json::json!({
        "entities": [
            {
                "id": "entity_01JP0000000000000000000001",
                "name": "character",
                "components": {
                    (SKINNED_MODEL_COMPONENT): {
                        "skeleton": {"$type": "asset_ref", "id": imported.skins[0].skeleton_id.as_str()}
                    }
                }
            },
            {
                "id": "entity_01JP0000000000000000000002",
                "name": "body",
                "parent": "entity_01JP0000000000000000000001",
                "components": {
                    (SKINNED_MESH_RENDERER_COMPONENT): {
                        "mesh": {"$type": "asset_ref", "id": imported.meshes[0].id.as_str()},
                        "model": {"$type": "entity_ref", "id": "entity_01JP0000000000000000000001"},
                        "material": {"$type": "asset_ref", "id": BUILTIN_WHITE_MATERIAL_ASSET_ID},
                        "material_slots": []
                    }
                }
            }
        ]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("skinned model scene");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("conversion");
    let model = bridge
        .get(&entity_id("entity_01JP0000000000000000000001"))
        .expect("model entity");
    let part = bridge
        .get(&entity_id("entity_01JP0000000000000000000002"))
        .expect("part entity");

    let rig = world
        .get_component::<crate::skinning::Skeleton>(model)
        .expect("the model entity owns the rig");
    assert_eq!(rig.joints.len(), imported.skins[0].skeleton.bones.len());
    let rig_pose = world
        .get_component::<crate::rig_pose::RigPose>(model)
        .expect("the model entity owns its layered rig pose");
    assert_eq!(
        rig_pose.skeleton_asset(),
        &imported.skins[0].skeleton.id
    );
    assert_eq!(
        rig_pose.joint_count(),
        imported.skins[0].skeleton.bones.len()
    );
    assert!(
        world
            .get_component::<crate::skinning::Skeleton>(part)
            .is_none(),
        "a render part never owns a rig of its own"
    );
    assert!(
        world
            .get_component::<crate::rig_pose::RigPose>(part)
            .is_none(),
        "a render part never owns a pose separate from its model rig"
    );

    let binding = world
        .get_component::<crate::skinning::SkinnedMesh>(part)
        .expect("render part must be bound");
    assert_eq!(
        binding.rig, model,
        "the renderer's model reference, not hierarchy, binds it to its rig"
    );
    assert_eq!(binding.joint_bones, imported.skins[0].joint_bone_ids);
}

#[test]
fn a_renderer_with_no_model_stays_in_bind_pose() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let source = AssetId::generate();
    let (manifest, imported) = skinned_fixture_manifest(directory.path(), &source);
    let scene_json = serde_json::json!({
        "entities": [{
            "id": "entity_01JP0000000000000000000002",
            "name": "orphan",
            "components": {
                (SKINNED_MESH_RENDERER_COMPONENT): {
                    "mesh": {"$type": "asset_ref", "id": imported.meshes[0].id.as_str()},
                    "material": {"$type": "asset_ref", "id": BUILTIN_WHITE_MATERIAL_ASSET_ID},
                    "material_slots": []
                }
            }
        }]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("orphan part scene");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("conversion");
    let part = bridge
        .get(&entity_id("entity_01JP0000000000000000000002"))
        .expect("part entity");

    assert!(
        world
            .get_component::<crate::skinning::SkinnedMesh>(part)
            .is_none(),
        "an unowned part is inert rather than bound to an arbitrary rig"
    );
    assert!(bridge
        .asset_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == COMPONENT_INACTIVE_DIAGNOSTIC));
}

#[test]
fn a_renderer_can_reference_an_external_model() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let source = AssetId::generate();
    let (manifest, imported) = skinned_fixture_manifest(directory.path(), &source);
    // The weapon is owned by no model and names the character's rig directly,
    // which is the external-rig case ADR 0087 keeps available.
    let scene_json = serde_json::json!({
        "entities": [
            {
                "id": "entity_01JP0000000000000000000001",
                "name": "character",
                "components": {
                    (SKINNED_MODEL_COMPONENT): {
                        "skeleton": {"$type": "asset_ref", "id": imported.skins[0].skeleton_id.as_str()}
                    }
                }
            },
            {
                "id": "entity_01JP0000000000000000000003",
                "name": "weapon",
                "components": {
                    (SKINNED_MESH_RENDERER_COMPONENT): {
                        "mesh": {"$type": "asset_ref", "id": imported.meshes[0].id.as_str()},
                        "model": {"$type": "entity_ref", "id": "entity_01JP0000000000000000000001"},
                        "material": {"$type": "asset_ref", "id": BUILTIN_WHITE_MATERIAL_ASSET_ID},
                        "material_slots": []
                    }
                }
            }
        ]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("external rig scene");
    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(
        directory.path(),
    ));
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("conversion");
    let model = bridge
        .get(&entity_id("entity_01JP0000000000000000000001"))
        .expect("model entity");
    let weapon = bridge
        .get(&entity_id("entity_01JP0000000000000000000003"))
        .expect("weapon entity");

    assert_eq!(
        world
            .get_component::<crate::skinning::SkinnedMesh>(weapon)
            .expect("weapon must be bound")
            .rig,
        model
    );
}

#[test]
fn minimal_scene_resolves_mesh_and_material_assets() {
    let scene = load_scene_fixture(include_str!("../../assets/scenes/minimal.scene.json"))
        .expect("minimal scene JSON must be valid");
    let triangle =
        AssetId::from_stable_id(engine_authoring::StableId::new(BUILTIN_TRIANGLE_ASSET_ID))
            .expect("built-in triangle ID must be valid");
    let blue = AssetId::from_stable_id(engine_authoring::StableId::new(
        BUILTIN_BLUE_MATERIAL_ASSET_ID,
    ))
    .expect("built-in material ID must be valid");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("minimal scene must bridge");

    assert!(bridge.asset(&triangle).is_some());
    assert!(bridge.asset(&blue).is_some());
    let player_id = scene
        .entities()
        .find(|(_, entity)| entity.name == "player")
        .map(|(id, _)| id)
        .expect("player entity must exist");
    let player = bridge.get(player_id).expect("player must be in bridge");
    assert!(world.has_component::<Handle<Mesh>>(player));
    assert!(world.has_component::<Material>(player));
}

#[test]
fn stable_imported_mesh_id_loads_geometry_from_gltf_source() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let mut positions = Vec::new();
    for value in [0.0_f32, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 3.0, 0.0] {
        positions.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(directory.path().join("mesh.bin"), positions).expect("buffer fixture");
    std::fs::write(
        directory.path().join("hero.gltf"),
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"uri":"mesh.bin","byteLength":36}],
            "bufferViews":[{"buffer":0,"byteLength":36}],
            "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[2,3,0]}],
            "meshes":[{"name":"body","primitives":[{"attributes":{"POSITION":0}}]}]
        }"#,
    )
    .expect("document fixture");
    let source = AssetId::generate();
    let mesh_id = AssetId::derive(&source, "mesh:0");
    let mut manifest = AssetManifest::default();
    manifest.insert(
        source,
        crate::asset::ManifestEntry {
            path: "hero.gltf".into(),
            name: Some("Hero".into()),
            import_settings: crate::asset::ImportSettings {
                source_dependencies: vec!["mesh.bin".into()],
                sub_assets: vec![crate::asset::ImportedSubAsset {
                    id: mesh_id.as_str().to_owned(),
                    kind: crate::asset::ImportedSubAssetKind::Mesh,
                    name: "body".into(),
                    index: 0,
                    target_model_source: None,
                }],
                ..crate::asset::ImportSettings::default()
            },
        },
    );

    let mut asset_state = BridgeAssetState::default();
    let (mesh, diagnostic) = load_mesh_asset(
        &mesh_id,
        Some(directory.path()),
        &manifest,
        &mut asset_state,
    );

    assert!(diagnostic.is_none());
    assert_eq!(asset_state.gltf_imports.len(), 1);
    assert_eq!(mesh.vertices.len(), 3);
    assert_eq!(mesh.vertices[1].position, [2.0, 0.0, 0.0]);
    assert_eq!(mesh.vertices[2].position, [0.0, 3.0, 0.0]);
}

#[test]
fn stable_imported_material_id_loads_derived_texture_pixels() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    image::RgbaImage::from_pixel(1, 1, image::Rgba([12, 34, 56, 255]))
        .save(directory.path().join("albedo.png"))
        .expect("texture fixture");
    std::fs::write(
        directory.path().join("hero.gltf"),
        r#"{
            "asset":{"version":"2.0"},
            "images":[{"uri":"albedo.png"}],
            "textures":[{"name":"Albedo","source":0}],
            "materials":[{"name":"Armor","pbrMetallicRoughness":{"baseColorTexture":{"index":0},"roughnessFactor":0.2,"metallicFactor":0.8}}]
        }"#,
    )
    .expect("document fixture");
    let source = AssetId::generate();
    let material_id = AssetId::derive(&source, "material:0");
    let texture_id = AssetId::derive(&source, "texture:0");
    let mut manifest = AssetManifest::default();
    manifest.insert(
        source,
        crate::asset::ManifestEntry {
            path: "hero.gltf".into(),
            name: Some("Hero".into()),
            import_settings: crate::asset::ImportSettings {
                source_dependencies: vec!["albedo.png".into()],
                sub_assets: vec![
                    crate::asset::ImportedSubAsset {
                        id: material_id.as_str().to_owned(),
                        kind: crate::asset::ImportedSubAssetKind::Material,
                        name: "Armor".into(),
                        index: 0,
                        target_model_source: None,
                    },
                    crate::asset::ImportedSubAsset {
                        id: texture_id.as_str().to_owned(),
                        kind: crate::asset::ImportedSubAssetKind::Texture,
                        name: "Albedo".into(),
                        index: 0,
                        target_model_source: None,
                    },
                ],
                ..crate::asset::ImportSettings::default()
            },
        },
    );

    let mut asset_state = BridgeAssetState::default();
    let (material, diagnostics) = load_material_asset(
        &material_id,
        Some(directory.path()),
        &manifest,
        &mut asset_state,
    );

    assert!(diagnostics.is_empty());
    assert_eq!(asset_state.gltf_imports.len(), 1);
    assert_eq!(asset_state.gltf_textures.len(), 1);
    assert_eq!(material.roughness, 0.2);
    assert_eq!(material.metallic, 0.8);
    assert_eq!(
        material
            .pending_texture
            .as_ref()
            .expect("base texture must remain ready for GPU upload")
            .rgba8,
        vec![12, 34, 56, 255]
    );
}

/// A material sub-asset extracted into a standalone file (ADR 0101) resolves
/// through that file's current values instead of the source's baked import,
/// so edits made in the Material Editor are picked up without reassigning
/// any existing reference.
#[test]
fn extracted_material_remap_resolves_to_the_standalone_file_instead_of_the_import() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    std::fs::write(
        directory.path().join("hero.gltf"),
        r#"{
            "asset":{"version":"2.0"},
            "materials":[{"name":"Armor","pbrMetallicRoughness":{"roughnessFactor":0.2,"metallicFactor":0.8}}]
        }"#,
    )
    .expect("document fixture");
    let extracted = engine_authoring::MaterialAsset {
        roughness: 0.9,
        metallic: 0.1,
        ..engine_authoring::MaterialAsset::default()
    };
    std::fs::write(
        directory.path().join("armor_extracted.material.json"),
        extracted.to_json().expect("extracted material fixture JSON"),
    )
    .expect("extracted material fixture");

    let source = AssetId::generate();
    let material_id = AssetId::derive(&source, "material:0");
    let extracted_id = AssetId::generate();
    let mut manifest = AssetManifest::default();
    manifest.insert(
        source,
        crate::asset::ManifestEntry {
            path: "hero.gltf".into(),
            name: Some("Hero".into()),
            import_settings: crate::asset::ImportSettings {
                sub_assets: vec![crate::asset::ImportedSubAsset {
                    id: material_id.as_str().to_owned(),
                    kind: crate::asset::ImportedSubAssetKind::Material,
                    name: "Armor".into(),
                    index: 0,
                    target_model_source: None,
                }],
                material_remaps: [(material_id.as_str().to_owned(), extracted_id.as_str().to_owned())]
                    .into_iter()
                    .collect(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        extracted_id,
        crate::asset::ManifestEntry {
            path: "armor_extracted.material.json".into(),
            name: Some("Armor (extracted)".into()),
            import_settings: crate::asset::ImportSettings::default(),
        },
    );

    let mut asset_state = BridgeAssetState::default();
    let (material, diagnostics) = load_material_asset(
        &material_id,
        Some(directory.path()),
        &manifest,
        &mut asset_state,
    );

    assert!(diagnostics.is_empty());
    // The redirect must bypass reimport entirely for this material.
    assert!(asset_state.gltf_imports.is_empty());
    assert_eq!(material.roughness, 0.9);
    assert_eq!(material.metallic, 0.1);
}

/// Ensures an extracted material keeps every texture slot inherited from its
/// imported source material.
///
/// Extraction writes imported Texture sub-asset IDs into the standalone
/// material file. Loading that file must resolve those IDs through the owning
/// model source instead of treating them as unregistered top-level files.
#[test]
fn extracted_material_keeps_imported_texture_slots_renderable() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    image::RgbaImage::from_pixel(1, 1, image::Rgba([12, 34, 56, 255]))
        .save(directory.path().join("albedo.png"))
        .expect("texture fixture");

    let source_id = AssetId::generate();
    let imported_material_id = AssetId::derive(&source_id, "material:0");
    let imported_texture_id = AssetId::derive(&source_id, "texture:0");
    let extracted_material_id = AssetId::generate();

    std::fs::write(
        directory.path().join("hero.gltf"),
        r#"{
            "asset":{"version":"2.0"},
            "images":[{"uri":"albedo.png"}],
            "textures":[{"name":"Albedo","source":0}],
            "materials":[{
                "name":"Armor",
                "pbrMetallicRoughness":{
                    "baseColorTexture":{"index":0},
                    "roughnessFactor":0.2
                }
            }]
        }"#,
    )
    .expect("model fixture");

    let extracted = engine_authoring::MaterialAsset {
        base_color_texture: Some(imported_texture_id.clone()),
        normal_texture: Some(imported_texture_id.clone()),
        emissive_texture: Some(imported_texture_id.clone()),
        roughness: 0.9,
        toon: engine_authoring::ToonLitProperties {
            ramp_texture: Some(imported_texture_id.clone()),
            sphere_texture: Some(imported_texture_id.clone()),
            ..engine_authoring::ToonLitProperties::default()
        },
        ..engine_authoring::MaterialAsset::default()
    };
    std::fs::write(
        directory.path().join("armor_extracted.material.json"),
        extracted
            .to_json()
            .expect("extracted material fixture JSON"),
    )
    .expect("extracted material fixture");

    let mut manifest = AssetManifest::default();
    manifest.insert(
        source_id,
        crate::asset::ManifestEntry {
            path: "hero.gltf".into(),
            name: Some("Hero".into()),
            import_settings: crate::asset::ImportSettings {
                source_dependencies: vec!["albedo.png".into()],
                sub_assets: vec![
                    crate::asset::ImportedSubAsset {
                        id: imported_material_id.as_str().to_owned(),
                        kind: crate::asset::ImportedSubAssetKind::Material,
                        name: "Armor".into(),
                        index: 0,
                        target_model_source: None,
                    },
                    crate::asset::ImportedSubAsset {
                        id: imported_texture_id.as_str().to_owned(),
                        kind: crate::asset::ImportedSubAssetKind::Texture,
                        name: "Albedo".into(),
                        index: 0,
                        target_model_source: None,
                    },
                ],
                material_remaps: [(
                    imported_material_id.as_str().to_owned(),
                    extracted_material_id.as_str().to_owned(),
                )]
                .into_iter()
                .collect(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        extracted_material_id,
        crate::asset::ManifestEntry {
            path: "armor_extracted.material.json".into(),
            name: Some("Armor (extracted)".into()),
            import_settings: crate::asset::ImportSettings::default(),
        },
    );

    let mut asset_state = BridgeAssetState::default();
    let (material, diagnostics) = load_material_asset(
        &imported_material_id,
        Some(directory.path()),
        &manifest,
        &mut asset_state,
    );

    assert!(
        diagnostics.is_empty(),
        "imported texture references must resolve without diagnostics: {diagnostics:?}"
    );
    assert_eq!(material.roughness, 0.9);
    assert_eq!(
        material
            .pending_texture
            .as_ref()
            .expect("base-color texture must be decoded")
            .rgba8,
        vec![12, 34, 56, 255]
    );
    assert!(material.pending_normal_texture.is_some());
    assert!(material.pending_emissive_texture.is_some());
    assert!(material.toon.pending_ramp_texture.is_some());
    assert!(material.toon.pending_sphere_texture.is_some());
    assert_eq!(asset_state.gltf_imports.len(), 1);
    assert_eq!(asset_state.gltf_textures.len(), 1);
}

/// Verifies that a Texture sub-asset override is resolved before decoding the
/// image used by an imported material.
#[test]
fn imported_material_uses_model_level_texture_override() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    image::RgbaImage::from_pixel(1, 1, image::Rgba([12, 34, 56, 255]))
        .save(directory.path().join("original.png"))
        .expect("original texture fixture");
    image::RgbaImage::from_pixel(1, 1, image::Rgba([210, 120, 30, 255]))
        .save(directory.path().join("replacement.png"))
        .expect("replacement texture fixture");

    let source_id = AssetId::generate();
    let material_id = AssetId::derive(&source_id, "material:0");
    let imported_texture_id = AssetId::derive(&source_id, "texture:0");
    let replacement_texture_id = AssetId::generate();
    std::fs::write(
        directory.path().join("hero.gltf"),
        r#"{
            "asset":{"version":"2.0"},
            "images":[{"uri":"original.png"}],
            "textures":[{"name":"Albedo","source":0}],
            "materials":[{
                "name":"Armor",
                "pbrMetallicRoughness":{"baseColorTexture":{"index":0}}
            }]
        }"#,
    )
    .expect("model fixture");

    let mut manifest = AssetManifest::default();
    manifest.insert(
        source_id,
        crate::asset::ManifestEntry {
            path: "hero.gltf".into(),
            name: Some("Hero".into()),
            import_settings: crate::asset::ImportSettings {
                source_dependencies: vec!["original.png".into()],
                sub_assets: vec![
                    crate::asset::ImportedSubAsset {
                        id: material_id.as_str().to_owned(),
                        kind: crate::asset::ImportedSubAssetKind::Material,
                        name: "Armor".into(),
                        index: 0,
                        target_model_source: None,
                    },
                    crate::asset::ImportedSubAsset {
                        id: imported_texture_id.as_str().to_owned(),
                        kind: crate::asset::ImportedSubAssetKind::Texture,
                        name: "Albedo".into(),
                        index: 0,
                        target_model_source: None,
                    },
                ],
                texture_remaps: [(
                    imported_texture_id.as_str().to_owned(),
                    replacement_texture_id.as_str().to_owned(),
                )]
                .into_iter()
                .collect(),
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        replacement_texture_id,
        crate::asset::ManifestEntry {
            path: "replacement.png".into(),
            name: Some("Replacement".into()),
            import_settings: crate::asset::ImportSettings::default(),
        },
    );

    let mut asset_state = BridgeAssetState::default();
    let (material, diagnostics) = load_material_asset(
        &material_id,
        Some(directory.path()),
        &manifest,
        &mut asset_state,
    );

    assert!(diagnostics.is_empty(), "unexpected diagnostics: {diagnostics:?}");
    assert_eq!(
        material
            .pending_texture
            .expect("replacement texture must be decoded")
            .rgba8,
        vec![210, 120, 30, 255]
    );
}

/// Writes a single-mesh glTF fixture whose only vertex position row is
/// controlled by the caller, so cache-invalidation tests can change the
/// source bytes (and byte length) deterministically.
fn write_gltf_point_fixture(directory: &std::path::Path, x: f32) {
    let mut positions = Vec::new();
    for value in [x, 0.0, 0.0] {
        positions.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(directory.join("mesh.bin"), positions).expect("buffer fixture");
    std::fs::write(
        directory.join("hero.gltf"),
        format!(
            r#"{{
                "asset":{{"version":"2.0"}},
                "buffers":[{{"uri":"mesh.bin","byteLength":12}}],
                "bufferViews":[{{"buffer":0,"byteLength":12}}],
                "accessors":[{{"bufferView":0,"componentType":5126,"count":1,"type":"VEC3","min":[{x},0,0],"max":[{x},0,0]}}],
                "meshes":[{{"name":"body","primitives":[{{"attributes":{{"POSITION":0}}}}]}}]
            }}"#
        ),
    )
    .expect("document fixture");
}

#[test]
fn shared_gltf_cache_reuses_parsed_source_across_conversions() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    write_gltf_point_fixture(directory.path(), 1.0);
    let source = AssetId::generate();
    let path = directory.path().join("hero.gltf");
    let shared = SharedGltfImportCache::default();

    let mut first_state = BridgeAssetState {
        shared_gltf_cache: Some(shared.clone()),
        ..BridgeAssetState::default()
    };
    let first =
        import_gltf_cached(&source, &path, &[], &[], &mut first_state).expect("first parse");

    let mut second_state = BridgeAssetState {
        shared_gltf_cache: Some(shared),
        ..BridgeAssetState::default()
    };
    let second =
        import_gltf_cached(&source, &path, &[], &[], &mut second_state).expect("cached parse");

    assert!(
        Arc::ptr_eq(&first, &second),
        "a later conversion must reuse the shared parse instead of re-reading the source"
    );
}

#[test]
fn shared_gltf_cache_reparses_when_source_bytes_change() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    write_gltf_point_fixture(directory.path(), 1.0);
    let source = AssetId::generate();
    let path = directory.path().join("hero.gltf");
    let shared = SharedGltfImportCache::default();

    let mut first_state = BridgeAssetState {
        shared_gltf_cache: Some(shared.clone()),
        ..BridgeAssetState::default()
    };
    let first =
        import_gltf_cached(&source, &path, &[], &[], &mut first_state).expect("first parse");
    assert_eq!(first.meshes[0].mesh.vertices[0].position, [1.0, 0.0, 0.0]);

    // 10.0 serialises to more JSON bytes than 1.0, so the stamp changes even
    // on filesystems with coarse modification-time granularity.
    write_gltf_point_fixture(directory.path(), 10.0);

    let mut second_state = BridgeAssetState {
        shared_gltf_cache: Some(shared),
        ..BridgeAssetState::default()
    };
    let second = import_gltf_cached(&source, &path, &[], &[], &mut second_state).expect("reparse");

    assert!(
        !Arc::ptr_eq(&first, &second),
        "an edited source must not be served from the shared cache"
    );
    assert_eq!(second.meshes[0].mesh.vertices[0].position, [10.0, 0.0, 0.0]);
}

#[test]
fn shared_gltf_cache_keeps_decoded_texture_identity_across_conversions() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    image::RgbaImage::from_pixel(1, 1, image::Rgba([12, 34, 56, 255]))
        .save(directory.path().join("albedo.png"))
        .expect("texture fixture");
    std::fs::write(
        directory.path().join("hero.gltf"),
        r#"{
            "asset":{"version":"2.0"},
            "images":[{"uri":"albedo.png"}],
            "textures":[{"name":"Albedo","source":0}],
            "materials":[{"name":"Armor","pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}]
        }"#,
    )
    .expect("document fixture");
    let source = AssetId::generate();
    let material_id = AssetId::derive(&source, "material:0");
    let texture_id = AssetId::derive(&source, "texture:0");
    let mut manifest = AssetManifest::default();
    manifest.insert(
        source,
        crate::asset::ManifestEntry {
            path: "hero.gltf".into(),
            name: Some("Hero".into()),
            import_settings: crate::asset::ImportSettings {
                source_dependencies: vec!["albedo.png".into()],
                sub_assets: vec![
                    crate::asset::ImportedSubAsset {
                        id: material_id.as_str().to_owned(),
                        kind: crate::asset::ImportedSubAssetKind::Material,
                        name: "Armor".into(),
                        index: 0,
                        target_model_source: None,
                    },
                    crate::asset::ImportedSubAsset {
                        id: texture_id.as_str().to_owned(),
                        kind: crate::asset::ImportedSubAssetKind::Texture,
                        name: "Albedo".into(),
                        index: 0,
                        target_model_source: None,
                    },
                ],
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    let shared = SharedGltfImportCache::default();
    let load = |shared: SharedGltfImportCache| {
        let mut asset_state = BridgeAssetState {
            shared_gltf_cache: Some(shared),
            ..BridgeAssetState::default()
        };
        let (material, diagnostics) = load_material_asset(
            &material_id,
            Some(directory.path()),
            &manifest,
            &mut asset_state,
        );
        assert!(diagnostics.is_empty());
        material
            .pending_texture
            .expect("base texture must remain ready for GPU upload")
    };

    let first = load(shared.clone());
    let second = load(shared);

    // Renderer GPU-upload caches key by allocation identity, so reuse must
    // preserve the exact `Arc`, not merely equal pixels.
    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn authorable_camera_component_spawns_runtime_camera() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "camera",
                    "components": {
                        "engine.camera": {
                            "enabled": false,
                            "priority": 42,
                            "fov_y_degrees": 70.0,
                            "near": 0.2,
                            "far": 500.0
                        }
                    }
                }
            ]
        }"#,
    )
    .expect("camera scene JSON must load");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("camera scene must bridge");
    let camera_id = scene
        .entities()
        .find(|(_, entity)| entity.name == "camera")
        .map(|(id, _)| id)
        .expect("camera entity must exist");
    let camera_entity = bridge.get(camera_id).expect("camera must be mapped");
    let camera = world
        .get_component::<Camera3D>(camera_entity)
        .expect("camera component must spawn");

    assert!(!camera.enabled);
    assert_eq!(camera.priority, 42);
    assert!((camera.fov_y_radians.to_degrees() - 70.0).abs() < f32::EPSILON);
    assert_eq!(camera.near, 0.2);
    assert_eq!(camera.far, 500.0);
}

#[test]
fn authorable_camera_without_enabled_and_priority_is_rejected() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "camera",
                "components": {
                    "engine.camera": {
                        "fov_y_degrees": 60.0,
                        "near": 0.1,
                        "far": 1000.0
                    }
                }
            }]
        }"#,
    )
    .expect("camera scene JSON must load");

    let mut world = World::new();
    let error = spawn_from_authoring_scene(&mut world, &scene)
        .expect_err("engine.camera requires enabled and priority");

    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == CAMERA_COMPONENT
    ));
}

#[test]
fn authorable_player_controller_component_spawns_runtime_controller() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "player",
                    "components": {
                        "engine.player_marker": {},
                        "engine.player_controller": {
                            "move_speed": 4.5,
                            "move_plane": "xy"
                        }
                    }
                }
            ]
        }"#,
    )
    .expect("player controller scene JSON must load");

    let mut world = World::new();
    let bridge =
        spawn_from_authoring_scene(&mut world, &scene).expect("controller scene must bridge");
    let player_id = scene
        .entities()
        .find(|(_, entity)| entity.name == "player")
        .map(|(id, _)| id)
        .expect("player entity must exist");
    let player = bridge.get(player_id).expect("player must be mapped");
    let controller = world
        .get_component::<PlayerController>(player)
        .expect("controller component must spawn");

    assert_eq!(controller.move_speed, 4.5);
    assert_eq!(controller.move_plane, MovePlane::Xy);
}

#[test]
fn multiple_directional_lights_emit_warning_diagnostic() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "sun_a",
                    "components": {
                        "engine.directional_light": {
                            "direction_x": -0.5,
                            "direction_y": -1.0,
                            "direction_z": -0.5,
                            "color_r": 1.0,
                            "color_g": 1.0,
                            "color_b": 1.0,
                            "intensity": 1.0
                        }
                    }
                },
                {
                    "id": "entity_01JP0000000000000000000002",
                    "name": "sun_b",
                    "components": {
                        "engine.directional_light": {
                            "direction_x": 0.0,
                            "direction_y": -1.0,
                            "direction_z": 0.0,
                            "color_r": 1.0,
                            "color_g": 0.8,
                            "color_b": 0.6,
                            "intensity": 0.5
                        }
                    }
                }
            ]
        }"#,
    )
    .expect("light scene JSON must load");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene).expect("light scene must bridge");

    assert_eq!(bridge.asset_diagnostics.len(), 1);
    assert_eq!(
        bridge.asset_diagnostics[0].code,
        "scene_bridge.multiple_directional_lights"
    );
    assert!(!bridge.asset_diagnostics[0].is_blocking());
}

#[test]
fn scene_presentation_components_convert_to_runtime_settings() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "presentation",
                "components": {
                    "engine.shadow_settings": {
                        "enabled": false,
                        "cascade_near_split": 0.3,
                        "cascade_far_split": 0.9,
                        "depth_bias": 0.001,
                        "normal_bias": 0.02
                    },
                    "engine.environment_lighting": {
                        "diffuse_ibl_enabled": true,
                        "color_r": 0.25,
                        "color_g": 0.5,
                        "color_b": 0.75,
                        "intensity": 1.5
                    },
                    "engine.post_process": {
                        "enabled": true,
                        "exposure": 1.25,
                        "tone_map": "reinhard",
                        "bloom_enabled": true,
                        "bloom_threshold": 0.8,
                        "bloom_intensity": 0.4,
                        "bloom_radius": 6.0
                    }
                }
            }]
        }"#,
    )
    .expect("presentation scene must load");

    let mut world = World::new();
    let bridge =
        spawn_from_authoring_scene(&mut world, &scene).expect("presentation scene must convert");
    let authoring_id = scene.entities().next().expect("entity must exist").0;
    let entity = bridge.get(authoring_id).expect("entity must be mapped");

    let shadow = world
        .get_component::<ShadowSettings>(entity)
        .expect("shadow settings must attach");
    assert!(!shadow.enabled);
    assert_eq!(shadow.cascade_splits, [0.3, 0.9]);
    let environment = world
        .get_component::<EnvironmentLighting>(entity)
        .expect("environment settings must attach");
    assert!(environment.diffuse_ibl_enabled);
    assert_eq!(environment.diffuse_color, Vec3::new(0.25, 0.5, 0.75));
    assert_eq!(environment.intensity, 1.5);
    let post_process = world
        .get_component::<PostProcessSettings>(entity)
        .expect("post-process settings must attach");
    assert_eq!(post_process.exposure, 1.25);
    assert_eq!(post_process.tone_map, ToneMapOperator::Reinhard);
    assert!(post_process.bloom.enabled);
}

#[test]
fn duplicate_scene_presentation_settings_warn_and_keep_conversion_non_blocking() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "first",
                    "components": {"engine.post_process": {
                        "enabled": true,
                        "exposure": 1.0,
                        "tone_map": "aces_fitted",
                        "bloom_enabled": false,
                        "bloom_threshold": 1.0,
                        "bloom_intensity": 0.15,
                        "bloom_radius": 4.0
                    }}
                },
                {
                    "id": "entity_01JP0000000000000000000002",
                    "name": "second",
                    "components": {"engine.post_process": {
                        "enabled": true,
                        "exposure": 2.0,
                        "tone_map": "reinhard",
                        "bloom_enabled": false,
                        "bloom_threshold": 1.0,
                        "bloom_intensity": 0.15,
                        "bloom_radius": 4.0
                    }}
                }
            ]
        }"#,
    )
    .expect("duplicate settings scene must load");

    let bridge = spawn_from_authoring_scene(&mut World::new(), &scene)
        .expect("duplicates are diagnostic, not a conversion failure");
    assert!(bridge.asset_diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "scene_bridge.multiple_post_process" && !diagnostic.is_blocking()
    }));
}

// --- engine.particle_emitter tests (Phase 52) ---

fn particle_emitter_default_object() -> BTreeMap<String, Value> {
    let registry = builtin_registry();
    let definition = registry
        .get(&ComponentTypeId::new(PARTICLE_EMITTER_COMPONENT))
        .expect("particle emitter must be registered");
    match definition.schema.default_value() {
        Value::Object(object) => object,
        other => panic!("particle emitter default value must be an object, got {other:?}"),
    }
}

fn spawn_single_particle_emitter_entity(
    object: BTreeMap<String, Value>,
) -> Result<(World, AuthoringToRuntimeMap, EntityId), SceneBridgeError> {
    let mut scene = AuthoringScene::new();
    let id = EntityId::generate();

    let mut tx = Transaction::begin(&scene);
    tx.apply(AuthoringCommand::CreateEntity {
        id: id.clone(),
        name: "emitter".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::AddComponent {
        entity: id.clone(),
        component_type: ComponentTypeId::new(PARTICLE_EMITTER_COMPONENT),
        value: Value::Object(object),
    });
    tx.commit(&mut scene)
        .expect("setup transaction must commit");

    let mut world = World::new();
    let result = spawn_from_authoring_scene(&mut world, &scene);
    result.map(|bridge| (world, bridge, id))
}

#[test]
fn particle_emitter_default_object_spawns_with_runtime_defaults() {
    let object = particle_emitter_default_object();
    let (world, bridge, id) = spawn_single_particle_emitter_entity(object)
        .expect("default particle emitter scene must bridge");
    let entity = bridge.get(&id).expect("emitter must be mapped");
    let emitter = world
        .get_component::<crate::particles::ParticleEmitter>(entity)
        .expect("particle emitter component must spawn");
    let material = world
        .get_component::<Material>(entity)
        .expect("particle emitter must install its authored material");

    let mut placeholder_meshes = Assets::<Mesh>::default();
    let runtime_default =
        crate::particles::ParticleEmitter::new(placeholder_meshes.add(Mesh::triangle()));

    assert_eq!(emitter.spawn_rate, runtime_default.spawn_rate);
    assert_eq!(emitter.lifetime, runtime_default.lifetime);
    assert_eq!(emitter.initial_speed, runtime_default.initial_speed);
    assert_eq!(emitter.direction, runtime_default.direction);
    assert_eq!(emitter.spread, runtime_default.spread);
    assert_eq!(emitter.gravity, runtime_default.gravity);
    assert_eq!(emitter.start_color, runtime_default.start_color);
    assert_eq!(emitter.end_color, runtime_default.end_color);
    assert_eq!(emitter.start_size, runtime_default.start_size);
    assert_eq!(emitter.end_size, runtime_default.end_size);
    assert_eq!(emitter.max_particles, runtime_default.max_particles);
    assert_eq!(emitter.seed, runtime_default.seed);
    assert_eq!(emitter.live_count(), 0);
    assert_eq!(material.color, Material::default().color);
}

#[test]
fn customized_particle_emitter_values_spawn_into_runtime_component() {
    let mut object = particle_emitter_default_object();
    object.insert("spawn_rate".into(), Value::F64(5.0));
    object.insert("direction_x".into(), Value::F64(1.0));
    object.insert("direction_y".into(), Value::F64(0.0));
    object.insert("direction_z".into(), Value::F64(0.0));
    object.insert("seed".into(), Value::I64(42));

    let (world, bridge, id) = spawn_single_particle_emitter_entity(object)
        .expect("customized particle emitter scene must bridge");
    let entity = bridge.get(&id).expect("emitter must be mapped");
    let emitter = world
        .get_component::<crate::particles::ParticleEmitter>(entity)
        .expect("particle emitter component must spawn");

    assert_eq!(emitter.spawn_rate, 5.0);
    assert_eq!(emitter.direction, Vec3::new(1.0, 0.0, 0.0));
    assert_eq!(emitter.seed, 42);
    assert_eq!(emitter.live_count(), 0);
}

#[test]
fn particle_emitter_lifetime_min_greater_than_max_fails() {
    let mut object = particle_emitter_default_object();
    object.insert("lifetime_min".into(), Value::F64(2.0));
    object.insert("lifetime_max".into(), Value::F64(1.0));

    let error = spawn_single_particle_emitter_entity(object)
        .err()
        .expect("lifetime_min greater than lifetime_max must fail conversion");

    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue {
            component_type,
            ..
        } if component_type.as_str() == PARTICLE_EMITTER_COMPONENT
    ));
}

#[test]
fn particle_emitter_non_finite_spawn_rate_fails() {
    // `AuthoringScene::validate` (run at both `Transaction::commit` and
    // bridge conversion time) already rejects a non-finite `Value::F64`
    // at the scene level, so an f64 NaN/infinity never reaches a
    // component's own field validation. A value that is finite as `f64`
    // but overflows on the component's `as f32` cast (Rust saturates
    // out-of-range float casts to infinity rather than panicking)
    // exercises this component's own finiteness check instead.
    let mut object = particle_emitter_default_object();
    object.insert("spawn_rate".into(), Value::F64(1e300));

    let error = spawn_single_particle_emitter_entity(object)
        .err()
        .expect("f32-overflowing spawn_rate must fail conversion");

    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue {
            component_type,
            ..
        } if component_type.as_str() == PARTICLE_EMITTER_COMPONENT
    ));
}

#[test]
fn particle_emitter_wrong_type_spawn_rate_fails() {
    let mut object = particle_emitter_default_object();
    object.insert("spawn_rate".into(), Value::Bool(true));

    let error = spawn_single_particle_emitter_entity(object)
        .err()
        .expect("boolean spawn_rate must fail conversion");

    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue {
            component_type,
            ..
        } if component_type.as_str() == PARTICLE_EMITTER_COMPONENT
    ));
}

#[test]
fn particle_emitter_negative_spawn_rate_fails() {
    let mut object = particle_emitter_default_object();
    object.insert("spawn_rate".into(), Value::F64(-1.0));

    let error = spawn_single_particle_emitter_entity(object)
        .err()
        .expect("negative spawn_rate must fail conversion");

    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue {
            component_type,
            ..
        } if component_type.as_str() == PARTICLE_EMITTER_COMPONENT
    ));
}

#[test]
fn particle_emitter_unregistered_mesh_asset_spawns_with_fallback_and_warning() {
    let mut object = particle_emitter_default_object();
    let unregistered = AssetId::from_stable_id(engine_authoring::StableId::new(
        "asset_01JP0000000000000000000999",
    ))
    .expect("valid asset id");
    object.insert("mesh".into(), Value::AssetRef(unregistered));

    let (world, bridge, id) = spawn_single_particle_emitter_entity(object)
        .expect("unregistered mesh asset must not block conversion");

    assert_eq!(bridge.asset_diagnostics.len(), 1);
    assert_eq!(
        bridge.asset_diagnostics[0].code, "asset.unregistered_file",
        "unregistered mesh asset must emit asset.unregistered_file warning"
    );
    let entity = bridge.get(&id).expect("emitter must be mapped");
    assert!(world.has_component::<crate::particles::ParticleEmitter>(entity));
}

// --- engine.ui_document tests (Phase 54) --------------------------------

fn ui_document_scene_json(asset_id: &str) -> String {
    format!(
        r#"{{
            "entities": [
                {{
                    "id": "entity_01JP0000000000000000000001",
                    "name": "hud",
                    "components": {{
                        "engine.ui_document": {{
                            "$type": "asset_ref",
                            "id": "{asset_id}"
                        }}
                    }}
                }}
            ]
        }}"#
    )
}

#[test]
fn ui_document_builtin_asset_spawns_with_builtin_document() {
    let scene = load_scene_fixture(&ui_document_scene_json(BUILTIN_UI_DOCUMENT_ASSET_ID))
        .expect("ui document scene JSON must load");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("built-in ui document scene must bridge");
    let hud_id = scene
        .entities()
        .find(|(_, entity)| entity.name == "hud")
        .map(|(id, _)| id)
        .expect("hud entity must exist");
    let entity = bridge.get(hud_id).expect("hud entity must be mapped");

    let document_ref = world
        .get_component::<UiDocumentRef>(entity)
        .expect("engine.ui_document must attach a UiDocumentRef");
    assert!(
        document_ref.source_path.is_none(),
        "built-in document must not resolve a manifest path"
    );
    assert_eq!(document_ref.document.root.children.len(), 1);
    assert!(matches!(
        &document_ref.document.root.children[0].kind,
        UiNodeKind::Text {
            content: UiString::Literal(text),
            ..
        } if text == "New UI"
    ));
}

#[test]
fn ui_document_unknown_asset_spawns_with_empty_document_and_diagnostic() {
    let scene = load_scene_fixture(&ui_document_scene_json("asset_01JP0000000000000000000999"))
        .expect("ui document scene JSON must load");

    let mut world = World::new();
    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("unregistered ui document asset must not block conversion");

    assert_eq!(bridge.asset_diagnostics.len(), 1);
    assert_eq!(
        bridge.asset_diagnostics[0].code, "asset.unregistered_file",
        "unregistered ui document asset must emit asset.unregistered_file warning"
    );
    assert!(!bridge.asset_diagnostics[0].is_blocking());

    let hud_id = scene
        .entities()
        .find(|(_, entity)| entity.name == "hud")
        .map(|(id, _)| id)
        .expect("hud entity must exist");
    let entity = bridge.get(hud_id).expect("hud entity must be mapped");
    let document_ref = world
        .get_component::<UiDocumentRef>(entity)
        .expect("component presence must not depend on asset health");
    assert_eq!(document_ref.document, UiDocument::default());
    assert!(document_ref.source_path.is_none());
}

#[test]
fn ui_document_manifest_registered_file_loads_with_source_path() {
    let dir = tempfile::tempdir().expect("must create temp dir");
    let ui_path = dir.path().join("hud.ui.json");
    std::fs::write(
        &ui_path,
        r#"{"schema_version":3,"root":{"id":"root","type":"spacer","size":3.0}}"#,
    )
    .expect("must write ui document file");

    let scene = load_scene_fixture(&ui_document_scene_json("asset_01JP0000000000000000000999"))
        .expect("ui document scene JSON must load");

    let mut world = World::new();
    world.insert_resource(crate::asset::AssetServer::with_assets_root(dir.path()));
    let mut manifest = AssetManifest::default();
    let asset_id = AssetId::from_stable_id(engine_authoring::StableId::new(
        "asset_01JP0000000000000000000999",
    ))
    .expect("valid asset id");
    manifest.insert(
        asset_id,
        crate::asset::ManifestEntry {
            path: "hud.ui.json".to_string(),
            name: Some("hud".to_string()),
            import_settings: crate::asset::ImportSettings::default(),
        },
    );
    world.insert_resource(manifest);

    let bridge = spawn_from_authoring_scene(&mut world, &scene)
        .expect("manifest-registered ui document scene must bridge");
    assert!(
        bridge
            .asset_diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "asset.unregistered_file"),
        "manifest-registered ui document must not be reported unregistered"
    );

    let hud_id = scene
        .entities()
        .find(|(_, entity)| entity.name == "hud")
        .map(|(id, _)| id)
        .expect("hud entity must exist");
    let entity = bridge.get(hud_id).expect("hud entity must be mapped");
    let document_ref = world
        .get_component::<UiDocumentRef>(entity)
        .expect("engine.ui_document must attach a UiDocumentRef");
    assert!(
        matches!(
            &document_ref.document.root.kind,
            UiNodeKind::Spacer { size } if (*size - 3.0).abs() < f32::EPSILON
        ),
        "manifest-registered document must load its file contents"
    );
    assert_eq!(
        document_ref.source_path.as_deref(),
        Some(ui_path.as_path()),
        "source_path must be the resolved absolute path"
    );
}

// --- engine.collider / engine.physics_body / engine.character_controller
// tests (Phase 57) --------------------------------------------------------

fn collider_default_object() -> BTreeMap<String, Value> {
    let registry = builtin_registry();
    let definition = registry
        .get(&ComponentTypeId::new(COLLIDER_COMPONENT))
        .expect("collider must be registered");
    match definition.schema.default_value() {
        Value::Object(object) => object,
        other => panic!("collider default value must be an object, got {other:?}"),
    }
}

fn spawn_single_component_entity(
    component_type: &str,
    value: Value,
) -> Result<(World, AuthoringToRuntimeMap, EntityId), SceneBridgeError> {
    let mut scene = AuthoringScene::new();
    let id = EntityId::generate();

    let mut tx = Transaction::begin(&scene);
    tx.apply(AuthoringCommand::CreateEntity {
        id: id.clone(),
        name: "subject".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::AddComponent {
        entity: id.clone(),
        component_type: ComponentTypeId::new(component_type),
        value,
    });
    tx.commit(&mut scene)
        .expect("setup transaction must commit");

    let mut world = World::new();
    let result = spawn_from_authoring_scene(&mut world, &scene);
    result.map(|bridge| (world, bridge, id))
}

#[test]
fn collider_default_object_spawns_aabb_with_default_layers() {
    let object = collider_default_object();
    let (world, bridge, id) =
        spawn_single_component_entity(COLLIDER_COMPONENT, Value::Object(object))
            .expect("default collider scene must bridge");
    let entity = bridge.get(&id).expect("entity must be mapped");

    let collider = world
        .get_component::<Collider>(entity)
        .expect("collider component must spawn");
    assert!(matches!(collider, Collider::Aabb { .. }));

    let layers = world
        .get_component::<CollisionLayers>(entity)
        .expect("collision layers must spawn");
    assert_eq!(layers.membership, 1);
    assert_eq!(layers.mask, u32::MAX);

    assert!(!world.has_component::<TriggerVolume>(entity));
}

#[test]
fn collider_sphere_shape_spawns_sphere_collider() {
    let mut object = collider_default_object();
    object.insert("shape".into(), Value::String("sphere".into()));
    object.insert("radius".into(), Value::F64(2.0));

    let (world, bridge, id) =
        spawn_single_component_entity(COLLIDER_COMPONENT, Value::Object(object))
            .expect("sphere collider scene must bridge");
    let entity = bridge.get(&id).expect("entity must be mapped");
    let collider = world
        .get_component::<Collider>(entity)
        .expect("collider component must spawn");
    assert!(matches!(collider, Collider::Sphere { radius } if *radius == 2.0));
}

#[test]
fn collider_capsule_y_shape_spawns_capsule_collider() {
    let mut object = collider_default_object();
    object.insert("shape".into(), Value::String("capsule_y".into()));
    object.insert("half_height".into(), Value::F64(0.7));
    object.insert("radius".into(), Value::F64(0.3));

    let (world, bridge, id) =
        spawn_single_component_entity(COLLIDER_COMPONENT, Value::Object(object))
            .expect("capsule collider scene must bridge");
    let entity = bridge.get(&id).expect("entity must be mapped");
    let collider = world
        .get_component::<Collider>(entity)
        .expect("collider component must spawn");
    assert!(matches!(
        collider,
        Collider::CapsuleY { half_height, radius }
        if *half_height == 0.7 && *radius == 0.3
    ));
}

#[test]
fn collider_is_trigger_true_attaches_trigger_volume() {
    let mut object = collider_default_object();
    object.insert("is_trigger".into(), Value::Bool(true));

    let (world, bridge, id) =
        spawn_single_component_entity(COLLIDER_COMPONENT, Value::Object(object))
            .expect("trigger collider scene must bridge");
    let entity = bridge.get(&id).expect("entity must be mapped");
    assert!(world.has_component::<TriggerVolume>(entity));
}

#[test]
fn collider_unknown_shape_string_fails_conversion() {
    let mut object = collider_default_object();
    object.insert("shape".into(), Value::String("cylinder".into()));

    let error = spawn_single_component_entity(COLLIDER_COMPONENT, Value::Object(object))
        .err()
        .expect("unknown shape must fail conversion");
    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == COLLIDER_COMPONENT
    ));
}

#[test]
fn collider_negative_radius_fails_conversion_for_sphere_shape() {
    let mut object = collider_default_object();
    object.insert("shape".into(), Value::String("sphere".into()));
    object.insert("radius".into(), Value::F64(-1.0));

    let error = spawn_single_component_entity(COLLIDER_COMPONENT, Value::Object(object))
        .err()
        .expect("negative radius must fail conversion");
    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == COLLIDER_COMPONENT
    ));
}

#[test]
fn collider_membership_out_of_u32_range_fails_conversion() {
    let mut object = collider_default_object();
    object.insert("membership".into(), Value::I64(-1));

    let error = spawn_single_component_entity(COLLIDER_COMPONENT, Value::Object(object))
        .err()
        .expect("out-of-range membership must fail conversion");
    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == COLLIDER_COMPONENT
    ));

    let mut object = collider_default_object();
    object.insert("mask".into(), Value::I64(i64::from(u32::MAX) + 1));
    let error = spawn_single_component_entity(COLLIDER_COMPONENT, Value::Object(object))
        .err()
        .expect("out-of-range mask must fail conversion");
    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == COLLIDER_COMPONENT
    ));
}

#[test]
fn physics_body_kind_strings_map_to_runtime_variants() {
    for (kind, expected) in [
        ("static", PhysicsBody::Static),
        ("kinematic", PhysicsBody::Kinematic),
        ("dynamic", PhysicsBody::Dynamic),
    ] {
        let mut object = BTreeMap::new();
        object.insert("kind".into(), Value::String(kind.into()));
        let (world, bridge, id) =
            spawn_single_component_entity(PHYSICS_BODY_COMPONENT, Value::Object(object))
                .unwrap_or_else(|_| panic!("kind \"{kind}\" must bridge"));
        let entity = bridge.get(&id).expect("entity must be mapped");
        let body = world
            .get_component::<PhysicsBody>(entity)
            .expect("physics body must spawn");
        assert_eq!(*body, expected, "kind \"{kind}\" must map correctly");
    }
}

#[test]
fn physics_body_unknown_kind_string_fails_conversion() {
    let mut object = BTreeMap::new();
    object.insert("kind".into(), Value::String("ghost".into()));
    let error = spawn_single_component_entity(PHYSICS_BODY_COMPONENT, Value::Object(object))
        .err()
        .expect("unknown kind must fail conversion");
    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == PHYSICS_BODY_COMPONENT
    ));
}

#[test]
fn character_controller_default_object_spawns_with_zero_velocity() {
    let registry = builtin_registry();
    let definition = registry
        .get(&ComponentTypeId::new(CHARACTER_CONTROLLER_COMPONENT))
        .expect("character controller must be registered");
    let object = match definition.schema.default_value() {
        Value::Object(object) => object,
        other => panic!("character controller default value must be an object, got {other:?}"),
    };

    let (world, bridge, id) =
        spawn_single_component_entity(CHARACTER_CONTROLLER_COMPONENT, Value::Object(object))
            .expect("default character controller scene must bridge");
    let entity = bridge.get(&id).expect("entity must be mapped");
    let controller = world
        .get_component::<KinematicCharacterController>(entity)
        .expect("character controller must spawn");
    assert_eq!(controller.velocity, Vec3::ZERO);
    assert_eq!(controller.gravity_scale, 1.0);
    assert_eq!(controller.max_resolve_iterations, 3);
    assert!(!controller.grounded);
}

#[test]
fn character_controller_max_resolve_iterations_out_of_range_fails_conversion() {
    let mut object = BTreeMap::new();
    object.insert("gravity_scale".into(), Value::F64(1.0));
    object.insert("max_resolve_iterations".into(), Value::I64(0));
    let error =
        spawn_single_component_entity(CHARACTER_CONTROLLER_COMPONENT, Value::Object(object))
            .err()
            .expect("zero max_resolve_iterations must fail conversion");
    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == CHARACTER_CONTROLLER_COMPONENT
    ));

    let mut object = BTreeMap::new();
    object.insert("gravity_scale".into(), Value::F64(1.0));
    object.insert("max_resolve_iterations".into(), Value::I64(17));
    let error =
        spawn_single_component_entity(CHARACTER_CONTROLLER_COMPONENT, Value::Object(object))
            .err()
            .expect("max_resolve_iterations above 16 must fail conversion");
    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == CHARACTER_CONTROLLER_COMPONENT
    ));
}

// --- engine.lock_on_target / engine.lock_on_camera tests (Phase 58) ------

fn lock_on_target_default_object() -> BTreeMap<String, Value> {
    let registry = builtin_registry();
    let definition = registry
        .get(&ComponentTypeId::new(LOCK_ON_TARGET_COMPONENT))
        .expect("lock_on_target must be registered");
    match definition.schema.default_value() {
        Value::Object(object) => object,
        other => panic!("lock_on_target default value must be an object, got {other:?}"),
    }
}

fn lock_on_camera_default_object() -> BTreeMap<String, Value> {
    let registry = builtin_registry();
    let definition = registry
        .get(&ComponentTypeId::new(LOCK_ON_CAMERA_COMPONENT))
        .expect("lock_on_camera must be registered");
    match definition.schema.default_value() {
        Value::Object(object) => object,
        other => panic!("lock_on_camera default value must be an object, got {other:?}"),
    }
}

/// Builds a two-entity scene: a plain "source" entity spawned with id
/// `source_id`, and a "camera" entity carrying `engine.lock_on_camera`
/// with `object` as its value (the caller sets `object["source"]`).
fn spawn_lock_on_camera_scene(
    object: BTreeMap<String, Value>,
    source_id: EntityId,
) -> Result<(World, AuthoringToRuntimeMap, EntityId, EntityId), SceneBridgeError> {
    let mut scene = AuthoringScene::new();
    let camera_id = EntityId::generate();

    let mut tx = Transaction::begin(&scene);
    tx.apply(AuthoringCommand::CreateEntity {
        id: source_id.clone(),
        name: "source".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::CreateEntity {
        id: camera_id.clone(),
        name: "camera".into(),
        parent: None,
    });
    tx.apply(AuthoringCommand::AddComponent {
        entity: camera_id.clone(),
        component_type: ComponentTypeId::new(LOCK_ON_CAMERA_COMPONENT),
        value: Value::Object(object),
    });
    tx.commit(&mut scene)
        .expect("setup transaction must commit");

    let mut world = World::new();
    let result = spawn_from_authoring_scene(&mut world, &scene);
    result.map(|bridge| (world, bridge, source_id, camera_id))
}

#[test]
fn lock_on_target_default_object_spawns_with_team_zero() {
    let object = lock_on_target_default_object();
    let (world, bridge, id) =
        spawn_single_component_entity(LOCK_ON_TARGET_COMPONENT, Value::Object(object))
            .expect("default lock_on_target scene must bridge");
    let entity = bridge.get(&id).expect("entity must be mapped");
    let target = world
        .get_component::<LockOnTarget>(entity)
        .expect("lock_on_target component must spawn");
    assert_eq!(target.team, 0);
}

#[test]
fn lock_on_target_custom_team_spawns_with_that_team() {
    let mut object = lock_on_target_default_object();
    object.insert("team".into(), Value::I64(7));
    let (world, bridge, id) =
        spawn_single_component_entity(LOCK_ON_TARGET_COMPONENT, Value::Object(object))
            .expect("custom-team lock_on_target scene must bridge");
    let entity = bridge.get(&id).expect("entity must be mapped");
    let target = world
        .get_component::<LockOnTarget>(entity)
        .expect("lock_on_target component must spawn");
    assert_eq!(target.team, 7);
}

#[test]
fn lock_on_target_negative_team_fails_conversion() {
    let mut object = lock_on_target_default_object();
    object.insert("team".into(), Value::I64(-1));
    let error = spawn_single_component_entity(LOCK_ON_TARGET_COMPONENT, Value::Object(object))
        .err()
        .expect("negative team must fail conversion");
    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == LOCK_ON_TARGET_COMPONENT
    ));
}

#[test]
fn lock_on_camera_default_object_resolves_source_and_spawns_with_defaults() {
    let source_id = EntityId::generate();
    let mut object = lock_on_camera_default_object();
    object.insert("source".into(), Value::EntityRef(source_id.clone()));

    let (world, bridge, source_authoring_id, camera_id) =
        spawn_lock_on_camera_scene(object, source_id)
            .expect("default lock_on_camera scene must bridge");

    let source_entity = bridge
        .get(&source_authoring_id)
        .expect("source must be mapped");
    let camera_entity = bridge.get(&camera_id).expect("camera must be mapped");
    let camera = world
        .get_component::<LockOnCamera>(camera_entity)
        .expect("lock_on_camera component must spawn");
    assert_eq!(camera.source, source_entity);
    assert_eq!(camera.distance, 6.0);
    assert_eq!(camera.height, 2.5);
    assert!((camera.spring_strength - 0.85).abs() < 1e-6);
    assert_eq!(camera.max_target_distance, 20.0);
    assert!(camera.require_line_of_sight);
    assert_eq!(camera.team_filter, -1);
}

// An `EntityRef` to a truly missing entity is already rejected by
// `AuthoringScene::validate()` before `spawn_from_authoring_scene` ever
// reaches component spawning (same as `spawn_follow_camera_component`'s
// identical fallback, which has no full-pipeline test either). This test
// exercises the bridge-level fallback directly by building a
// `SpawnContext` whose `entity_map` does not contain the referenced id,
// bypassing scene validation on purpose.
#[test]
fn lock_on_camera_unresolved_source_skips_component_with_diagnostic() {
    let unknown_source = EntityId::generate();
    let camera_authoring_id = EntityId::generate();
    let authoring_entity = AuthoringEntity::new(camera_authoring_id, "camera");

    let mut object = lock_on_camera_default_object();
    object.insert("source".into(), Value::EntityRef(unknown_source));

    let mut world = World::new();
    let camera_entity = world.spawn().expect("spawn camera entity");
    let manifest = AssetManifest::default();
    let entity_map: HashMap<EntityId, Entity> = HashMap::new();
    let mut asset_diagnostics = Vec::new();
    let mut asset_state = BridgeAssetState::default();

    let mut context = SpawnContext {
        world: &mut world,
        authoring_entity: &authoring_entity,
        asset_root: None,
        manifest: &manifest,
        entity_map: &entity_map,
        asset_diagnostics: &mut asset_diagnostics,
        asset_state: &mut asset_state,
    };

    let result =
        spawn_lock_on_camera_component(camera_entity, &Value::Object(object), &mut context);
    assert!(result.is_ok(), "unresolved source must not be a hard error");

    assert!(
        world.get_component::<LockOnCamera>(camera_entity).is_none(),
        "camera component must be skipped when source is unresolved"
    );
    assert!(asset_diagnostics
        .iter()
        .any(|d| d.code == "scene_bridge.lock_on_camera_unresolved_source"));
}

#[test]
fn lock_on_camera_missing_source_field_fails_conversion() {
    let object = lock_on_camera_default_object();
    let source_id = EntityId::generate();
    let error = spawn_lock_on_camera_scene(object, source_id)
        .err()
        .expect("missing source field must fail conversion");
    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == LOCK_ON_CAMERA_COMPONENT
    ));
}

#[test]
fn lock_on_camera_non_positive_distance_fails_conversion() {
    let source_id = EntityId::generate();
    let mut object = lock_on_camera_default_object();
    object.insert("source".into(), Value::EntityRef(source_id.clone()));
    object.insert("distance".into(), Value::F64(0.0));
    let error = spawn_lock_on_camera_scene(object, source_id)
        .err()
        .expect("non-positive distance must fail conversion");
    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == LOCK_ON_CAMERA_COMPONENT
    ));
}

#[test]
fn lock_on_camera_spring_strength_out_of_range_fails_conversion() {
    let source_id = EntityId::generate();
    let mut object = lock_on_camera_default_object();
    object.insert("source".into(), Value::EntityRef(source_id.clone()));
    object.insert("spring_strength".into(), Value::F64(1.5));
    let error = spawn_lock_on_camera_scene(object, source_id)
        .err()
        .expect("out-of-range spring_strength must fail conversion");
    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == LOCK_ON_CAMERA_COMPONENT
    ));
}

#[test]
fn lock_on_camera_team_filter_below_negative_one_fails_conversion() {
    let source_id = EntityId::generate();
    let mut object = lock_on_camera_default_object();
    object.insert("source".into(), Value::EntityRef(source_id.clone()));
    object.insert("team_filter".into(), Value::I64(-2));
    let error = spawn_lock_on_camera_scene(object, source_id)
        .err()
        .expect("team_filter below -1 must fail conversion");
    assert!(matches!(
        error,
        SceneBridgeError::InvalidComponentValue { component_type, .. }
        if component_type.as_str() == LOCK_ON_CAMERA_COMPONENT
    ));
}

#[test]
fn persisted_game_component_requires_a_loaded_definition() {
    let mut scene = AuthoringScene::new();
    let entity = EntityId::generate();
    let component_type = ComponentTypeId::new("game.c_01k00000000000000000000000");
    let mut transaction = Transaction::begin(&scene);
    transaction.apply(AuthoringCommand::CreateEntity {
        id: entity.clone(),
        name: "game_entity".into(),
        parent: None,
    });
    transaction.apply(AuthoringCommand::AddComponent {
        entity: entity.clone(),
        component_type: component_type.clone(),
        value: Value::Object(BTreeMap::new()),
    });
    transaction.commit(&mut scene).unwrap();

    let error = spawn_from_authoring_scene(&mut World::new(), &scene)
        .expect_err("missing game code must block runtime conversion");
    assert!(matches!(
        error,
        SceneBridgeError::MissingGameComponent {
            entity: found_entity,
            component_type: found_type,
        } if found_entity == entity && found_type == component_type
    ));
}
