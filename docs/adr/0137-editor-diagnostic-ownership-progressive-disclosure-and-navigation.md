# ADR 0137: Editor Diagnostic Ownership, Progressive Disclosure, and Navigation

Status: Accepted
Date: 2026-08-17
Amends: ADR 0068
Relates to: ADR 0011, ADR 0058, ADR 0072, ADR 0084, ADR 0085, ADR 0116

## Problem

GameEngine already has structured authoring diagnostics, Problems, Console, Inspector and Hierarchy context, and Scene View preview diagnostics, but those surfaces do not yet share one presentation-ownership contract. As a result, implementation mechanisms can be exposed instead of repairable facts.

The most visible example is Animation Controller conversion. A Graph/Animation Set binding mismatch can surface as `editor.scene_view.components_skipped` plus an abstract message such as `must be an Animation Set binding for every graph motion slot`. That does not identify the missing slot, the state requiring it, or the authored assets the user should open.

ADR 0068 correctly keeps Scene View alive by skipping component conversion failures during best-effort preview. Its original requirement to list skipped components as persistent yellow Scene View prose is too broad for a primary authoring workspace. The conversion policy should remain, while repair guidance moves to structured diagnostics and navigation.

## Decision

### 1. Diagnostic facts and presentation are separate responsibilities

The subsystem that knows a semantic rule owns the diagnostic fact: stable code, severity, concise human explanation, and the most specific stable authoring target it can identify. Editor surfaces own presentation, aggregation, progressive disclosure, and navigation.

The Editor MUST NOT make a transport or conversion mechanism the primary user-facing problem when a domain-specific repairable cause is available. Internal facts such as "runtime conversion skipped" may remain in diagnostic logging and Console output. Problems should show the semantic cause.

`engine-authoring::Diagnostic` remains GUI-free. UI callbacks, egui widget handles, window identities, and tab names are not serialized into the shared diagnostic DTO.

### 2. Problems is the authoritative repair surface

Problems is the formal aggregate for repairable diagnostics. A detailed Problems entry SHOULD answer:

- which authored object is affected;
- which invariant is violated;
- the concrete missing, invalid, or conflicting value when known;
- which related authored objects participate in the failure; and
- which repair locations can be opened directly.

The Editor MAY derive several navigation actions from one semantic diagnostic. The initial implementation should use an Editor-only navigation projection over stable authoring targets rather than persist presentation routes. Conceptually:

```text
EditorDiagnosticEntry
  diagnostic: Diagnostic
  navigation:
    - Select entity
    - Reveal component in Inspector
    - Open asset
    - Open graph and frame node
```

Each action resolves its target at activation time.

### 3. Animation Controller binding diagnostics identify the exact broken binding

Animation Graph + Animation Set binding diagnostics MUST make available enough structured information to identify at least:

- the affected authoring Entity;
- the `engine.animation_controller` component;
- the Animation Graph asset and `GraphId`;
- the Animation Set asset;
- the missing Motion Slot display name;
- the stable `MotionSlotId`; and
- every Animation State that references that slot, using source `NodeId` where available.

The primary Problems text should stay concise and repair-oriented, for example:

```text
Animation Set "HeroSet" is missing Motion Slot "Run" (motion_...)
required by State "Run" in Graph "PlayerGraph".
```

Display names are explanatory only. Identity and navigation MUST use stable IDs, not the string `Run`.

When resolvable, the entry SHOULD offer actions to:

1. select the Entity;
2. reveal `engine.animation_controller` in Inspector;
3. open the Animation Graph and frame/select the requiring State or States; and
4. open the Animation Set and focus the missing Motion Slot binding.

If several States reference one missing slot, one semantic binding problem may expose all related States rather than emit indistinguishable long messages.

### 4. Scene View owns long-form prose only for preview-fatal failures

Scene View is a primary authoring workspace. It displays blocking prose directly only when Scene Preview itself cannot meaningfully exist, for example:

- renderer or GPU initialization/fatal rendering failure;
- camera/viewport creation failure that prevents a usable preview; or
- scene-level conversion/runtime-world failure that makes the preview world untrustworthy as a whole.

Ordinary component validation errors and ADR 0068 best-effort component skips do not place persistent long diagnostic paragraphs over Scene View. A compact status affordance may indicate that the preview is incomplete, but repairable detail belongs in Problems.

The conversion layer may continue to record `scene_bridge.component_skipped` or equivalent internal evidence.

### 5. Hierarchy, Inspector, Problems, Scene View, and Console use progressive disclosure

| Surface | Responsibility |
| --- | --- |
| Scene View | Preview-fatal failures; compact non-fatal preview status only |
| Hierarchy | Per-Entity warning/error indicator and severity summary |
| Inspector | Concise selected Entity/Component summary and navigation to Problems |
| Problems | Authoritative repairable diagnostic details and navigation |
| Console | Runtime logs, internal diagnostics, debug traces, provider/process output |

The same semantic issue MUST NOT be copied as the same long paragraph into every surface. Hierarchy and Inspector summarize; Problems expands; Console preserves runtime/internal evidence when useful.

Removing Scene View prose must not make the issue invisible: contextual indicators and the Problems surface retain discoverability.

### 6. Diagnostic navigation is a projection over stable targets

The shared `DiagnosticTarget` variants for Entity, Component, Asset, Graph, Node, Edge, Port, Group, and source file remain the semantic navigation basis. Multiple repair actions may be represented by an Editor-only projection over those identities.

The initial implementation MUST NOT persist window routing, tab names, selection indices, or display labels as diagnostic identity. If a target no longer exists because the document changed, navigation fails softly and the diagnostic is refreshed or marked stale; it MUST NOT select a different object merely because its display name matches.

### 7. Conversion diagnostics are cause-oriented, not mechanism-oriented

Best-effort preview conversion is an implementation policy, not a user workflow. When a component cannot be projected into the preview runtime:

1. retain internal conversion evidence for engineering diagnosis;
2. surface an existing domain validation diagnostic when it explains the cause;
3. otherwise create a repair-oriented Problems diagnostic owned by the relevant authoring/bridge boundary; and
4. avoid requiring the user to understand that a runtime component was "skipped" in order to repair authoring data.

A fallback conversion diagnostic may explain that preview behavior is unavailable for that component, but it must identify the Entity, Component, and actionable conversion reason.

## Scope / Non-goals

This ADR defines diagnostic ownership, progressive disclosure, Animation Controller binding detail, and Editor navigation. It does not:

- change ADR 0068 best-effort conversion semantics;
- weaken strict Play/player/package validation;
- implement Hierarchy indicators, Inspector summaries, Problems actions, or new diagnostic producers in this ADR-only change;
- redesign Console logging;
- change Animation Graph, Animation Set, or Animation Controller serialization;
- define Graph runtime debugging, which is ADR 0138;
- define Editor working-copy coherence, which is ADR 0139; or
- address the Animation Set ComboBox popup-height issue.

## UX model

```text
problem occurs
  -> compact severity becomes visible in context
  -> Problems contains one repair-oriented semantic entry
  -> user opens the entry
  -> direct actions navigate to Entity / Component / Graph State / Animation Set
  -> edit updates the authoritative working copy
  -> validation refresh removes or updates the entry
```

Scene View continues to show as much of the scene as it safely can throughout this flow.

## Diagnostic ownership

Domain validators own domain facts. Scene conversion owns conversion evidence. Runtime systems own runtime failures. Editor presentation owns deduplication, progressive disclosure, and navigation affordances. A presentation layer may combine evidence, but it MUST NOT silently invent a different semantic cause or severity.

## Graph Debug architecture

Graph Debug follows the same presentation rule through ADR 0138: it may show concise runtime context and source-local status, while repairable authoring detail remains in Problems. This ADR does not define Graph Debug providers.

## Domain-specific debug provider contract

Not defined here. Providers consume/produce diagnostics according to this ownership model but are specified by ADR 0138.

## Animation Preview responsibility boundary

Animation Preview may surface concise preview-local state, but authoring validation and binding failures use the same Problems/navigation contract. Preview-specific simulation state is not a separate diagnostic authority.

## Working-copy / saved-copy model

Diagnostic producers in one Editor process validate the authoritative working copy defined by ADR 0139. They MUST NOT suppress a current problem by silently validating an older saved file.

## AI Studio presentation boundary

AI Studio may link to the same stable diagnostic targets, but it does not become a second diagnostic or authoring authority. Local/detached AI Studio presentation remains governed by ADR 0131; remote presentation by ADR 0133.

## Stable ID / serialization impact

No canonical project serialization changes are introduced. Existing stable IDs remain authoritative. `MotionSlotId` is binding identity, `NodeId` identifies requiring Animation States, and Entity/Asset/Graph identities remain unchanged. Editor navigation metadata is transient application state.

If a future implementation extends the shared `Diagnostic` DTO with related targets, that is an explicit public diagnostic-API change and still MUST NOT become Scene/Graph/Animation Set project serialization.

## Public API / crate boundary impact

`engine-authoring` remains GUI-free and owns shared semantic diagnostics and `DiagnosticTarget`. Domain validators remain in their existing authoring/runtime domain boundaries. The Editor may add an application-layer diagnostic presentation/navigation model that depends on authoring IDs and Editor workspace services. Runtime crates MUST NOT depend on Editor navigation types.

Animation binding validation may expose additional structured evidence needed for Graph/Set/State navigation, but Animation Set or Graph semantics MUST NOT move into GUI code.

## Migration / compatibility

Existing diagnostic codes remain valid unless an implementation change explicitly replaces a mechanism-oriented code with a semantic code and updates current consumers/tests together. `editor.scene_view.components_skipped` may remain an internal/logging code; user-facing Problems entries do not preserve its mechanism-oriented text as a compatibility contract.

Existing scenes, graphs, animation sets, and graph views require no migration. ADR 0068 is amended only in presentation; best-effort skipping remains Accepted.

## Testing strategy

Implementation must cover at least:

- a missing Animation Set slot reports Entity, Graph, Set, `MotionSlotId`, and requiring State `NodeId`;
- multiple States referencing one missing slot remain discoverable without name matching;
- Problems actions resolve the intended Entity/Component/asset/graph node;
- stale/deleted targets fail navigation safely;
- best-effort conversion continues previewing valid components;
- non-fatal component skips do not create persistent Scene View prose;
- preview-fatal failures still create direct Scene View failure presentation;
- Hierarchy/Inspector summaries agree with Problems severity; and
- tests assert stable codes/targets rather than complete human message wording.

## Visual Validation requirements

This ADR-only documentation change requires no Visual Validation. When implemented, Scene View, Hierarchy, Inspector, and Problems presentation changes require Visual Validation. It must prove that a non-fatal Animation Controller problem does not cover Scene View with long prose, the affected Entity remains discoverable, Problems carries full repair detail, and navigation lands on the intended context.

## Rollout / implementation phases

1. Add the Editor diagnostic-presentation/navigation projection.
2. Upgrade Animation Controller binding diagnostics and repair actions.
3. Move non-fatal Scene View skip prose to Problems while retaining internal evidence.
4. Add Hierarchy severity indicators and Inspector summaries.
5. Audit other mechanism-oriented Editor diagnostics against the same ownership rules.

## Rejected alternatives

### Make every diagnostic message self-contained and very long

Rejected. Long text cannot provide reliable navigation and duplicates poorly across surfaces.

### Keep all Scene View conversion warnings as yellow overlay prose

Rejected. It consumes the primary authoring workspace and exposes preview implementation details.

### Put all diagnostics only in Problems

Rejected. Problems owns detail, but contextual indicators are needed for discoverability.

### Mirror the same full message everywhere

Rejected. It creates noise and unclear ownership.

### Store Editor navigation callbacks or tab names in `Diagnostic`

Rejected. Shared diagnostics must remain GUI-free across Editor, CLI, MCP, tests, and future frontends.
