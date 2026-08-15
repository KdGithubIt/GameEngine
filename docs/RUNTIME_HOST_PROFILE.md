# Runtime Host Profile

Editor Play and the packaged player both construct `engine::App`, then call
`register_runtime_systems`. A gameplay feature must not add a host-local copy
of this registration list. Project GameModule callbacks are appended only
after the shared engine profile and may be reordered through validated Project
Settings constraints.

## Fixed-step producers and consumers

The shared profile registers these stable systems in addition to the base
services installed by `App::new`:

| Stable ID | Responsibility | Required ordering |
| --- | --- | --- |
| `engine.animation_graph` | Select animation graph states | before animation |
| `engine.animation` | Sample clips and publish animation events | after animation graph |
| `engine.root_motion_motor` | Convert extracted local motion to a motor displacement | after animation, before character controller |
| `engine.velocity_integration` | Advance Rapier gameplay physics, including gravity and contact response, and publish dynamic transforms/velocities | before fixed transform propagation |
| `engine.navigation_agent` | Move agents along host paths | before fixed transform propagation |
| `engine.character_controller` | Integrate kinematic characters and consume root motion | after player/root-motion intent, before fixed transform propagation |
| `engine.fixed_transform_propagation` | Refresh world matrices after movement | after all fixed movement and animation |
| `engine.collision_detection` | Detect current-step contacts and resolve overlap | after fixed transform propagation |

`CollisionEvents`, `AnimationEvents`, animation assets, and the Behavior Tree
registry are inserted before registration. Every system treats an empty query
or an absent optional NavMesh as normal, so a partial scene can start without a
mid-frame missing-resource failure.

## Frame and boundary work

The frame schedule contains Behavior Tree dispatch, configured player input,
lock-on selection/cameras, orbit/follow cameras, particles, UI relay, deferred
audio/save effects, and render-facing transform propagation. Scene changes and
prefab spawns require exclusive world access and are processed by
`App::process_scene_requests` at the host frame boundary rather than from an
ECS callback. Rendering preparation follows schedule execution in the common
`App` runner.

The parity regression test fingerprints stable IDs, enabled state, and
constraints for two independently constructed hosts. It also executes both
schedules on an empty world to prove that the complete profile is harmless
when no matching authorable components exist.
