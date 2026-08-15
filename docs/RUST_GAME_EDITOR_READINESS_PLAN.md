# Rust Game Editor Readiness Plan

Status: Proposed for review
Date: 2026-07-13
Scope: Single-player 3D action RPG production with project-local Rust code

Related production-workflow plan:
`docs/EDITOR_PRODUCTION_WORKFLOW_PLAN.md`

## 0. 日本語要約

この計画の完成条件は、エンジン内部に機能が存在することではなく、通常の
プロジェクトをエディタで開き、プロジェクト固有 Rust だけでゲームを実装し、
Editor Play で確認し、同じ挙動のパッケージを作れることです。

今回の対象外は次の2点です。

- Rhai を使ったゲーム実装と、Rhai 向けの新規機能追加
- ネットワーク／ローカルマルチプレイ

最初に行うのは新しい戦闘機能の追加ではありません。欠落している実プロジェクトを
復元し、プロジェクト Rust から Input・Time・Transform・Collision・Animation・
Audio・UI・Scene・Save 等へ安全にアクセスできる GameModule API を設計し、
Editor Play と player のランタイム登録を一本化します。この土台が完成した後に、
glTF、アニメーション、衝突、AI、UI、オーディオ、制作支援、パッケージングを
順番に接続します。

各フェーズは、ユニットテストだけでは完了になりません。エディタでの作成・保存・
再起動・Play、パッケージ実行、異常系診断、手動の見た目／入力確認、性能計測まで
通過して初めて完了とします。

## 1. Purpose

This document is the delivery plan for turning the current engine into an
editor that can comfortably build, test, and package a small but complete 3D
action RPG through its normal project workflow.

It is a plan, not a replacement for the canonical contracts in:

- `docs/AI_FRIENDLY_AUTHORING_SPEC.md`
- `docs/RUST_CODE_STYLE.md`
- accepted records under `docs/adr/`

Any implementation decision that changes crate boundaries, a serialized
format, stable identifiers, the GameModule ABI, or command semantics MUST be
accepted in an ADR before implementation depends on it.

The plan responds to the current audit finding that many runtime subsystems
exist in isolation, but their normal authoring-to-Editor-Play-to-package path
is incomplete. A subsystem is not considered complete merely because its Rust
types and unit tests exist.

## 2. Product target and explicit non-goals

### 2.1 Editor Ready v1 target

The target is an editor-openable project in which a developer can perform the
following without modifying the engine workspace:

1. Create or open a project.
2. Import a glTF/GLB character and environment.
3. Create project-local Rust components and systems.
4. Read approved engine state and issue approved engine commands from Rust.
5. Author scenes, prefabs, collision, animation, AI, camera, audio, and UI.
6. Configure keyboard and gamepad actions.
7. Press Play and observe the same gameplay systems used by the packaged game.
8. Diagnose build, authoring, runtime, and asset errors in the editor.
9. Build a package and complete the game on a clean desktop machine.

The proving game is a small arena action RPG with a title/briefing flow, one
player, two AI allies, multiple enemies, melee combat, lock-on camera, HUD,
pause, audio, animation, particles, scene transitions, and saved progress.

### 2.2 Rust-first gameplay rule

Project gameplay for this milestone MUST be implementable with project-local
Rust code in `game/`.

- Rhai is not used by the proving game and receives no new milestone work.
- Existing Rhai code does not need to be removed if it remains compatible.
- No required gameplay, UI event, AI, save, or scene-flow path may depend on
  Rhai.
- Runtime behavior may be built into the engine when it is reusable engine
  behavior, but game-specific behavior MUST live in the project GameModule or
  authoring data.

### 2.3 Explicit non-goals

The following are excluded from Editor Ready v1:

- Network multiplayer, rollback, replication, matchmaking, and lobbies.
- Local multiplayer.
- Console certification and console platform ports.
- A complete terrain/world-streaming solution for large open worlds.
- A general visual scripting language.
- Full live GameModule state migration while Play is running.
- IK authoring, cinematic timeline editing, and advanced cutscene tools.
- A full external-DCC replacement.
- A complete general-purpose physics engine.

These exclusions MUST NOT be used to omit the single-player runtime,
authoring, debugging, or packaging paths listed in this plan.

## 3. Current release blockers

The following are confirmed blockers, not optional polish:

### B-01: The accepted vertical slice project is missing

- `docs/phases/phase-62-busters-lite.md` names `examples/busters_lite/` as the
  authoring source of truth.
- That directory is absent.
- `cargo test -p engine --example busters_lite` fails because the project
  cannot be opened.
- `cargo test --workspace` does not execute this example test and therefore
  gives a false sense of coverage.

### B-02: Project Rust cannot implement normal gameplay

The current `GameWorld` exposes project component snapshots only. Game systems
cannot safely read or command Transform, Time, Input, collision, animation,
audio, scene flow, save data, navigation, or structural ECS operations.

### B-03: GameModule dispatch does not scale

Each GameModule system serializes and deserializes a broad JSON world snapshot.
The cost grows with the total number of project entities and the number of
systems, even when a system needs only one component type.

### B-04: Runtime hosts do not register the complete runtime

Editor Play and the packaged player do not install the same complete system
set. Collision, physics, character control, navigation, animation
graph evaluation, and related resources are not consistently reachable.

### B-05: Authoring coverage is incomplete

Important runtime types have no authorable component and no normal Inspector
path, including Animator, Animation Graph player, Behavior Tree runner,
NavMesh agent, and audio emitters/listeners.

### B-06: Project input settings are not the runtime source of truth

Keyboard/gamepad settings can be edited and persisted, but normal gameplay and
the current script snapshot do not consistently consume the configured action
map. Analog axes are not connected to action-RPG movement.

### B-07: Character asset import is not end-to-end

The editor mesh picker and normal runtime mesh loading are effectively OBJ
oriented. The glTF importer is not connected to the normal import, manifest,
reimport, scene placement, skin, clip, material, and package workflow.

### B-08: Movement, character collision, and combat collision are disconnected

The current PlayerController writes Transform directly, while the kinematic
controller owns a separate velocity path. Character-to-character collision,
collision transitions, continuous checks for fast attacks, and broad-phase
scaling are missing.

### B-09: Existing M1 acceptance is unverified

The manual checklist has no recorded completed run. The nested GameModule E2E
test is ignored by default. No clean-machine package completion has been
recorded for the intended vertical slice.

## 4. Required feature inventory

This is the minimum feature inventory for Editor Ready v1. “Partial” includes
features that have runtime code but no complete normal-project path.

| Area | Current audit | Editor Ready v1 requirement | Delivery |
| --- | --- | --- | --- |
| Project creation/open | Partial | Unicode-safe project create/open/reopen with truthful defaults and SDK status | ER-0, ER-10 |
| Project Rust build | Partial | Check/Build/Release, diagnostics, cancellation, newest-module tracking | ER-1, ER-10, ER-11 |
| Rust gameplay access | Blocked | Query approved engine state and issue deferred gameplay commands | ER-1 |
| Rust game resources | Missing | Schema-described mission/global state owned safely by the host | ER-1 |
| Runtime scheduling | Partial | One truthful Editor/player catalog with constraints and profiling | ER-2 |
| Input actions | Partial | Keyboard, mouse, gamepad buttons/axes, rebinding, focus, virtual input | ER-3 |
| Character movement | Partial | Camera-relative analog motor integrated with collision | ER-3, ER-7 |
| Scene hierarchy/editing | Partial | Multi-edit, parenting, search, snapping, safe delete, undo/redo | ER-10 |
| Prefabs | Partial | Create/place/override/revert/unpack/spawn/dependency handling | ER-9 |
| Texture/material import | Partial | Register, preview, edit, reimport, diagnose, package | ER-4A |
| glTF/GLB | Runtime-only/partial | Complete source-to-sub-assets-to-scene-to-package path | ER-5 |
| Rendering settings | Partial | Authorable lighting/environment/shadow/postprocess parity | ER-4A |
| Skinned animation | Runtime-only/partial | Clip/graph assignment, preview, events, root-motion policy | ER-4, ER-5, ER-6 |
| Collision/physics | Partial | Registered pipeline, stable motor, broad phase, transitions, sweeps | ER-2, ER-7 |
| Combat contacts | Missing | Hitbox lifetime, one-hit policy, teams, knockback, invulnerability | ER-7 |
| Navigation | Runtime-only | Bake, visualize, assign agents, query/command from Rust | ER-4, ER-8 |
| Behavior Trees | Partial | Attach runner, register Rust nodes, blackboard, live debug | ER-4, ER-8 |
| Targeting/camera | Partial | Rust control, camera-relative movement, obstruction/debug parity | ER-1, ER-3, ER-7 |
| Particles/LOD | Runtime-only/partial | Author, preview, debug, persist, package | ER-4A |
| UI | Partial | Tree/Inspector/preview, Rust bindings/events, gamepad navigation | ER-1, ER-9 |
| Audio | Runtime-only/partial | Author emitters/listener, positional SE, mixer, Rust commands | ER-1, ER-9 |
| Scene flow | Partial | Rust requests, completion/failure, persistence, loading hook | ER-1, ER-9 |
| Save/load | Partial | Versioning, migration, atomicity, recovery, user-writable location | ER-1, ER-9, ER-11 |
| Runtime inspection | Partial | Pause/step/runtime hierarchy/system and gameplay profiling | ER-10 |
| Diagnostics | Partial | Source/asset/entity-linked stable errors for every failed path | All phases |
| Packaging | Partial | One operation, release module, complete dependencies, clean machine | ER-11 |
| Proving game | Blocked | Real editor project using every standard path and project Rust | ER-12 |
| Automated/manual QA | Partial | All-targets, nested module, parity, visual/device/package/soak records | All phases |

Items not listed as non-goals in Section 2.3 may not be silently removed from
this inventory. A plan revision must explain and review any scope reduction.

## 5. Completion policy

### 5.1 Definition of Ready for an implementation phase

A phase may start only when:

- Its user workflow and failure workflow are written down.
- Affected crates and persisted files are listed.
- Required ADRs are accepted.
- Backward compatibility and migration needs are identified.
- Automated tests and manual visual/device checks are specified first.
- The previous dependency phase has passed its exit gate.

### 5.2 Definition of Done for an engine feature

A feature is complete only if all applicable statements are true:

- It is authorable from an editor-openable project.
- The authoring change uses the shared command/transaction boundary.
- It survives save, editor restart, and project reopen.
- It converts through the normal authoring-to-runtime bridge.
- It works in Editor Play.
- It works in the generic packaged player with equivalent semantics.
- Project-local Rust can observe or command it where gameplay needs it.
- Invalid values produce actionable diagnostics without crashing.
- Undo/redo and dirty-state behavior are defined for editor mutations.
- Package analysis includes all referenced assets.
- Unit, integration, regression, and relevant visual/device tests pass.
- Public APIs and non-obvious implementation constraints are documented.

Standalone examples are useful secondary tests, but never satisfy this
definition on their own.

### 5.3 Required quality gates

Every phase that changes Rust MUST pass:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

Editor Ready milestones additionally MUST run:

```text
cargo test --workspace --all-targets
cargo test -p engine-editor -- --ignored --test-threads=1
cargo test -p engine --example busters_lite
cargo check -p engine --bin player
```

If an ignored test requires a toolchain, GPU, audio device, or interactive
desktop, the release record MUST state the machine, command, result, and reason
it cannot be a normal CI test. “Ignored” is not equivalent to “passed.”

### 5.4 Parity rule

Editor Play and the packaged player MUST share:

- the same runtime-system registration function;
- the same system descriptors and dependency constraints;
- the same scene conversion path;
- the same GameModule component and system dispatch path;
- the same input-action resolution semantics;
- the same asset resolution and missing-asset policy, except where packaging
  deliberately promotes a warning to a blocking error.

Host differences MUST be explicit data in one profile description and covered
by a parity test. Host-local copies of registration lists are prohibited.

## 6. Architecture work that must happen first

## ER-0: Restore a truthful baseline

Goal: make repository status and tests accurately describe what can run.

Required work:

1. Restore `examples/busters_lite/` or deliberately replace it with a renamed
   editor-openable proving project.
2. Ensure it contains `project.json`, `project_settings.json`,
   `asset_manifest.json`, scenes, prefabs, UI, audio, and game code as needed.
3. Remove or correct “implemented” claims that are not reachable through the
   normal project workflow.
4. Add the busters example test to an all-targets release gate.
5. Record the first manual baseline run, including expected failures.
6. Add a machine-readable feature-reachability inventory with these states:
   `runtime_only`, `authorable`, `editor_play`, `packaged`, `verified`.

Exit gate:

- The proving project opens in the editor.
- Its start scene exists and package analysis can traverse it.
- Targeted tests fail only for explicitly recorded missing features.
- Documentation no longer reports an absent fixture as complete.

## ER-1: Project-local Rust gameplay API

Goal: make `game/` capable of implementing the proving game without modifying
the engine workspace.

An ADR extending or superseding ADR 0050 MUST be accepted before coding.

### Required Rust-visible data

Project systems need safe, bounded access to:

- frame time and fixed time;
- configured input actions, including pressed/just-pressed/just-released and
  scalar/vector axes;
- stable runtime entity handles and authoring identities where available;
- project components declared by GameModule macros;
- approved read views of Transform/GlobalTransform;
- character-controller grounded and velocity state;
- collision enter/stay/exit records;
- animation state and animation events;
- lock-on state;
- navigation path/agent status;
- UI events and current binding values;
- scene-flow state;
- save values explicitly exposed to gameplay;
- game-owned global runtime resources.

### Required deferred commands

Project systems need host-validated commands for:

- set/translate/rotate Transform;
- set character desired velocity and facing;
- spawn a prefab and receive a later spawn result event;
- despawn an entity;
- add/remove or enable/disable an approved component where safe;
- play/crossfade/stop animation;
- set Animation Graph parameters;
- create/enable/disable attack hitboxes;
- play SE, play/crossfade/stop BGM, and set mixer volumes;
- acquire/cycle/release lock-on;
- set/remove UI bindings and receive button events;
- request a scene transition;
- read/write/load/save a versioned save slot;
- set/cancel/query gameplay timers;
- emit targeted or broadcast game events.

### Query and performance requirements

- A system MUST declare the data it reads and writes.
- The host MUST build query-specific views, not serialize the entire project
  world once per system.
- The ABI MUST not pass Rust references, trait objects, `String`, or other
  layout-dependent Rust values.
- Fixed-layout ABI records or bounded byte buffers may be used; any serialized
  payload MUST be scoped to the declared query/command, not the whole world.
- Multiple project component types must be queryable together.
- Read-only queries must not be encoded and written back.
- No system may silently retain a runtime entity past generation validation.
- Command queues must have documented caps and overflow diagnostics.
- GameModule system timing and bytes transferred must be visible in the
  profiler.

### Game-owned runtime resources

The GameModule needs host-owned, schema-described resources for state that is
not naturally attached to an entity, such as mission phase, score, selected
loadout, and pause state.

- Resource IDs must be stable and namespaced.
- Resource data must not be persisted into scenes by accident.
- Save conversion must be explicit.
- Module reload policy must be defined; Editor Ready v1 may require stopping
  Play before adopting a new module generation.

Exit gate:

- A project Rust system reads configured input and fixed delta, queries its
  game component plus Transform, moves through a deferred command, spawns and
  despawns a prefab, receives a collision event, plays an animation and SE,
  updates UI, requests a scene, and writes a save slot in integration tests.
- The same project module passes in Editor Play and packaged player tests.
- A benchmark with at least 100 relevant entities and 10 project systems does
  not serialize the entire game world 10 times per frame.

## ER-2: One complete runtime host profile

Goal: make every authorable runtime component actually execute.

Required work:

1. Define one shared runtime registration catalog.
2. Register required resources before systems.
3. Register, order, and identify at least:
   - transform propagation;
   - animation graph evaluation;
   - animation sampling and events;
   - gameplay Rust systems;
   - gravity and velocity integration;
   - kinematic character control;
   - collision detection and event transitions;
   - restitution where enabled;
   - navigation agents;
   - Behavior Tree runners;
   - lock-on selection and camera;
   - particles;
   - audio command application;
   - UI event relay;
   - scene-switch command processing;
   - prefab spawn/despawn command processing;
   - rendering preparation.
4. Express producer/consumer dependencies with stable system constraints.
5. Make systems harmless when no matching components exist.
6. Make missing required resources a startup diagnostic, not a mid-frame
   surprise.
7. Show the truthful final schedule in the Systems panel.

Exit gate:

- A generated fingerprint of registered system IDs, schedules, enabled state,
  and constraints matches between Editor Play and player.
- An authorable component test proves each component's runtime system ran, not
  merely that scene conversion added the Rust component.

## ER-3: Input Actions and action-RPG controller

Goal: make Project Settings the only binding source used by normal gameplay.

Required work:

- Compile `ProjectSettings.input_actions` into a runtime resource at Play and
  player startup.
- Support keyboard keys, mouse buttons, gamepad buttons, and gamepad axes.
- Support deadzone, scale, inversion, and positive/negative axis composition.
- Expose button transitions and scalar/vector actions to project Rust.
- Route virtual input through the same action resolver.
- Correctly release held inputs on Game View focus loss and device disconnect.
- Add controller reconnect handling and device diagnostics.
- Provide camera-relative planar movement, analog magnitude, facing policy,
  acceleration/deceleration, and optional sprint/dodge requests.
- Integrate desired movement with the kinematic controller rather than writing
  Transform through an independent path.
- Add an input debugger showing physical input and resolved actions.

Exit gate:

- Rebinding movement from WASD to arrow keys changes gameplay without code.
- Keyboard and one gamepad can complete the proving game.
- Editor Play and package resolve identical action values from identical
  virtual-input recordings.

## 7. End-to-end authoring and asset work

## ER-4: Complete authorable component coverage

Goal: expose every required reusable runtime feature through the component
registry and Inspector.

Required built-in authorable components:

- Animator and animation clip bindings;
- Animation Graph player and parameter defaults;
- Behavior Tree runner and blackboard defaults;
- NavMesh agent;
- audio emitter, audio listener, and optional music controller;
- collision event receiver or Rust event subscription metadata where needed;
- camera-relative character motor settings;
- lock-on source and target settings;
- persistent runtime identity/name/tag/team data used by Rust queries;
- existing mesh, material, particle, UI, collider, body, camera, and light
  components with complete validation.

Inspector requirements:

- Asset fields use filtered asset pickers, not free-form IDs.
- Entity references use scene entity pickers.
- Enum fields use enum controls.
- Layer masks show project layer names.
- Conditional fields hide or disable irrelevant values.
- Validation appears beside the field and in Problems.
- Adding/removing/editing components is undoable and transaction-backed.
- Project GameModule components remain editable after module rebuild and show
  a non-destructive missing-definition state when the build is broken.

Exit gate:

- Every listed component can be added, edited, saved, reopened, converted,
  executed in Play, and packaged in a parameterized integration suite.

## ER-4A: Rendering, material, and scene presentation reachability

Goal: verify that existing rendering features are production paths rather than
runtime-only demonstrations.

Required work:

- Register and reimport common texture formats through the normal asset
  workflow.
- Expose material texture slots, scalar/color properties, transparency mode,
  culling, and supported shading options through the Material editor.
- Provide material and texture preview with missing/invalid asset diagnostics.
- Make directional/ambient lighting, environment lighting, shadows, and
  post-processing editable with an immediate Scene View preview.
- Define which settings are scene-owned and which are project-owned.
- Make skinned meshes, static meshes, GPU instancing, LOD groups, and particle
  emitters authorable and package-reachable.
- Add authorable LOD thresholds and Scene View LOD debug display.
- Add particle preview, restart, bounds, and emission debug controls.
- Verify camera aspect, exposure, tone mapping, bloom, shadow cascades, and
  environment lighting in both Editor Play and package.
- Provide a visible fallback material/mesh/texture for non-blocking missing
  assets; do not silently substitute an unrelated triangle.
- Record renderer limits and emit diagnostics before exceeding texture, joint,
  light, instance, or particle limits.

Exit gate:

- One environment and one skinned character retain matching materials,
  lighting, shadows, particles, LOD behavior, and post-processing in Scene
  View, Editor Play, and the packaged player.
- Reopening and reimporting the project does not lose material or sub-asset
  references.

## ER-5: glTF/GLB import and reimport pipeline

Goal: import real character and environment assets without custom engine code.

Required work:

- Register `.gltf` and `.glb` through the normal asset workflow.
- Resolve GLB data and glTF external buffers/images relative to the source.
- Generate stable sub-asset IDs for meshes, materials, textures, skeletons,
  skins, and animation clips.
- Persist import settings and source fingerprints.
- Import normals, tangents, UVs, indices, joints, weights, inverse bind poses,
  animation channels, and supported material textures.
- Define unsupported-feature diagnostics for morph targets, compression,
  interpolation modes, or extensions not yet handled.
- Connect imported sub-assets to manifest pickers and scene drag/drop.
- Implement reimport without silently changing stable references.
- Run import as a cancellable background job with progress and Problems output.
- Include all derived assets and source dependencies in package analysis.
- Stop advertising FBX as directly usable unless an actual import path exists;
  document conversion to glTF if FBX remains unsupported.

Exit gate:

- A GLB character with skin, at least three clips, materials, and textures can
  be imported, placed, animated in Scene Preview, played, reimported, reopened,
  and packaged without hand-editing JSON.

## ER-6: Animation production workflow

Goal: make animation usable for responsive combat rather than code-only demos.

Required work:

- Clip picker and preview controls.
- Animation Graph assignment and parameter editing.
- Runtime graph state/transition inspection during Play.
- Crossfade controls with deterministic interruption rules.
- Animation event editor with named events and timeline positions.
- Rust delivery of animation events in the same fixed-step contract.
- Explicit root-motion mode: disabled, extracted-only, or applied through the
  character motor.
- Playback speed, looping, one-shot, and completion events.
- Stable handling of missing/reimported clips.
- Preview pose reset and scene-edit/play separation.
- Optional per-state transition duration overrides.

Exit gate:

- Idle, move, three attacks, hit reaction, defeat, and revive animations are
  authored without engine code.
- Attack damage is enabled by an animation event and is not tied to wall-clock
  guesses in a game system.

## ER-7: Collision, character motor, and combat contacts

Goal: provide predictable action-game movement and hit detection.

Required work:

- Register the fixed-step collision/physics/controller pipeline by default.
- Unify player desired movement with the kinematic character controller.
- Add slope limits, step offset, ground snapping, skin width, ceiling handling,
  and stable corner resolution.
- Define character-to-character blocking or separation.
- Add broad-phase acceleration suitable for hundreds of colliders.
- Add collision/trigger `enter`, `stay`, and `exit` transitions.
- Add swept checks or shape casts for fast melee hitboxes and dashes.
- Prevent one attack activation from damaging the same target every fixed
  step unless explicitly configured.
- Provide reusable attack-hitbox activation, team/layer filtering, hit result,
  knockback request, and invulnerability-window primitives.
- Support static environment collision through an accepted mesh/compound
  collider strategy.
- Draw accurate sphere/capsule/box/mesh collider gizmos in Scene View.
- Show contact normals, grounded state, velocity, and active hitboxes in Play
  debug mode.

Exit gate:

- Player, allies, and enemies cannot walk through arena walls or one another
  under the selected separation policy.
- A three-hit combo produces exactly the expected hit events at 30, 60, and
  120 rendered FPS.
- A fast dash does not pass through the validation wall or target.

## ER-8: Navigation and Behavior Tree authoring

Goal: author and debug allies/enemies through normal project data and Rust
behaviors.

Required work:

- NavMesh settings in Project Settings or a scene-owned bake document.
- Bake button, cancellable bake job, save/load, stale-bake detection, and
  clickable diagnostics.
- Scene View NavMesh visualization and path preview.
- Authorable NavMeshAgent with target, speed, stopping distance, repath policy,
  and status.
- Project Rust query/command access for target setting and path status.
- Basic local avoidance/separation for the proving combatant count.
- Authorable BehaviorTreeRunner linking a compiled tree asset.
- Project Rust registration of BT actions/conditions through stable IDs.
- Typed blackboard keys or schema-described values.
- Runtime visualization of active/running/succeeded/failed nodes.
- Clear behavior when a node implementation, graph asset, or NavMesh is
  missing.

Exit gate:

- Two allies follow, select enemies, attack, and recover without direct
  straight-line Transform movement.
- Enemies navigate around a wall and expose their live BT node/path in the
  editor debugger.

## 8. Game-flow and presentation work

## ER-9: UI, audio, scene flow, save, and prefab completion

Goal: let project Rust drive the complete game loop through stable engine
services.

### UI

- UI document tree editor, Inspector, preview, anchors, layout, text, images,
  buttons, and visibility/enabled bindings.
- Asset and font pickers with CJK verification.
- Rust-side UI binding commands and typed button/event delivery.
- Focus/navigation support for keyboard and gamepad.
- Resolution/DPI/safe-area checks and aspect-ratio preview presets.
- Pause/menu input ownership so gameplay does not also react to UI input.

### Audio

- Authorable audio emitter/listener.
- 2D SE, positional SE, distance attenuation, BGM loop/crossfade, master/BGM/SE
  buses, and preview.
- Rust commands and completion/error diagnostics.
- Defined behavior when no audio device exists.

### Scene flow and save

- Rust scene-transition request and completion/failure event.
- Loading-screen hook and prevention of duplicate transition requests.
- Explicit persistent game resources across scene transitions.
- Versioned save schema, migration hook, atomic writes, slot metadata, and
  corrupt-save recovery diagnostics.
- Use an OS-appropriate user-data save location for distributed builds;
  portable saves beside the package may remain an explicit build option.

### Prefabs

- Create prefab from selection, place prefab, inspect source, apply/revert
  overrides, unpack, and detect broken references.
- Runtime prefab spawn/despawn through Rust commands.
- Stable nested-prefab policy and dependency traversal for packaging.

Exit gate:

- Title -> briefing -> arena -> result -> title uses actual scene transitions.
- Gamepad can navigate every required menu.
- Mission clear saves progress, restart reloads it, and a deliberately corrupt
  slot produces recovery UI rather than a crash.

## ER-10: Daily editor productivity and debugging

Goal: remove friction that makes repeated game iteration unsafe or slow.

### Scene and hierarchy workflow

- Multi-selection, box selection, duplicate, delete with child policy, reparent,
  drag/drop parenting, search, isolate/hide/lock, and frame selected.
- World/local transform modes, snapping, numeric entry, pivot mode, and undo
  coalescing.
- Create common primitives, cameras, lights, empty entities, and prefab
  instances from menus.
- Scene tabs or a clear save/discard flow for changing documents.
- Accurate dirty state for every editor-owned document.
- Copy/paste components and multi-edit shared fields with one undo transaction.
- Autosave/recovery files that never overwrite the last explicit save and can
  be reviewed or discarded after an editor crash.

### Asset workflow

- Import queue with progress/cancel/retry.
- Rename/move/delete with reference analysis and safe fix-up transactions.
- Dependency and reverse-dependency view.
- Rename/move/delete must either update every supported reference atomically or
  refuse the operation with a dependency list.
- Thumbnails for meshes, materials, textures, prefabs, and UI where practical.
- Consistent register/reimport/reveal/open actions.

### Rust workflow

- Create component/system/resource templates with stable IDs and useful
  comments.
- Incremental Check/Build status, cancellation, source-linked diagnostics, and
  one-click rebuild before Play.
- Play must use the newest successful module and clearly warn when source is
  newer than the loaded module.
- Stop Play before module generation replacement; never silently run mixed
  generations.
- Pin or report the engine SDK version used to build the project module and
  provide a clear migration diagnostic when it changes.

### Runtime debugging

- Pause, resume, single-frame step, and fixed-step count display.
- Runtime hierarchy and read-only component inspection.
- System profiler, GameModule transfer metrics, draw stats, collision stats,
  script-free game event log, and memory/asset counts.
- Clickable Console/Problems entries with stable diagnostic codes.
- Capture frame and deterministic virtual-input recording/replay.
- Debug overlays for colliders, NavMesh, paths, lock-on, camera obstruction,
  animation state, and hitboxes.

Exit gate:

- A developer can change Rust gameplay, rebuild, enter Play, reproduce a
  recorded input sequence, inspect the failing entity/system, stop, undo an
  authoring change, and retry without restarting the editor.

## ER-11: Packaging and distribution reliability

Goal: produce a self-contained desktop build from the same project.

Required work:

- Build the release GameModule automatically as part of Package, or provide a
  single explicit package operation that performs both steps.
- Reject stale or ABI-incompatible modules.
- Validate start scene, referenced assets, derived glTF assets, UI, audio,
  graphs, prefabs, and native library before copying.
- Produce deterministic package layout and a build report.
- Preserve licenses/notices required by dependencies and bundled assets.
- Resolve user-writable save/log/config locations.
- Capture startup failures in a readable log beside an OS-appropriate log
  directory.
- Test paths containing spaces and Japanese characters.
- Test a clean machine without the Rust toolchain or repository checkout.
- Define release/debug symbols and crash-report policy.

Exit gate:

- One Package action creates a build that starts and completes the proving game
  on a clean Windows machine.
- The packaged build does not depend on `target/`, workspace paths, Cargo, or
  the editor installation.

## 9. Final proving game and release acceptance

## ER-12: Replace the fake vertical slice with a real project

Goal: prove the editor, not a standalone engine example.

The final project MUST:

- be the authoring source of truth under `examples/`;
- open through the normal Project Hub;
- use a project-local `game/` Rust module;
- avoid game-specific changes in `crates/engine` and `crates/editor`;
- use the normal imported asset, scene, prefab, animation, collision, AI, UI,
  audio, save, and package paths;
- not depend on Rhai;
- not require network functionality;
- use the same runtime code in Editor Play and package;
- include enough source assets or redistributable placeholders to run from a
  fresh checkout.

Minimum playable content:

1. Title scene and gamepad/keyboard menu.
2. Mission briefing scene or UI flow.
3. Arena with static collision and a baked NavMesh.
4. One animated player with camera-relative movement, lock-on, dodge, and a
   three-step combo.
5. Two animated AI allies.
6. At least three enemies, including one stronger captain.
7. Animation-event-driven hitboxes, damage, hit reaction, defeat, particles,
   and SE.
8. Lock-on camera with obstruction handling.
9. HUD, pause, result UI, BGM, and mixer settings.
10. Scene transition and versioned save of clear count and best/last time.
11. Package completion from a clean output directory.

The standalone `crates/engine/examples/busters_lite.rs` may remain only as a
thin secondary launcher using the same project and runtime path. It MUST NOT
contain a separate game implementation.

## 10. Verification matrix

| Area | Unit | Integration | Editor manual | Package manual | Performance |
| --- | --- | --- | --- | --- | --- |
| GameModule Rust API | ABI validation, query/command codecs | generated project module | rebuild/Play/error links | same module loads | bytes and time per system |
| Runtime systems | order/constraints | component executes in host | Systems panel truth | parity fingerprint | system timings |
| Input | action resolver/deadzones | virtual recording | focus/rebind/device reconnect | keyboard + gamepad | event latency |
| Rendering | material/settings limits | scene-to-render conversion | Scene/Play visual parity | package visual parity | draw/GPU timings |
| glTF | parse/sub-asset IDs | import/reimport/package graph | preview and drag/drop | clean asset load | import time/memory |
| Animation | blend/events/root motion | graph + character | preview/live graph | same event sequence | animator time |
| Collision | shapes/transitions/sweep | motor + combat | gizmos/debug overlay | same contacts | broad-phase/contact time |
| AI | bake/path/BT | ally/enemy scenario | live path/node view | same decisions | path/BT time |
| UI | layout/events/navigation | Rust UI event loop | DPI/aspect/gamepad | all menus | UI frame time |
| Audio | buses/attenuation | Rust commands | preview/device failure | BGM/SE audible | active voice cost |
| Save/scene | schema/migration/atomicity | transition and reload | corrupt-slot recovery | writable user path | load/save time |
| Packaging | dependency plan | temp package | Package diagnostics | clean-machine completion | startup time |

## 11. Performance and stability budgets

ER-0 MUST record a reference machine. ER-1 establishes repeatable headless or
deterministic benchmarks before optimizing.

Initial acceptance targets for the proving game on the reference machine:

- 60 FPS target at 1920x1080 in a release package.
- No sustained gameplay-system frame above 2 ms for six combatants.
- A 100-combatant stress scene records p95 costs for GameModule dispatch,
  collision, navigation, animation, and rendering separately.
- No whole-project JSON snapshot per GameModule system.
- No unbounded command/event queue.
- No per-frame asset load or script/game-code compilation.
- No editor freeze longer than 100 ms for import/bake/build operations that can
  reasonably run as background work; longer work must show progress and remain
  cancellable.
- Play/Stop repeated 20 times must not leak loaded module generations, GPU
  resources, audio voices, or runtime entities.
- A 30-minute automated combat soak must not panic, deadlock, or grow memory
  without bound.

Budgets may be revised only with a recorded benchmark, explanation, and review.

## 12. Delivery order and dependency schedule

Implementation order is mandatory unless a reviewed plan update explains the
change.

```text
Wave 0: Truth and architecture
  ER-0 baseline
    -> ADR for GameModule Rust API
    -> ADR for shared runtime host/profile if ADR 0051 is insufficient

Wave 1: Make Rust gameplay possible
  ER-1 GameModule API
    -> ER-2 shared runtime host
    -> ER-3 input/controller

Wave 2: Make content authorable
  ER-4 component coverage
    -> ER-4A rendering/material reachability
    -> ER-5 glTF import
    -> ER-6 animation workflow

Wave 3: Make combat and AI real
  ER-7 collision/character/combat
    -> ER-8 navigation/BT

Wave 4: Complete the game loop
  ER-9 UI/audio/scene/save/prefab
    -> ER-10 editor productivity/debugging
    -> ER-11 packaging

Wave 5: Prove and harden
  ER-12 real vertical slice
    -> full automated gates
    -> manual matrix
    -> performance/soak
    -> Editor Ready v1 release decision
```

ER-4, ER-4A, and ER-5 may overlap only after ER-1 and ER-2 contracts are stable. UI
presentation work may overlap ER-7/ER-8, but Rust UI event delivery cannot be
declared complete before ER-1. ER-12 starts only after every prior exit gate
passes.

## 13. Review checkpoints

At the end of every ER phase, perform the following review before starting the
next dependent phase:

1. Review the complete diff, including generated files and serialized samples.
2. Compare implementation with this plan, canonical spec, relevant phase doc,
   and accepted ADRs.
3. Search for duplicate host paths and runtime-only shortcuts.
4. Confirm no stable ID or serialized format changed silently.
5. Run the phase's automated gates.
6. Run and record its manual editor/device checks.
7. Check Editor Play/package parity.
8. Inspect diagnostics for malformed assets and missing definitions.
9. Re-run the proving project from a fresh checkout or clean temp project when
   the phase affects scaffolding, assets, or packaging.
10. Update the reachability inventory and only then change phase status.

No phase may be marked implemented while its proving fixture is missing, its
required E2E test is ignored and unrun, or its manual acceptance fields are
blank.

## 14. First approved implementation slice

After this plan is reviewed, the first implementation proposal should contain
only ER-0 and the ADR draft for ER-1. It should not begin adding more gameplay
features yet.

The first slice should produce:

- a restored/renamed editor-openable busters project fixture;
- a failing but correctly wired all-targets vertical-slice test that documents
  current missing behavior;
- corrected roadmap/status documentation;
- a reachability inventory;
- a GameModule Rust API ADR covering query declarations, engine read views,
  deferred commands, game resources, ABI safety, performance, compatibility,
  and Editor Play/package parity.

Implementation of the new ABI begins only after that ADR and its test plan are
approved.
