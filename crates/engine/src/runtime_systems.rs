//! Shared host-profile registration for editor Play and packaged players.

use crate::anim_graph::anim_graph_system;
use crate::animation::{
    animation_system, root_motion_motor_system, AnimationClip, AnimationEvents,
};
use crate::asset::Assets;
use crate::audio::authored_audio_system;
use crate::behavior_tree::{behavior_tree_tick_system, BehaviorTreeBehaviorRegistry};
use crate::camera::{follow_camera_system, lock_on_camera_system, orbit_camera_system};
use crate::character_controller::character_controller_system;
use crate::collision::{collision_detection_system, CollisionEvents, CollisionStats};
use crate::combat::{
    apply_knockback_system, combat_contact_system, combat_debug_draw_system, HitResults,
    KnockbackRequests,
};
use crate::event_debug::{runtime_event_timeline_system, RuntimeEventTimeline};
use crate::foot_ik::foot_ik_system;
use crate::lock_on::lock_on_system;
use crate::secondary_motion::{
    secondary_motion_presentation_system, secondary_motion_system, SecondaryMotionWorlds,
};
use crate::navmesh::{nav_mesh_agent_system, nav_mesh_debug_draw_system};
use crate::physics::velocity_system;
use crate::player::{player_character_motor_system, player_controller_system};
use crate::pose_graph::PoseArena;
use crate::rig_pose::{publish_final_rig_pose_system, rig_pose_clear_transient_system};
use crate::transform::transform_propagation_system;
use crate::App;
use engine_ecs::{SystemDescriptor, SystemOrigin, SystemRegistrationError};

/// Registers the shared gameplay systems used by every runtime host.
///
/// The same descriptors are used when the editor builds its Systems panel
/// catalog, Editor Play, and packaged Player. Host-specific callers may add
/// presentation systems after this function, but gameplay system parity must
/// not depend on the host.
pub fn register_runtime_systems(app: &mut App) -> Result<(), SystemRegistrationError> {
    if app
        .world()
        .get_resource::<Assets<AnimationClip>>()
        .is_none()
    {
        app.insert_resource(Assets::<AnimationClip>::default());
    }
    if app.world().get_resource::<AnimationEvents>().is_none() {
        app.insert_resource(AnimationEvents::default());
    }
    if app.world().get_resource::<PoseArena>().is_none() {
        app.insert_resource(PoseArena::new());
    }
    if app.world().get_resource::<CollisionEvents>().is_none() {
        app.insert_resource(CollisionEvents::default());
    }
    if app.world().get_resource::<CollisionStats>().is_none() {
        app.insert_resource(CollisionStats::default());
    }
    if app.world().get_resource::<SecondaryMotionWorlds>().is_none() {
        app.insert_resource(SecondaryMotionWorlds::default());
    }
    if app.world().get_resource::<HitResults>().is_none() {
        app.insert_resource(HitResults::default());
    }
    if app.world().get_resource::<RuntimeEventTimeline>().is_none() {
        app.insert_resource(RuntimeEventTimeline::default());
    }
    if app.world().get_resource::<KnockbackRequests>().is_none() {
        app.insert_resource(KnockbackRequests::default());
    }
    if app
        .world()
        .get_resource::<BehaviorTreeBehaviorRegistry>()
        .is_none()
    {
        app.insert_resource(BehaviorTreeBehaviorRegistry::default());
    }
    app.try_add_system_with_descriptor(
        engine_system(
            "engine.combat_debug",
            "Combat Debug",
            "Draws character velocity, grounded state, active hitboxes, and contact normals.",
        ),
        combat_debug_draw_system,
    )?;
    app.try_add_system_with_descriptor(
        engine_system(
            "engine.navigation_debug",
            "Navigation Debug",
            "Draws the baked NavMesh and live agent paths.",
        ),
        nav_mesh_debug_draw_system,
    )?;
    app.try_add_system_with_descriptor(
        engine_system(
            "engine.behavior_tree_tick",
            "Behavior Tree Tick",
            "Ticks compiled behavior tree runners once per frame.",
        ),
        behavior_tree_tick_system,
    )?;
    app.try_add_system_with_descriptor(
        engine_system(
            "engine.authored_audio",
            "Authored Audio",
            "Applies one-shot emitter and music-controller autoplay requests.",
        ),
        authored_audio_system,
    )?;
    app.try_add_system_with_descriptor(
        engine_system(
            "engine.secondary_motion_presentation",
            "Secondary Motion Presentation",
            "Interpolates fixed-step secondary-motion poses for smooth render-time presentation.",
        )
        .try_before("engine.transform_propagation")
        .expect("built-in system IDs are valid"),
        secondary_motion_presentation_system,
    )?;
    app.try_add_system_with_descriptor(
        engine_system(
            "engine.player_controller",
            "Player Input",
            "Captures configured actions for the fixed-step character motor.",
        ),
        player_controller_system,
    )?;
    app.try_add_system_with_descriptor(
        engine_system(
            "engine.lock_on_selection",
            "Lock-on Selection",
            "Selects and validates the active lock-on target.",
        ),
        lock_on_system,
    )?;
    app.try_add_system_with_descriptor(
        engine_system(
            "engine.lock_on_camera",
            "Lock-on Camera",
            "Orients lock-on cameras toward the selected target.",
        )
        .try_after("engine.lock_on_selection")
        .expect("built-in system IDs are valid"),
        lock_on_camera_system,
    )?;
    app.try_add_system_with_descriptor(
        engine_system(
            "engine.orbit_camera",
            "Orbit Camera",
            "Updates orbit cameras from input and target transforms.",
        ),
        orbit_camera_system,
    )?;
    app.try_add_system_with_descriptor(
        engine_system(
            "engine.follow_camera",
            "Follow Camera",
            "Updates follow cameras from their tracked targets.",
        ),
        follow_camera_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.rig_pose_clear_transient",
            "Clear Transient Rig Pose",
            "Clears procedural and physics pose layers before fixed-step evaluation.",
        )
        .try_before("engine.animation_graph")
        .expect("built-in system IDs are valid"),
        rig_pose_clear_transient_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.animation_graph",
            "Animation Graph",
            "Evaluates project animation graph conditions before sampling.",
        ),
        anim_graph_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.animation",
            "Animation",
            "Advances animation clips and evaluates animation pose layers.",
        )
        .try_after("engine.animation_graph")
        .expect("built-in system IDs are valid"),
        animation_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.root_motion_motor",
            "Root Motion Motor",
            "Converts extracted local animation motion into a collision-resolved motor request.",
        )
        .try_after("engine.animation")
        .expect("built-in system IDs are valid"),
        root_motion_motor_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.foot_ik",
            "Foot IK",
            "Writes planted-foot corrections to the procedural pose layer.",
        )
        .try_after("engine.fixed_transform_propagation")
        .expect("built-in system IDs are valid"),
        foot_ik_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.velocity_integration",
            "Velocity Integration",
            "Advances Rapier gameplay physics, including gravity and contact response, and publishes dynamic transforms and velocities.",
        ),
        velocity_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.navigation_agent",
            "Navigation Agent",
            "Advances navigation agents along host-computed paths.",
        ),
        nav_mesh_agent_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.player_character_motor",
            "Player Character Motor",
            "Applies camera-relative input intent to kinematic velocity.",
        ),
        player_character_motor_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.character_controller",
            "Character Controller",
            "Integrates kinematic character movement and static obstacle response.",
        )
        .try_after("engine.player_character_motor")
        .expect("built-in system IDs are valid")
        .try_after("engine.root_motion_motor")
        .expect("built-in system IDs are valid"),
        character_controller_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.fixed_transform_propagation",
            "Fixed Transform Propagation",
            "Refreshes world transforms after fixed-step movement before collision detection.",
        )
        .try_after("engine.animation")
        .expect("built-in system IDs are valid")
        .try_after("engine.velocity_integration")
        .expect("built-in system IDs are valid")
        .try_after("engine.navigation_agent")
        .expect("built-in system IDs are valid")
        .try_after("engine.character_controller")
        .expect("built-in system IDs are valid"),
        transform_propagation_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.secondary_motion",
            "Secondary Motion",
            "Advances isolated secondary-motion bodies and writes the physics pose layer.",
        )
        .try_after("engine.fixed_transform_propagation")
        .expect("built-in system IDs are valid")
        .try_after("engine.foot_ik")
        .expect("built-in system IDs are valid"),
        secondary_motion_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.rig_pose_publish_final",
            "Publish Final Rig Pose",
            "Composes pose layers and publishes final local joint transforms.",
        )
        .try_after("engine.secondary_motion")
        .expect("built-in system IDs are valid")
        .try_after("engine.foot_ik")
        .expect("built-in system IDs are valid"),
        publish_final_rig_pose_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.secondary_motion_transform_propagation",
            "Final Rig Transform Propagation",
            "Refreshes joint world transforms after publishing final rig poses.",
        )
        .try_after("engine.rig_pose_publish_final")
        .expect("built-in system IDs are valid"),
        transform_propagation_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.collision_detection",
            "Collision Detection",
            "Detects current fixed-step overlaps, resolves bodies, and publishes contacts.",
        )
        .try_after("engine.secondary_motion_transform_propagation")
        .expect("built-in system IDs are valid"),
        collision_detection_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.combat_contacts",
            "Combat Contacts",
            "Applies hitbox damage, invulnerability, and hit-result generation.",
        )
        .try_after("engine.collision_detection")
        .expect("built-in system IDs are valid"),
        combat_contact_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.knockback",
            "Combat Knockback",
            "Applies accepted hit impulses to character motor velocity.",
        )
        .try_after("engine.combat_contacts")
        .expect("built-in system IDs are valid"),
        apply_knockback_system,
    )?;
    app.try_add_fixed_system_with_descriptor(
        engine_system(
            "engine.event_timeline",
            "Combat / Animation Event Timeline",
            "Retains a bounded fixed-step timeline of animation markers and accepted combat hits.",
        )
        .try_after("engine.animation")
        .expect("built-in system IDs are valid")
        .try_after("engine.knockback")
        .expect("built-in system IDs are valid"),
        runtime_event_timeline_system,
    )?;
    Ok(())
}

fn engine_system(id: &str, display_name: &str, description: &str) -> SystemDescriptor {
    SystemDescriptor::new(id, display_name, SystemOrigin::Engine)
        .expect("built-in system IDs are valid")
        .with_description(description)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        clear_input_transitions, drain_virtual_input, GamepadAxis, GamepadId, InputActionMap,
        InputCommand, InputSource, KeyCode, VirtualInputQueue,
    };
    use engine_authoring::project_settings::{AxisBinding, ProjectSystemSchedule};
    use engine_authoring::{InputAction, ProjectSettings};

    fn fingerprint(app: &App) -> Vec<String> {
        [
            ProjectSystemSchedule::Update,
            ProjectSystemSchedule::FixedUpdate,
        ]
        .into_iter()
        .flat_map(|schedule| {
            app.system_infos(schedule).into_iter().map(move |info| {
                format!(
                    "{schedule:?}|{}|{}|before={:?}|after={:?}",
                    info.descriptor.id(),
                    info.is_enabled,
                    info.descriptor.before(),
                    info.descriptor.after()
                )
            })
        })
        .collect()
    }

    #[test]
    fn repeated_host_registration_produces_identical_schedule_fingerprints() {
        let mut editor_play = App::new();
        register_runtime_systems(&mut editor_play)
            .expect("Editor Play runtime systems must register");
        let mut player = App::new();
        register_runtime_systems(&mut player).expect("Player runtime systems must register");

        assert_eq!(fingerprint(&editor_play), fingerprint(&player));
        let update = player.system_infos(ProjectSystemSchedule::Update);
        assert!(update
            .iter()
            .any(|info| info.descriptor.id().as_str() == "engine.authored_audio"));
        let secondary_motion_presentation = update
            .iter()
            .find(|info| info.descriptor.id().as_str() == "engine.secondary_motion_presentation")
            .expect("secondary-motion presentation system must be registered");
        assert!(
            secondary_motion_presentation
                .descriptor
                .before()
                .iter()
                .any(|id| id.as_str() == "engine.transform_propagation"),
            "secondary-motion presentation must update local joints before world propagation"
        );
        let fixed = player.system_infos(ProjectSystemSchedule::FixedUpdate);
        let fixed_ids: Vec<_> = fixed
            .iter()
            .map(|info| info.descriptor.id().as_str())
            .collect();
        for required in [
            "engine.rig_pose_clear_transient",
            "engine.animation_graph",
            "engine.animation",
            "engine.root_motion_motor",
            "engine.foot_ik",
            "engine.velocity_integration",
            "engine.navigation_agent",
            "engine.player_character_motor",
            "engine.character_controller",
            "engine.fixed_transform_propagation",
            "engine.secondary_motion",
            "engine.rig_pose_publish_final",
            "engine.secondary_motion_transform_propagation",
            "engine.collision_detection",
            "engine.combat_contacts",
            "engine.knockback",
            "engine.event_timeline",
        ] {
            assert!(fixed_ids.contains(&required), "missing `{required}`");
        }
        let descriptor = |id: &str| {
            fixed
                .iter()
                .find(|info| info.descriptor.id().as_str() == id)
                .map(|info| &info.descriptor)
                .unwrap()
        };
        assert!(descriptor("engine.animation")
            .after()
            .iter()
            .any(|id| id.as_str() == "engine.animation_graph"));
        assert!(descriptor("engine.root_motion_motor")
            .after()
            .iter()
            .any(|id| id.as_str() == "engine.animation"));
        assert!(descriptor("engine.foot_ik")
            .after()
            .iter()
            .any(|id| id.as_str() == "engine.fixed_transform_propagation"));
        assert!(descriptor("engine.character_controller")
            .after()
            .iter()
            .any(|id| id.as_str() == "engine.root_motion_motor"));
        assert!(descriptor("engine.collision_detection")
            .after()
            .iter()
            .any(|id| id.as_str() == "engine.secondary_motion_transform_propagation"));
        assert!(descriptor("engine.secondary_motion")
            .after()
            .iter()
            .any(|id| id.as_str() == "engine.fixed_transform_propagation"));
        assert!(descriptor("engine.secondary_motion_transform_propagation")
            .after()
            .iter()
            .any(|id| id.as_str() == "engine.rig_pose_publish_final"));
        assert!(descriptor("engine.rig_pose_publish_final")
            .after()
            .iter()
            .any(|id| id.as_str() == "engine.secondary_motion"));
        assert!(descriptor("engine.combat_contacts")
            .after()
            .iter()
            .any(|id| id.as_str() == "engine.collision_detection"));
        assert!(descriptor("engine.knockback")
            .after()
            .iter()
            .any(|id| id.as_str() == "engine.combat_contacts"));
        assert!(descriptor("engine.event_timeline")
            .after()
            .iter()
            .any(|id| id.as_str() == "engine.animation"));
        assert!(descriptor("engine.event_timeline")
            .after()
            .iter()
            .any(|id| id.as_str() == "engine.knockback"));
        assert!(editor_play
            .world()
            .get_resource::<Assets<AnimationClip>>()
            .is_some());
        assert!(editor_play
            .world()
            .get_resource::<AnimationEvents>()
            .is_some());
        assert!(editor_play
            .world()
            .get_resource::<CollisionEvents>()
            .is_some());
        assert!(editor_play.world().get_resource::<HitResults>().is_some());
        assert!(editor_play
            .world()
            .get_resource::<RuntimeEventTimeline>()
            .is_some());

        // A complete host profile must remain safe for empty or partial
        // scenes; missing matching components are normal, not startup errors.
        editor_play.ecs_mut().run_fixed_update().unwrap();
        editor_play.ecs_mut().update().unwrap();
    }

    #[test]
    fn identical_virtual_recording_resolves_equally_in_editor_and_player_hosts() {
        let settings = ProjectSettings {
            input_actions: vec![InputAction {
                name: "move".to_owned(),
                keys: Vec::new(),
                mouse_buttons: Vec::new(),
                gamepad_buttons: Vec::new(),
                gamepad_axes: vec![AxisBinding {
                    axis: 0,
                    deadzone: 0.2,
                    scale: 0.75,
                    invert: true,
                }],
                key_axes: vec![engine_authoring::KeyAxisBinding {
                    vector_component: 1,
                    negative_keys: vec!["KeyS".to_owned()],
                    positive_keys: vec!["KeyW".to_owned()],
                    scale: 1.0,
                }],
            }],
            ..ProjectSettings::default()
        };
        let (map, diagnostics) = InputActionMap::from_project_settings(&settings);
        assert!(diagnostics.is_empty());
        let mut editor = App::new();
        let mut player = App::new();
        editor.insert_resource(map.clone());
        player.insert_resource(map);

        let recording = [
            InputCommand::GamepadConnected {
                gamepad: GamepadId(2),
            },
            InputCommand::GamepadAxis {
                gamepad: GamepadId(2),
                axis: GamepadAxis::LeftStickX,
                value: 0.8,
            },
            InputCommand::Key {
                key: KeyCode::KeyW,
                pressed: true,
            },
        ];
        for app in [&mut editor, &mut player] {
            clear_input_transitions(app.world_mut());
            let queue = app
                .world_mut()
                .get_resource_mut::<VirtualInputQueue>()
                .expect("host input queue");
            for command in recording {
                queue.push(InputSource::Replay, command);
            }
            drain_virtual_input(app.world_mut());
        }

        let editor_state = editor
            .world()
            .get_resource::<InputActionMap>()
            .expect("editor action map")
            .resolve_action("move", editor.world());
        let player_state = player
            .world()
            .get_resource::<InputActionMap>()
            .expect("player action map")
            .resolve_action("move", player.world());
        assert_eq!(editor_state, player_state);
        assert_eq!(editor_state.vector, [-0.6, 1.0]);
        assert!(editor_state.just_pressed);
    }
}
