# ADR 0138: Play-Mode Graph Debug Shell and Domain Providers

Status: Accepted
Date: 2026-08-17
Amends: ADR 0123
Relates to: ADR 0006, ADR 0011, ADR 0033, ADR 0082, ADR 0084, ADR 0085, ADR 0116, ADR 0136, ADR 0137, ADR 0139

## Problem

Play mode currently gives Behavior Tree a fixed top-level tab beside Game View and Scene View, even when no Behavior Tree runner exists. Animation Graph execution has no equivalent source-graph runtime view. Copying the Behavior Tree UI for every graph domain would create permanent per-domain tabs, duplicate Graph Canvas behavior, and make Behavior Tree semantics an accidental universal model.

ADR 0006 deliberately separates domain-neutral graph identity/storage/presentation foundations from domain-specific semantics. The current Behavior Tree debugger already reuses shared Graph Canvas rendering while its live meanings—active path, Running, Success, Failure, aborts, blackboard—remain Behavior Tree concepts. Animation Graph needs different semantics: State, transition, fade progress, parameters, Motion Slot resolution, clip time, and events.

Runtime source mapping must also remain honest when the Editor working copy differs from the source generation used by the running instance. Debug UI must detect staleness rather than silently read another copy from disk or overlay runtime state on the wrong source.

## Decision

### 1. Play mode exposes one domain-neutral Graph Debug entry

The normal primary views are conceptually:

```text
Game View | Scene View | Graph Debug
```

`Graph Debug` is one shell for all runtime graph domains. Behavior Tree is no longer a permanently privileged top-level tab.

The Graph Debug entry is hidden when the current runtime world contains no debuggable graph execution target. This is preferred over a permanently disabled tab because Graph Debug represents live runtime state, not a general authoring editor. If the selected target disappears while Graph Debug is already open, the shell may remain long enough to show a concise target-lost state and allow another selection; it MUST NOT present the last snapshot as live.

### 2. The shell enumerates actual runtime graph execution targets

Providers enumerate only graph instances that exist in the current Play runtime world. Example:

```text
Player
  Animation Graph: PlayerGraph

Enemy01
  Behavior Tree: EnemyAI

Enemy02
  Behavior Tree: EnemyAI
```

A Graph asset merely existing in the project is not enough to appear. A target is listed only when a runtime instance/provider can observe execution for an Entity.

Runtime ECS identity may participate in session-local lookup but MUST NOT be persisted into project data. Stable authoring Entity/Graph identity is used for source/navigation correlation.

### 3. The Graph Debug shell owns graph-neutral interaction and presentation

The shell owns cross-domain behavior:

- runtime target list and selection;
- source Graph identity resolution;
- shared Graph Canvas presentation;
- source `NodeId` and `EdgeId` selection;
- Frame All and source-target framing;
- generic active/highlight overlay primitives;
- runtime Entity selection/navigation;
- details-pane hosting;
- stale-source detection and explanation;
- read-only debug presentation; and
- lifecycle when Play starts/stops or a target disappears.

Graph Canvas remains shared. Graph Debug MUST NOT fork an Animation-only or Behavior-Tree-only canvas implementation.

The shell does not own Behavior Tree or Animation runtime semantics.

### 4. Runtime semantics are supplied by domain-specific debug providers

Each runtime graph domain integrates through an Editor-side provider/adapter. Conceptually:

```text
GraphDebugProvider
  domain identity + display label
  enumerate live runtime targets
  resolve source Graph identity
  obtain domain runtime snapshot
  map runtime source identity -> NodeId / EdgeId
  produce common highlight/focus primitives where meaningful
  provide domain-specific details/overlay presentation
  report runtime/source generation and stale-source state
```

The exact Rust trait is implementation detail; the responsibility split is normative. Runtime domains expose GUI-free read-only observation DTOs. They do not depend on egui, Editor widgets, Graph Canvas, or window state.

The common shell MUST NOT require every domain to encode semantics into one universal status enum. Shared primitives such as active node, active edge, severity marker, and focus-source-ID are allowed; domain-specific state remains domain-specific.

### 5. Behavior Tree becomes the first provider, not the universal model

The Behavior Tree provider consumes the real executor snapshot from ADR 0123 and exposes, as applicable:

- active path;
- current Running node;
- Success/Failure and recent terminal transitions;
- abort/reset reason;
- per-node elapsed time;
- blackboard values;
- recent transitions; and
- runtime/validation errors mapped to source nodes.

Existing Behavior Tree runtime semantics and `NodeId` mapping remain valid. Only the top-level Editor presentation changes from a fixed dedicated Play tab to the Graph Debug shell.

### 6. Animation Graph runtime debugging observes the actual controller

Animation Preview remains an authoring tool. Graph Debug observes the actual Animation Controller running in Play.

The Animation Graph provider must expose enough read-only runtime evidence to answer what animation an Entity is executing and why. The first complete contract includes, where meaningful:

- runtime Entity;
- Animation Graph asset and `GraphId`;
- Animation Set asset;
- current State `NodeId`;
- previous and next State while transitioning;
- transition source/destination and progress;
- State elapsed time and clip/playback time;
- Graph parameter names and current runtime values;
- selected/resolved `MotionSlotId` and display name;
- resolved motion variant;
- resolved `MotionSource` / Animation Clip identity;
- recent animation-event firing history;
- active transition and the condition/reason that made it eligible/taken; and
- runtime error or unresolved binding state.

The Graph Canvas highlights current State and active transition using source IDs. The details pane exposes the resolution chain:

```text
State
  -> MotionSlotId
  -> Animation Set binding / selected variant
  -> MotionSource
  -> concrete clip
```

Transition-reason presentation is observational. It may expose evaluated conditions/parameters or another stable runtime reason descriptor; the Editor MUST NOT reimplement transition evaluation to explain the runtime.

### 7. Animation Preview and Graph Debug have separate authority

**Animation Preview** answers what the authored graph/clip would do under authoring preview controls. It may seek, override preview parameters, or exercise transitions without a running game.

**Graph Debug** answers what the actual Play-mode controller is doing now. It is read-only observation of runtime state.

The two may share source-resolution and Graph Canvas utilities, but MUST NOT share mutable simulation state or substitute one another's snapshot as authoritative. Preview rendering/resource residency follows ADR 0136; Graph Debug does not become a second preview renderer merely to inspect execution.

### 8. Source resolution follows the authoritative working-copy contract

ADR 0139 defines the current Editor working copy. Graph Debug uses that working copy for source canvas/navigation when available, with saved disk as fallback only when no working copy exists. Separately, each provider reports the source identity/generation used to create the running instance.

The shell compares:

```text
runtime source identity/generation
vs.
current Editor working-copy identity/revision
```

If they differ, Graph Debug shows stale-source state. It MUST NOT silently reload disk to hide unsaved edits and MUST NOT claim that the newest working copy is what an older runtime instance executes. Source overlays are shown only when the stable mapping is still valid.

### 9. Add Node presentation is semantic, not a redundant domain heading

The shared node schema/catalog remains authoritative. This decision changes only palette presentation.

For the current Animation Graph schema, whose useful choices are `Entry` and `State`, Add Node should present those choices directly. The string `Animation Graph` is not useful as a category heading when every listed node is already in that domain.

As node types grow, categories should describe meaning, for example:

- State;
- Blend;
- Control; and
- Utility.

A domain name may appear in cross-domain search/filter context, but a single-domain palette MUST NOT require a redundant domain heading. Behavior Tree keeps meaningful semantic categories such as composite, decorator, condition, and action.

### 10. Graph Debug is read-only and never becomes a second runtime controller

The shell/providers observe real runtime state. They do not own execution, transition decisions, Behavior Tree traversal, Animation Set resolution, or parameter mutation.

Pause/step/breakpoint or explicit runtime-edit controls may be added later, but write-capable debugging requires an explicit runtime-control contract. Read-only observation is the baseline.

## Scope / Non-goals

This ADR covers Play-mode graph target discovery, the shared Graph Debug shell, domain providers, Behavior Tree integration, Animation Graph runtime debugging, stale-source handling, and Add Node category presentation.

It does not replace authoring Graph editors, remove Animation Preview, force runtime semantics into the common Graph model, define Visual Script semantics, persist runtime debug state, implement breakpoints, or implement the described UI in this ADR-only change.

## UX model

```text
enter Play
  -> live graph targets are discovered
  -> Graph Debug appears only when at least one target exists
  -> choose Entity + graph domain
  -> source Graph opens read-only in shared canvas
  -> active source IDs are highlighted
  -> domain details explain runtime-specific state
  -> navigation may open the authored source for repair
```

When no target exists, Play keeps only relevant non-graph primary views.

## Diagnostic ownership

Runtime/provider failures follow ADR 0137. A provider may show concise runtime context or a source-local error, but repairable authoring detail remains in Problems. The shell does not duplicate full Problems prose.

## Graph Debug architecture

```text
runtime domain
  -> GUI-free execution snapshot + stable source mapping
  -> Editor domain provider
  -> common Graph Debug target model / highlight primitives
  -> shared Graph Canvas + target selector + details host
```

Future graph domains integrate by adding a provider and runtime observation DTO, not another permanent Play tab or another graph canvas.

## Domain-specific debug provider contract

A provider defines:

- stable runtime-debug domain key and display label;
- live target enumeration;
- runtime-instance to source-Graph resolution;
- active/focusable `NodeId`/`EdgeId` values;
- runtime snapshot/source-generation identity;
- domain-specific details and optional overlay meaning; and
- behavior when source or runtime state disappears.

A provider MUST NOT mutate source Graphs as a side effect of observation, use display names as source identity, make runtime crates depend on Editor UI types, or copy domain runtime state into common Graph serialization.

## Animation Preview responsibility boundary

Animation Preview is authoring-time simulation; Graph Debug is Play-runtime observation. Preview uses ADR 0136's project/device preview residency where it renders assets. Graph Debug uses runtime snapshots and source graph presentation, not a duplicate preview world.

## Working-copy / saved-copy model

Graph Debug consumes ADR 0139's working-copy read contract. Disk is not an automatic freshness source. A runtime instance may legitimately be older than the current working copy; this is represented as stale-source state.

## AI Studio presentation boundary

No new AI Studio presentation authority is created. AI Studio may later consume explicit read-only runtime-debug capabilities, but the Graph Debug shell is not an authoring writer or Agent Host. Local/detached AI Studio remains ADR 0131/0135 territory; remote access remains ADR 0133.

## Stable ID / serialization impact

No Graph, GraphView, Scene, Animation Set, or Behavior Tree serialization changes are introduced. Existing `GraphId`, `NodeId`, `EdgeId`, `MotionSlotId`, Entity and Asset IDs remain mapping keys. Runtime target handles, execution generations, debug history, selection, highlights, stale flags, and details-pane state are transient.

## Public API / crate boundary impact

Runtime graph domains may need richer public read-only snapshot DTOs. Those DTOs belong with the runtime/domain ownership that computes semantics. The Editor owns provider registration and Graph Debug presentation. The authoring Graph foundation remains GUI/runtime-semantics neutral. Runtime crates MUST NOT depend on `editor`.

If a common debug descriptor crate is later justified, it may contain GUI-free observation/source-mapping contracts but MUST NOT force Behavior Tree and Animation semantics into one type.

## Migration / compatibility

Existing Behavior Tree execution snapshots and source `NodeId` mappings remain valid; their UI moves into the provider shell. Existing Animation Preview remains. No project files migrate. Existing node-schema category metadata remains valid; only presentation of the redundant single-domain Animation Graph heading changes.

## Testing strategy

Implementation must cover at least:

- Graph Debug hidden when no provider enumerates a live target;
- mixed Animation/Behavior targets enumerate under correct Entities;
- target disappearance cannot leave a stale snapshot labeled live;
- common canvas selection/Frame All works for provider source graphs;
- stable `NodeId`/`EdgeId` mappings drive highlights;
- stale working-copy/runtime generations are detected;
- Behavior Tree presentation matches executor snapshots;
- Animation provider reports State, transition, parameters, slot, set, resolved motion/clip, event history, and error state from runtime evidence;
- transition reasons are observed, not reevaluated in Editor code;
- Graph Debug never mutates graph documents; and
- Add Node omits the redundant Animation Graph category while preserving semantic grouping.

## Visual Validation requirements

This ADR-only documentation change requires no Visual Validation. Implementation of the Play header, Graph Debug shell, runtime overlays, target list, details pane, stale-source presentation, or Add Node palette requires Visual Validation for zero targets, Animation-only, Behavior-only, mixed targets, active transition progress, stale source, long names, and the current Entry/State-only palette.

## Rollout / implementation phases

1. Put the existing Behavior Tree integration behind a provider and introduce the Graph Debug shell.
2. Add live-target discovery and zero-target tab hiding.
3. Move Behavior Tree presentation into the provider without changing executor semantics.
4. Extend Animation runtime observation DTOs and add the Animation provider.
5. Add stale working-copy/runtime source detection through ADR 0139.
6. Simplify Animation Graph Add Node presentation.
7. Add future graph domains only through the provider contract.

## Rejected alternatives

### Add a permanent Animation Graph tab beside Behavior Tree

Rejected. Every future graph domain would add another top-level tab and duplicate shared interaction.

### Copy the Behavior Tree debugger and rename statuses

Rejected. Running/Success/Failure/Abort are Behavior Tree semantics, not a universal graph runtime model.

### Put every runtime semantic into one common GraphDebugStatus enum

Rejected. It would make the domain-neutral layer depend on the union of all current/future graph semantics.

### Always show a disabled Graph Debug tab

Rejected for the baseline. Hiding the entry when there are no live targets keeps Play chrome relevant.

### Read source graphs from disk on every debug refresh

Rejected. It hides unsaved working copies and may overlay runtime state on a different source.

### Use display names for runtime-to-source mapping

Rejected. Names are editable/non-unique; stable IDs are the contract.

### Merge Animation Preview and runtime Graph Debug

Rejected. Preview simulates authoring choices; Graph Debug observes the actual running controller.
