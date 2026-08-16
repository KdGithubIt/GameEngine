//! Builtin component registry, schema, and validation tests.

use super::*;

use super::schemas::builtin_asset_id;
use crate::asset::Assets;
use crate::lock_on::LockOnTarget;
use crate::mesh::Mesh;
use crate::particles::ParticleEmitter;
use crate::scene_bridge::*;
use engine_authoring::test_fixtures::load_scene_fixture;

#[test]
fn registry_returns_registered_definition() {
    let registry = builtin_registry();

    assert!(registry
        .get(&ComponentTypeId::new(TRANSFORM_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(PLAYER_MARKER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(STATIC_MESH_RENDERER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(SKINNED_MESH_RENDERER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(LOD_GROUP_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(CAMERA_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(DIRECTIONAL_LIGHT_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(AMBIENT_LIGHT_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(POINT_LIGHT_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(SPOT_LIGHT_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(SHADOW_SETTINGS_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(ENVIRONMENT_LIGHTING_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(POST_PROCESS_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(PLAYER_CONTROLLER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(PARTICLE_EMITTER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(UI_DOCUMENT_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(COLLIDER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(PHYSICS_BODY_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(CHARACTER_CONTROLLER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(DAMAGE_RECEIVER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(LOCK_ON_TARGET_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(LOCK_ON_CAMERA_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(NAV_MESH_AGENT_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(NAV_MESH_SURFACE_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(RUNTIME_METADATA_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(ANIMATION_CONTROLLER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(BEHAVIOR_TREE_RUNNER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(AUDIO_EMITTER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(AUDIO_LISTENER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(MUSIC_CONTROLLER_COMPONENT))
        .is_some());
    assert!(registry
        .get(&ComponentTypeId::new(FOOT_IK_COMPONENT))
        .is_some());
}

#[test]
fn camera_schema_defaults_to_an_enabled_camera_at_priority_zero() {
    let registry = builtin_registry();
    let camera = registry
        .get(&ComponentTypeId::new(CAMERA_COMPONENT))
        .expect("camera definition must be registered");
    let Value::Object(defaults) = camera.schema.default_value() else {
        panic!("camera defaults must be an object");
    };

    assert_eq!(camera.schema.version, 2);
    assert_eq!(defaults.get("enabled"), Some(&Value::Bool(true)));
    assert_eq!(defaults.get("priority"), Some(&Value::I64(0)));
}

#[test]
fn registry_returns_none_for_unknown_type() {
    let registry = builtin_registry();

    assert!(registry
        .get(&ComponentTypeId::new("gameplay.unknown"))
        .is_none());
}

#[test]
fn animation_controller_uses_the_animation_set_picker() {
    let registry = builtin_registry();
    let InspectorHint::Fields { fields } = registry
        .get(&ComponentTypeId::new(ANIMATION_CONTROLLER_COMPONENT))
        .expect("animation controller must be registered")
        .inspector
    else {
        panic!("animation controller must declare field controls");
    };
    let controller_binding = fields
        .iter()
        .find(|hint| hint.name == "animation_set")
        .expect("Animation Controller must expose animation_set");
    assert!(matches!(
        controller_binding.control,
        Some(InspectorFieldControl::AssetRef(AssetKind::AnimationSet))
    ));
    assert!(
        fields.iter().all(|hint| hint.name != "clip_source"),
        "new Animation Controller must not expose the legacy clip_source picker"
    );
}

#[test]
fn registry_iterates_in_registration_order() {
    let registry = builtin_registry();
    let types = registry
        .definitions()
        .map(|definition| definition.schema.type_id.as_str().to_owned())
        .collect::<Vec<_>>();

    assert_eq!(
        types,
        vec![
            TRANSFORM_COMPONENT,
            PLAYER_MARKER_COMPONENT,
            STATIC_MESH_RENDERER_COMPONENT,
            SKINNED_MESH_RENDERER_COMPONENT,
            SKINNED_MODEL_COMPONENT,
            BONE_ATTACHMENT_COMPONENT,
            LOD_GROUP_COMPONENT,
            CAMERA_COMPONENT,
            DIRECTIONAL_LIGHT_COMPONENT,
            AMBIENT_LIGHT_COMPONENT,
            SHADOW_SETTINGS_COMPONENT,
            ENVIRONMENT_LIGHTING_COMPONENT,
            POST_PROCESS_COMPONENT,
            PLAYER_CONTROLLER_COMPONENT,
            ORBIT_CAMERA_COMPONENT,
            FOLLOW_CAMERA_COMPONENT,
            PARTICLE_EMITTER_COMPONENT,
            UI_DOCUMENT_COMPONENT,
            COLLIDER_COMPONENT,
            PHYSICS_BODY_COMPONENT,
            CHARACTER_CONTROLLER_COMPONENT,
            DAMAGE_RECEIVER_COMPONENT,
            LOCK_ON_TARGET_COMPONENT,
            LOCK_ON_CAMERA_COMPONENT,
            NAV_MESH_AGENT_COMPONENT,
            NAV_MESH_SURFACE_COMPONENT,
            RUNTIME_METADATA_COMPONENT,
            ANIMATION_CONTROLLER_COMPONENT,
            BEHAVIOR_TREE_RUNNER_COMPONENT,
            AUDIO_EMITTER_COMPONENT,
            AUDIO_LISTENER_COMPONENT,
            MUSIC_CONTROLLER_COMPONENT,
            FOOT_IK_COMPONENT,
            SECONDARY_MOTION_COMPONENT,
            POINT_LIGHT_COMPONENT,
            SPOT_LIGHT_COMPONENT,
            VFX_PLAYER_COMPONENT,
        ]
    );
}

#[test]
fn builtin_registry_contains_every_declared_component_exactly_once() {
    let declared = builtins::builtin_components();
    let registry = builtin_registry();

    assert_eq!(registry.len(), declared.len());
    let mut seen = std::collections::BTreeSet::new();
    for component in &declared {
        assert!(
            seen.insert(component.type_id),
            "`{}` is declared twice",
            component.type_id
        );
        assert!(
            registry
                .get(&ComponentTypeId::new(component.type_id))
                .is_some(),
            "`{}` is declared but not registered",
            component.type_id
        );
    }
    let registered_order: Vec<_> = registry
        .definitions()
        .map(|definition| definition.schema.type_id.as_str().to_owned())
        .collect();
    let declared_order: Vec<_> = declared
        .iter()
        .map(|component| component.type_id.to_owned())
        .collect();
    assert_eq!(
        registered_order, declared_order,
        "registration order is part of the authoring contract"
    );
}

/// Checks every declaration against the rules the table is meant to enforce.
///
/// ADR 0054 requires this coverage for every registered component. Driving it
/// from the declaration table means a new component is checked the moment it
/// is added, instead of needing its own hand-written copy of these assertions.
#[test]
fn every_declared_component_satisfies_the_declaration_contract() {
    for component in builtins::builtin_components() {
        let id = component.type_id;
        let schema = component.schema();
        assert_eq!(
            schema.type_id.as_str(),
            id,
            "`{id}` declares a different schema type id"
        );
        assert!(!schema.display_name.is_empty(), "`{id}` needs a display name");
        assert!(!schema.description.is_empty(), "`{id}` needs a description");
        assert!(!schema.category.is_empty(), "`{id}` needs a category");
        assert!(schema.version >= 1, "`{id}` needs a schema version");

        let mut seen = std::collections::BTreeSet::new();
        for field in component.fields {
            assert!(
                seen.insert(field.name),
                "`{id}.{}` is declared twice",
                field.name
            );
            assert!(
                !field.display_name.is_empty() && !field.description.is_empty(),
                "`{id}.{}` needs a label and a description",
                field.name
            );
            assert_field_default_matches_declaration(id, field);
        }

        // A default value the component itself rejects would make "add
        // component" produce an immediately invalid entity.
        let Value::Object(defaults) = schema.default_value() else {
            continue;
        };
        for field in component.fields {
            if field.default.to_value().is_some() {
                assert!(
                    defaults.contains_key(field.name),
                    "`{id}.{}` declares a default the schema drops",
                    field.name
                );
            }
        }
    }
}

/// Asserts one declared default is consistent with the field's own control.
fn assert_field_default_matches_declaration(component: &str, field: &FieldDef) {
    let Some(default) = field.default.to_value() else {
        assert!(
            !matches!(field.control, Some(InspectorFieldControl::Enum(_))),
            "`{component}.{}` offers choices but has no default",
            field.name
        );
        return;
    };
    match field.control {
        Some(InspectorFieldControl::Number(range)) => {
            let number = match &default {
                Value::F64(value) => *value,
                Value::I64(value) => *value as f64,
                other => panic!(
                    "`{component}.{}` declares a numeric range for a {other:?} default",
                    field.name
                ),
            };
            assert!(
                range.contains(number),
                "`{component}.{}` defaults to {number}, outside its own declared range ({})",
                field.name,
                range.expectation()
            );
        }
        Some(InspectorFieldControl::Enum(options)) => {
            let Value::String(value) = &default else {
                panic!(
                    "`{component}.{}` offers choices but defaults to {default:?}",
                    field.name
                );
            };
            assert!(
                options.contains(&value.as_str()),
                "`{component}.{}` defaults to `{value}`, which is not one of its choices",
                field.name
            );
        }
        _ => {}
    }
}

#[test]
fn animation_controller_graph_requires_an_animation_set() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "animated",
                "components": {
                    "engine.animation_controller": {
                        "graph": {"$type":"asset_ref","id":"asset_01JP0000000000000000000002"},
                        "looping": true,
                        "playback_speed": 1.0,
                        "completion_event": "animation.completed",
                        "root_motion_mode": "disabled",
                        "fade_duration": 0.2,
                        "parameters": {}
                    }
                }
            }]
        }"#,
    )
    .expect("animation controller fixture must load");

    let diagnostics = validate_builtin_component_values(&scene);

    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "scene.component_dependency_missing"
            && diagnostic.message.contains("animation_set")
    }));
}

#[test]
fn animation_controller_animation_set_requires_a_graph() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "animated",
                "components": {
                    "engine.animation_controller": {
                        "skeleton": {"$type":"asset_ref","id":"asset_01JP0000000000000000000001"},
                        "animation_set": {"$type":"asset_ref","id":"asset_01JP0000000000000000000002"},
                        "looping": true,
                        "playback_speed": 1.0,
                        "completion_event": "animation.completed",
                        "root_motion_mode": "disabled",
                        "fade_duration": 0.2,
                        "parameters": {}
                    }
                }
            }]
        }"#,
    )
    .expect("animation controller fixture must load");

    let diagnostics = validate_builtin_component_values(&scene);

    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "scene.component_dependency_missing"
            && diagnostic
                .message
                .contains("animation_set` requires `graph")
    }));
}

#[test]
fn unassigned_required_asset_reference_validates_as_non_blocking_warning() {
    // The state produced by Add Component before the user assigns the
    // reference: defaulted fields are present, the required AssetRef is not.
    let scene = load_scene_fixture(
        r#"{
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "incomplete",
                "components": {
                    "engine.nav_mesh_surface": {}
                }
            }]
        }"#,
    )
    .expect("scene JSON must load");

    let diagnostics = validate_builtin_component_values(&scene);

    let unassigned: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "scene.component_reference_unassigned")
        .collect();
    assert_eq!(unassigned.len(), 1);
    assert!(
        !unassigned[0].is_blocking(),
        "an unassigned reference is an editing state, not an error"
    );
    assert!(unassigned[0].message.contains("source"));
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "scene.component_field_missing"),
        "the unassigned reference must not also be reported as a missing field"
    );
}

#[test]
fn foot_ik_without_a_sibling_skeleton_reports_a_missing_dependency() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "no_skeleton",
                "components": {
                    "engine.foot_ik": {"max_correction": 0.3, "enabled": true}
                }
            }]
        }"#,
    )
    .expect("scene JSON must load");

    let diagnostics = validate_builtin_component_values(&scene);

    assert!(
        diagnostics.iter().any(|diagnostic| diagnostic.code
            == "scene.component_dependency_missing"
            && diagnostic.message.contains("engine.foot_ik")),
        "a foot_ik component without a sibling skeleton must be reported: {diagnostics:?}"
    );
}

#[test]
fn builtin_validation_reports_invalid_enum_and_field_type() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "invalid",
                "components": {
                    "engine.physics_body": {"kind": "ghost"},
                    "engine.nav_mesh_agent": {
                        "speed": "fast",
                        "stopping_distance": 0.1,
                        "has_target": false,
                        "target_x": 0.0,
                        "target_y": 0.0,
                        "target_z": 0.0
                    }
                }
            }]
        }"#,
    )
    .expect("invalid component values are structurally valid scene JSON");

    let diagnostics = validate_builtin_component_values(&scene);

    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "scene.component_enum_value"));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "scene.component_field_type"));
    assert!(diagnostics
        .iter()
        .all(|diagnostic| matches!(diagnostic.target, Some(DiagnosticTarget::Component { .. }))));
}

#[test]
fn builtin_validation_reports_invalid_animation_parameter_rows() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "animated",
                "components": {
                    "engine.skinned_model": {
                        "skeleton": {"$type":"asset_ref","id":"asset_01JP0000000000000000000003"}
                    },
                    "engine.animation_controller": {
                        "animation_set": {"$type":"asset_ref","id":"asset_01JP0000000000000000000001"},
                        "graph": {"$type":"asset_ref","id":"asset_01JP0000000000000000000002"},
                        "looping": true,
                        "playback_speed": 1.0,
                        "completion_event": "animation.completed",
                        "root_motion_mode": "disabled",
                        "fade_duration": 0.2,
                        "parameters": {"move": true, "attack": "yes"}
                    }
                }
            }]
        }"#,
    )
    .expect("animation validation fixture");

    let diagnostics = validate_builtin_component_values(&scene);

    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "scene.animation_parameter_invalid"));
}

#[test]
fn builtin_validation_uses_inspector_ranges_and_active_conditions() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "invalid_ranges",
                "components": {
                    "engine.camera": {
                        "fov_y_degrees": 60.0,
                        "near": 2.0,
                        "far": 1.0
                    },
                    "engine.collider": {
                        "shape": "sphere",
                        "half_extent_x": -1.0,
                        "half_extent_y": -1.0,
                        "half_extent_z": -1.0,
                        "radius": 0.0,
                        "half_height": -1.0,
                        "is_trigger": false,
                        "membership": 1,
                        "mask": 1
                    },
                    "engine.nav_mesh_agent": {
                        "speed": -1.0,
                        "stopping_distance": 0.1,
                        "has_target": false,
                        "target_x": 0.0,
                        "target_y": 0.0,
                        "target_z": 0.0
                    }
                }
            }]
        }"#,
    )
    .expect("range fixture must be structurally valid");

    let diagnostics = validate_builtin_component_values(&scene);
    let range_messages = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "scene.component_number_range")
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();

    assert_eq!(range_messages.len(), 2);
    assert!(range_messages
        .iter()
        .any(|message| message.contains("radius")));
    assert!(range_messages
        .iter()
        .any(|message| message.contains("speed")));
    assert!(!range_messages
        .iter()
        .any(|message| message.contains("half_extent")));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "scene.component_field_relation"));
}

#[test]
fn numeric_range_preserves_exclusive_and_inclusive_bounds() {
    assert!(!NumericRange::greater_than(0.0).contains(0.0));
    assert!(NumericRange::greater_than(0.0).contains(f64::EPSILON));
    assert!(NumericRange::inclusive(0.0, 1.0).contains(0.0));
    assert!(NumericRange::inclusive(0.0, 1.0).contains(1.0));
    assert!(!NumericRange::inclusive(0.0, 1.0).contains(f64::NAN));
}

#[test]
fn builtin_asset_validation_reports_category_and_graph_kind_mismatches() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    std::fs::write(
        directory.path().join("enemy.graph.json"),
        r#"{"kind":"anim.graph"}"#,
    )
    .expect("graph fixture must be written");
    std::fs::write(directory.path().join("not_audio.png"), b"not an image")
        .expect("category fixture must be written");
    let graph_asset = AssetId::generate();
    let audio_asset = AssetId::generate();
    let mut manifest = AssetManifest::default();
    manifest.insert(
        graph_asset.clone(),
        crate::asset::ManifestEntry {
            path: "enemy.graph.json".into(),
            name: None,
            import_settings: crate::asset::ImportSettings::default(),
        },
    );
    manifest.insert(
        audio_asset.clone(),
        crate::asset::ManifestEntry {
            path: "not_audio.png".into(),
            name: None,
            import_settings: crate::asset::ImportSettings::default(),
        },
    );
    let scene_json = serde_json::json!({
        "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "invalid_assets",
            "components": {
                (TRANSFORM_COMPONENT): {"x": 0.0, "y": 0.0, "z": 0.0},
                (AUDIO_EMITTER_COMPONENT): {
                    "clip": {"$type": "asset_ref", "id": audio_asset.as_str()},
                    "volume": 1.0,
                    "spatial_blend": 1.0,
                    "min_distance": 1.0,
                    "max_distance": 10.0,
                    "autoplay": false
                },
                (BEHAVIOR_TREE_RUNNER_COMPONENT): {
                    "graph": {"$type": "asset_ref", "id": graph_asset.as_str()},
                    "blackboard": {},
                    "enabled": true
                }
            }
        }]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture must load");

    let diagnostics = validate_builtin_component_assets(&scene, &manifest, Some(directory.path()));

    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "scene.asset_category_mismatch"));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "scene.asset_graph_kind_mismatch"));
    assert!(diagnostics
        .iter()
        .all(|diagnostic| matches!(diagnostic.target, Some(DiagnosticTarget::Component { .. }))));
}

/// FBXなどのモデルソースから派生したSkeletonサブアセットを、
/// Skinned ModelのSkeletonフィールドが正常に参照できることを確認する。
///
/// この組み合わせが互換表から漏れると、実際の種類と要求種類がどちらも
/// Skeletonであるにもかかわらず、`scene.asset_category_mismatch`が発生する。
#[test]
fn builtin_asset_validation_accepts_imported_skeleton_for_skinned_model() {
    // インポート済みモデルのソースファイルが存在する状態を再現する。
    // このテストではFBXの解析自体は行わず、検証処理が確認するファイル存在だけを用意する。
    let directory = tempfile::tempdir().expect("temporary model asset root");
    std::fs::write(directory.path().join("character.fbx"), b"fbx fixture")
        .expect("model source fixture must be written");

    // SkeletonサブアセットのIDは、本番のインポート処理と同じ決定的な導出関数で生成する。
    // 手書きIDを使わないことで、マニフェストの逆引き条件も本番と同じにする。
    let source_asset = AssetId::generate();
    let skeleton_asset = crate::asset::imported_sub_asset_id(
        &source_asset,
        crate::asset::ImportedSubAssetKind::Skeleton,
        0,
    );

    // トップレベルにはFBXソースだけを登録し、Skeletonはその派生サブアセットとして保持する。
    // 派生アセットをトップレベルへ重複登録しないのは、通常のインポート契約と同じである。
    let mut manifest = AssetManifest::default();
    manifest.insert(
        source_asset.clone(),
        crate::asset::ManifestEntry {
            path: "character.fbx".into(),
            name: Some("Character".into()),
            import_settings: crate::asset::ImportSettings {
                sub_assets: vec![crate::asset::ImportedSubAsset {
                    id: skeleton_asset.as_str().to_owned(),
                    kind: crate::asset::ImportedSubAssetKind::Skeleton,
                    name: "Character Skeleton".into(),
                    index: 0,
                    target_model_source: None,
                }],
                ..crate::asset::ImportSettings::default()
            },
        },
    );

    // Skinned ModelはSkeletonサブアセットを直接参照する。
    // Render Partの有無はこのカテゴリー検証と無関係なので、最小構成として空配列を使用する。
    let scene_json = serde_json::json!({
        "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "character",
            "components": {
                (SKINNED_MODEL_COMPONENT): {
                    "skeleton": {
                        "$type": "asset_ref",
                        "id": skeleton_asset.as_str()
                    }
                }
            }
        }]
    });
    let scene =
        load_scene_fixture(&scene_json.to_string()).expect("skinned model scene must load");

    // 本番と同じ組み込みコンポーネントのアセット検証を実行する。
    let diagnostics = validate_builtin_component_assets(&scene, &manifest, Some(directory.path()));

    // SkeletonからSkeletonへの参照は正しいため、カテゴリー不一致を報告してはならない。
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "scene.asset_category_mismatch"),
        "an imported Skeleton must satisfy a Skeleton field: {diagnostics:#?}"
    );
}

#[test]
fn filesystem_validation_uses_source_stamp_and_skips_an_entry_without_one() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let source_path = directory.path().join("character.glb");
    std::fs::write(&source_path, b"initial model bytes").expect("model fixture");
    let source_asset = AssetId::generate();
    let mesh_asset = crate::asset::imported_sub_asset_id(
        &source_asset,
        crate::asset::ImportedSubAssetKind::Mesh,
        0,
    );
    let stamp = crate::asset::SourceStamp::capture(&source_path, &[])
        .expect("initial metadata stamp");
    let mut manifest = AssetManifest::default();
    manifest.insert(
        source_asset.clone(),
        crate::asset::ManifestEntry {
            path: "character.glb".into(),
            name: Some("Character".into()),
            import_settings: crate::asset::ImportSettings {
                source_fingerprint: Some("fnv1a64:test".into()),
                source_stamp: Some(stamp),
                sub_assets: vec![crate::asset::ImportedSubAsset {
                    id: mesh_asset.as_str().to_owned(),
                    kind: crate::asset::ImportedSubAssetKind::Mesh,
                    name: "Body".into(),
                    index: 0,
                    target_model_source: None,
                }],
                ..crate::asset::ImportSettings::default()
            },
        },
    );
    let scene_json = serde_json::json!({
        "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "character",
            "components": {
                (STATIC_MESH_RENDERER_COMPONENT): {
                    "mesh": {"$type": "asset_ref", "id": mesh_asset.as_str()},
                    "material": {"$type": "asset_ref", "id": BUILTIN_BLUE_MATERIAL_ASSET_ID},
                    "material_slots": []
                }
            }
        }]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture");

    let unchanged = validate_builtin_component_asset_files(&scene, &manifest, directory.path());
    assert!(unchanged
        .iter()
        .all(|diagnostic| diagnostic.code != "scene.import_source_changed"));

    std::fs::write(&source_path, b"changed model bytes with another length")
        .expect("changed model fixture");
    let changed = validate_builtin_component_asset_files(&scene, &manifest, directory.path());
    assert!(changed
        .iter()
        .any(|diagnostic| diagnostic.code == "scene.import_source_changed"));

    manifest
        .get_mut(&source_asset)
        .expect("source entry")
        .import_settings
        .source_stamp = None;
    let unstamped = validate_builtin_component_asset_files(&scene, &manifest, directory.path());
    assert!(unstamped
        .iter()
        .all(|diagnostic| diagnostic.code != "scene.import_source_changed"));
}

#[test]
fn material_dependency_validation_reports_nested_texture_errors() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let material_id = AssetId::generate();
    let missing_texture = AssetId::generate();
    let material = engine_authoring::MaterialAsset {
        base_color_texture: Some(missing_texture.clone()),
        ..engine_authoring::MaterialAsset::default()
    };
    std::fs::write(
        directory.path().join("surface.material.json"),
        material.to_json().expect("material JSON"),
    )
    .expect("material fixture");
    let mut manifest = AssetManifest::default();
    manifest.insert(
        material_id.clone(),
        crate::asset::ManifestEntry {
            path: "surface.material.json".into(),
            name: None,
            import_settings: crate::asset::ImportSettings::default(),
        },
    );
    let scene_json = serde_json::json!({
        "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "material_user",
            "components": {
                (STATIC_MESH_RENDERER_COMPONENT): {
                    "mesh": {"$type": "asset_ref", "id": BUILTIN_TRIANGLE_ASSET_ID},
                    "material": {"$type": "asset_ref", "id": material_id.as_str()},
                    "material_slots": []
                }
            }
        }]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture");

    let diagnostics = validate_builtin_component_assets(&scene, &manifest, Some(directory.path()));

    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "scene.material_texture_unregistered"
            && diagnostic.message.contains(missing_texture.as_str())
    }));
}

#[test]
fn material_dependency_validation_reports_oversized_texture() {
    let directory = tempfile::tempdir().expect("temporary asset root");
    let material_id = AssetId::generate();
    let texture_id = AssetId::generate();
    image::RgbaImage::new(crate::render_limits::MAX_TEXTURE_DIMENSION + 1, 1)
        .save(directory.path().join("wide.png"))
        .expect("oversized texture fixture");
    let material = engine_authoring::MaterialAsset {
        base_color_texture: Some(texture_id.clone()),
        ..engine_authoring::MaterialAsset::default()
    };
    std::fs::write(
        directory.path().join("wide.material.json"),
        material.to_json().expect("material JSON"),
    )
    .expect("material fixture");
    let mut manifest = AssetManifest::default();
    for (id, path) in [
        (material_id.clone(), "wide.material.json"),
        (texture_id, "wide.png"),
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
            "name": "wide_texture",
            "components": {
                (STATIC_MESH_RENDERER_COMPONENT): {
                    "mesh": {"$type": "asset_ref", "id": BUILTIN_TRIANGLE_ASSET_ID},
                    "material": {"$type": "asset_ref", "id": material_id.as_str()},
                    "material_slots": []
                }
            }
        }]
    });
    let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture");

    let diagnostics = validate_builtin_component_assets(&scene, &manifest, Some(directory.path()));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "renderer.texture_dimension_limit"));
}

#[test]
fn builtin_registry_exposes_schema_driven_field_controls() {
    let registry = builtin_registry();
    let player = registry
        .get(&ComponentTypeId::new(PLAYER_CONTROLLER_COMPONENT))
        .expect("player controller must be registered");
    let InspectorHint::Fields { fields } = player.inspector else {
        panic!("player controller must expose field-level controls");
    };
    assert!(fields.iter().any(|hint| {
        hint.name == "move_plane"
            && hint.control == Some(InspectorFieldControl::Enum(&["xz", "xy"]))
    }));

    let collider = registry
        .get(&ComponentTypeId::new(COLLIDER_COMPONENT))
        .expect("collider must be registered");
    let InspectorHint::Fields { fields } = collider.inspector else {
        panic!("collider must expose field-level controls");
    };
    assert!(fields.iter().any(|hint| {
        hint.name == "mask" && hint.control == Some(InspectorFieldControl::LayerMask)
    }));

    let nav_agent = registry
        .get(&ComponentTypeId::new(NAV_MESH_AGENT_COMPONENT))
        .expect("navigation agent must be registered");
    let InspectorHint::Fields { fields } = nav_agent.inspector else {
        panic!("navigation agent must expose field-level controls");
    };
    assert!(fields.iter().any(|hint| {
        hint.name == "target_x"
            && hint.visible_when
                == Some(InspectorFieldCondition::Bool {
                    field: "has_target",
                    equals: true,
                })
    }));

    let lod = registry
        .get(&ComponentTypeId::new(LOD_GROUP_COMPONENT))
        .expect("LOD group must be registered");
    let InspectorHint::Fields { fields } = lod.inspector else {
        panic!("LOD group must expose its structured editor");
    };
    assert_eq!(fields[0].control, Some(InspectorFieldControl::LodLevels));

    let skinned_model = registry
        .get(&ComponentTypeId::new(SKINNED_MODEL_COMPONENT))
        .expect("Skinned Model must be registered");
    let InspectorHint::Fields { fields } = skinned_model.inspector else {
        panic!("Skinned Model must expose field controls");
    };
    assert!(fields.iter().any(|hint| {
        hint.name == "skeleton"
            && hint.control == Some(InspectorFieldControl::AssetRef(AssetKind::Skeleton))
    }));

    let skinned = registry
        .get(&ComponentTypeId::new(SKINNED_MESH_RENDERER_COMPONENT))
        .expect("skinned mesh renderer must be registered");
    let InspectorHint::Fields { fields } = skinned.inspector else {
        panic!("skinned mesh must expose field controls");
    };
    assert!(fields.iter().any(|hint| {
        hint.name == "mesh"
            && hint.control == Some(InspectorFieldControl::AssetRef(AssetKind::Mesh))
    }));
}

#[test]
fn lod_validation_rejects_empty_non_positive_and_unordered_levels() {
    let scene = load_scene_fixture(
        r#"{
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "bad_lod",
                "components": {
                    "engine.lod_group": {"levels": [
                        {"distance": 10.0, "mesh": {"$type":"asset_ref","id":"asset_01JP0000000000000000000101"}},
                        {"distance": 5.0, "mesh": {"$type":"asset_ref","id":"asset_01JP0000000000000000000102"}},
                        {"distance": 0.0, "mesh": {"$type":"asset_ref","id":"asset_01JP0000000000000000000102"}}
                    ]}
                }
            }]
        }"#,
    )
    .expect("LOD fixture JSON must load");

    let diagnostics = validate_builtin_component_values(&scene);
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "scene.lod_distance_order"));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "scene.lod_distance_invalid"));
}

#[test]
fn asset_component_defaults_match_existing_editor_defaults() {
    let registry = builtin_registry();

    let static_renderer = registry
        .get(&ComponentTypeId::new(STATIC_MESH_RENDERER_COMPONENT))
        .expect("static mesh renderer schema must exist");
    let Value::Object(fields) = static_renderer.schema.default_value() else {
        panic!("static mesh renderer default must be an object");
    };
    assert_eq!(
        fields.get("mesh"),
        Some(&Value::AssetRef(builtin_asset_id(
            BUILTIN_TRIANGLE_ASSET_ID
        )))
    );
    assert_eq!(
        fields.get("material"),
        Some(&Value::AssetRef(builtin_asset_id(
            BUILTIN_WHITE_MATERIAL_ASSET_ID
        )))
    );
    assert_eq!(
        fields.get("material_slots"),
        Some(&Value::Array(Vec::new()))
    );
}

#[test]
fn ui_document_default_is_the_builtin_ui_document_asset() {
    let registry = builtin_registry();

    let ui_document = registry
        .get(&ComponentTypeId::new(UI_DOCUMENT_COMPONENT))
        .expect("ui_document schema must exist");

    assert_eq!(
        ui_document.schema.default_value(),
        Value::AssetRef(builtin_asset_id(BUILTIN_UI_DOCUMENT_ASSET_ID))
    );
}

#[test]
fn particle_emitter_schema_defaults_match_runtime_particle_emitter_new() {
    let registry = builtin_registry();
    let particle_emitter = registry
        .get(&ComponentTypeId::new(PARTICLE_EMITTER_COMPONENT))
        .expect("particle emitter schema must exist");

    let mut meshes = Assets::<Mesh>::default();
    let runtime_default = ParticleEmitter::new(meshes.add(Mesh::triangle()));

    let Value::Object(fields) = particle_emitter.schema.default_value() else {
        panic!("particle emitter default value must be an object");
    };

    assert_eq!(
        fields.get("mesh"),
        Some(&Value::AssetRef(builtin_asset_id(BUILTIN_QUAD_ASSET_ID)))
    );
    assert_eq!(
        fields.get("material"),
        Some(&Value::AssetRef(builtin_asset_id(
            BUILTIN_WHITE_MATERIAL_ASSET_ID
        )))
    );
    assert_eq!(
        fields.get("spawn_rate"),
        Some(&Value::F64(runtime_default.spawn_rate as f64))
    );
    assert_eq!(
        fields.get("lifetime_min"),
        Some(&Value::F64(runtime_default.lifetime.0 as f64))
    );
    assert_eq!(
        fields.get("lifetime_max"),
        Some(&Value::F64(runtime_default.lifetime.1 as f64))
    );
    assert_eq!(
        fields.get("initial_speed_min"),
        Some(&Value::F64(runtime_default.initial_speed.0 as f64))
    );
    assert_eq!(
        fields.get("initial_speed_max"),
        Some(&Value::F64(runtime_default.initial_speed.1 as f64))
    );
    assert_eq!(
        fields.get("direction_x"),
        Some(&Value::F64(runtime_default.direction.x as f64))
    );
    assert_eq!(
        fields.get("direction_y"),
        Some(&Value::F64(runtime_default.direction.y as f64))
    );
    assert_eq!(
        fields.get("direction_z"),
        Some(&Value::F64(runtime_default.direction.z as f64))
    );
    assert_eq!(
        fields.get("spread"),
        Some(&Value::F64(runtime_default.spread as f64))
    );
    assert_eq!(
        fields.get("gravity_x"),
        Some(&Value::F64(runtime_default.gravity.x as f64))
    );
    assert_eq!(
        fields.get("gravity_y"),
        Some(&Value::F64(runtime_default.gravity.y as f64))
    );
    assert_eq!(
        fields.get("gravity_z"),
        Some(&Value::F64(runtime_default.gravity.z as f64))
    );
    assert_eq!(
        fields.get("start_color_r"),
        Some(&Value::F64(runtime_default.start_color[0] as f64))
    );
    assert_eq!(
        fields.get("start_color_g"),
        Some(&Value::F64(runtime_default.start_color[1] as f64))
    );
    assert_eq!(
        fields.get("start_color_b"),
        Some(&Value::F64(runtime_default.start_color[2] as f64))
    );
    assert_eq!(
        fields.get("start_color_a"),
        Some(&Value::F64(runtime_default.start_color[3] as f64))
    );
    assert_eq!(
        fields.get("end_color_r"),
        Some(&Value::F64(runtime_default.end_color[0] as f64))
    );
    assert_eq!(
        fields.get("end_color_g"),
        Some(&Value::F64(runtime_default.end_color[1] as f64))
    );
    assert_eq!(
        fields.get("end_color_b"),
        Some(&Value::F64(runtime_default.end_color[2] as f64))
    );
    assert_eq!(
        fields.get("end_color_a"),
        Some(&Value::F64(runtime_default.end_color[3] as f64))
    );
    assert_eq!(
        fields.get("start_size"),
        Some(&Value::F64(runtime_default.start_size as f64))
    );
    assert_eq!(
        fields.get("end_size"),
        Some(&Value::F64(runtime_default.end_size as f64))
    );
    assert_eq!(
        fields.get("max_particles"),
        Some(&Value::I64(runtime_default.max_particles as i64))
    );
    assert_eq!(
        fields.get("seed"),
        Some(&Value::I64(runtime_default.seed as i64))
    );
    assert_eq!(
        fields.len(),
        26,
        "every particle emitter field must have a default"
    );
}

#[test]
fn lock_on_target_default_matches_runtime_default() {
    let registry = builtin_registry();
    let lock_on_target = registry
        .get(&ComponentTypeId::new(LOCK_ON_TARGET_COMPONENT))
        .expect("lock_on_target schema must exist");

    let Value::Object(fields) = lock_on_target.schema.default_value() else {
        panic!("lock_on_target default value must be an object");
    };
    assert_eq!(
        fields.get("team"),
        Some(&Value::I64(i64::from(LockOnTarget::default().team)))
    );
}

#[test]
fn lock_on_camera_schema_field_defaults_match_the_phase_58_spec() {
    let registry = builtin_registry();
    let lock_on_camera = registry
        .get(&ComponentTypeId::new(LOCK_ON_CAMERA_COMPONENT))
        .expect("lock_on_camera schema must exist");

    let Value::Object(fields) = lock_on_camera.schema.default_value() else {
        panic!("lock_on_camera default value must be an object");
    };
    // "source" has no default (it is a required entity reference), so it
    // is intentionally absent from the default object.
    assert!(!fields.contains_key("source"));
    assert_eq!(fields.get("distance"), Some(&Value::F64(6.0)));
    assert_eq!(fields.get("height"), Some(&Value::F64(2.5)));
    assert_eq!(fields.get("spring_strength"), Some(&Value::F64(0.85)));
    assert_eq!(fields.get("max_target_distance"), Some(&Value::F64(20.0)));
    assert_eq!(
        fields.get("require_line_of_sight"),
        Some(&Value::Bool(true))
    );
    assert_eq!(fields.get("team_filter"), Some(&Value::I64(-1)));
}

#[test]
fn a_renderer_has_one_unambiguous_model_reference() {
    let scene = load_scene_fixture(
        &serde_json::json!({
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "first",
                    "components": {
                        (SKINNED_MODEL_COMPONENT): {
                            "skeleton": {"$type": "asset_ref", "id": "asset_01JP0000000000000000000001"}
                        }
                    }
                },
                {
                    "id": "entity_01JP0000000000000000000002",
                    "name": "second",
                    "components": {
                        (SKINNED_MODEL_COMPONENT): {
                            "skeleton": {"$type": "asset_ref", "id": "asset_01JP0000000000000000000002"}
                        }
                    }
                },
                {
                    "id": "entity_01JP0000000000000000000003",
                    "name": "body",
                    "components": {
                        (SKINNED_MESH_RENDERER_COMPONENT): {
                            "model": {"$type": "entity_ref", "id": "entity_01JP0000000000000000000002"},
                            "material_slots": []
                        }
                    }
                }
            ]
        })
        .to_string(),
    )
    .expect("scene");

    let diagnostics = validate_builtin_component_values(&scene);

    assert!(diagnostics
        .iter()
        .all(|diagnostic| diagnostic.code != "scene.render_part_claimed_twice"));
}

#[test]
fn a_model_reference_that_targets_a_non_model_is_reported() {
    let scene = load_scene_fixture(
        &serde_json::json!({
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "coin",
                    "components": {
                        (TRANSFORM_COMPONENT): {"x": 0.0, "y": 0.0, "z": 0.0}
                    }
                },
                {
                    "id": "entity_01JP0000000000000000000002",
                    "name": "body",
                    "components": {
                        (SKINNED_MESH_RENDERER_COMPONENT): {
                            "model": {"$type": "entity_ref", "id": "entity_01JP0000000000000000000001"},
                            "material_slots": []
                        }
                    }
                }
            ]
        })
        .to_string(),
    )
    .expect("scene");

    let diagnostics = validate_builtin_component_values(&scene);

    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "scene.entity_reference_wrong_target"));
}

#[test]
fn a_model_pointing_at_a_non_model_is_reported() {
    let scene = load_scene_fixture(
        &serde_json::json!({
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "coin",
                    "components": {(TRANSFORM_COMPONENT): {"x": 0.0, "y": 0.0, "z": 0.0}}
                },
                {
                    "id": "entity_01JP0000000000000000000002",
                    "name": "weapon",
                    "components": {
                        (SKINNED_MESH_RENDERER_COMPONENT): {
                            "model": {"$type": "entity_ref", "id": "entity_01JP0000000000000000000001"},
                            "material_slots": []
                        }
                    }
                }
            ]
        })
        .to_string(),
    )
    .expect("scene");

    let diagnostics = validate_builtin_component_values(&scene);

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "scene.entity_reference_wrong_target"),
        "a Coin is not a rig, and picking one must be reported rather than silently ignored"
    );
}

#[test]
fn an_animation_controller_without_a_skinned_model_is_reported() {
    let scene = load_scene_fixture(
        &serde_json::json!({
            "entities": [{
                "id": "entity_01JP0000000000000000000001",
                "name": "character",
                "components": {(ANIMATION_CONTROLLER_COMPONENT): {"enabled": true}}
            }]
        })
        .to_string(),
    )
    .expect("scene");

    let diagnostics = validate_builtin_component_values(&scene);

    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "scene.component_dependency_missing"));
}

#[test]
fn pmx_path_matches_mesh_and_gltf_source_kinds() {
    // Regression guard: the editor's "Register Asset" action and every
    // other `GltfSource`-gated flow (reimport, import settings, build
    // packaging) depend on `asset_path_matches_kind` recognizing `.pmx`
    // exactly like `.fbx` (ADR 0097 §2). This was missed once already —
    // a sibling classifier in `engine-editor::asset_browser` (a
    // display-only lookalike `AssetKind`) was updated instead of this,
    // the actually-authoritative, function, which silently hid the
    // Register Asset menu item for `.pmx` files.
    let path = std::path::Path::new("character.pmx");
    assert!(
        asset_path_matches_kind(AssetKind::Mesh, path),
        "AssetKind::Mesh must accept .pmx"
    );
    assert!(
        asset_path_matches_kind(AssetKind::GltfSource, path),
        "AssetKind::GltfSource must accept .pmx"
    );
    // PMX carries no animation of its own (motion lives in a separate
    // `.vmd` source per ADR 0097 §3), so it must NOT be treated as an
    // AnimationClip or MotionSource path.
    assert!(!asset_path_matches_kind(AssetKind::AnimationClip, path));
    assert!(!asset_path_matches_kind(AssetKind::MotionSource, path));
}

#[test]
fn bmp_path_matches_texture_kind_case_insensitively() {
    // This authoritative predicate gates manifest registration, material
    // pickers, validation, packaging, and the UI image source picker.
    assert!(asset_path_matches_kind(
        AssetKind::Texture,
        std::path::Path::new("textures/legacy.BMP")
    ));
    assert!(!asset_path_matches_kind(
        AssetKind::Mesh,
        std::path::Path::new("textures/legacy.BMP")
    ));
}

#[test]
fn vmd_path_matches_motion_source_and_animation_clip_kinds_only() {
    let path = std::path::Path::new("dance.vmd");
    assert!(
        asset_path_matches_kind(AssetKind::MotionSource, path),
        "AssetKind::MotionSource must accept .vmd"
    );
    // A `.vmd` is a source of importable clips, so an AnimationClip
    // reference may point at the file itself, exactly as it may point at a
    // `.gltf`/`.fbx` (ADR 0097 §3).
    assert!(
        asset_path_matches_kind(AssetKind::AnimationClip, path),
        "AssetKind::AnimationClip must accept .vmd"
    );
    // A motion carries no geometry and no rig, so it must never be routed
    // through the model importer: `GltfSource` is what every mesh/skin/
    // prefab-producing flow gates on.
    assert!(
        !asset_path_matches_kind(AssetKind::GltfSource, path),
        "AssetKind::GltfSource must reject .vmd"
    );
    assert!(!asset_path_matches_kind(AssetKind::Mesh, path));
}
