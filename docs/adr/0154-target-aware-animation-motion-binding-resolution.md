# ADR 0154: Target-Aware Animation Motion Binding Resolution and Variant Authoring

Status: Proposed
Date: 2026-08-18
Amends: ADR 0079, ADR 0085, ADR 0099, ADR 0110

## Context

ADR 0085 makes one `Animation Set` reusable across character rigs by binding stable Motion Slots instead of embedding target-specific runtime handles. ADR 0110 then generalized each binding to an explicitly tagged `MotionSourceRef` with `Auto`, `Native`, and `Humanoid` variants. Its automatic precedence is intentionally lossless-first: direct Native on the target skeleton, then an explicit `RetargetMap`, then Humanoid adaptation, otherwise unsupported. Explicit selection still wins over that automatic ordering.

The implemented authoring and runtime paths expose three gaps around that contract.

First, target compatibility is currently discovered too late. `resolve_animation_binding_clip` in the Scene Bridge receives the actual entity skeleton and performs the real target-dependent decision only while the scene is being converted to runtime state. An `Auto` binding can therefore be valid for one character and unsupported for another, but the editor does not report that when the Animation Set is assigned, while the scene is edited, or before conversion reaches that binding. A known authoring incompatibility may surface only as a generic Play/runtime asset failure.

Second, the Animation Set picker does not present Humanoid as a variant of the same logical motion. `Auto` and `Native` choices are emitted from Native Animation sub-assets, while the picker independently collects every `HumanoidMotion` sub-asset in the project and appends them to one flat list. That presentation hides the property ADR 0110 intended to make visible: a HumanoidMotion is a portable variant of one logical source motion and may be adapted to any target that has a valid HumanoidProfile. It also mixes unrelated Humanoid motions beside target-specific Native choices.

The imported identity model already contains the information needed to avoid that flattening. A HumanoidMotion ID is deterministically derived from its Native clip ID with the `humanoid` child derivation, and import builds one portable variant from a Native animation only when its source profile is usable. No additional persisted relationship is required merely to group the two variants.

Third, Humanoid resolution is implemented as a parallel specialized selection path even though the project already has target-oriented animation concepts. ADR 0099 records explicit `motion_model_sources` for VMD output models and emits target-specific Native clips. ADR 0079 records explicit Retarget Maps between source and target skeletons. It is tempting to put Humanoid into `motion_model_sources` as another target entry. That would, however, conflate two different meanings:

- a `motion_model_sources` entry is a concrete model that participates in import and produces a target-specific Native clip;
- Humanoid is a portable semantic representation that can later be baked for an open-ended class of target skeletons with valid HumanoidProfiles.

The design needs a unified compatibility view and one decision algorithm without inventing a fake Humanoid model target, changing the meaning of VMD import settings, or weakening ADR 0110's separation between full-fidelity Native animation and portable body motion.

The editor already has a Problems/validation pipeline for issues that should be known before Play. `validation.no_camera` is intentionally reported from authoring validation without starting the runtime, and the Problems surface can retain structured diagnostics with asset/entity/component targets and repair navigation. Animation target compatibility should use the same early-feedback model while preserving Scene and Animation Set editability.

No real PMX, VMD, FBX, glTF, or other third-party character fixture is required to define or test this contract. Future implementation tests for this ADR must construct synthetic `SkeletonAsset`, `BoneDef`, HumanoidProfile, motion catalog, manifest, and RetargetMap fixtures in code.

## Decision

### 1. Define one target-aware motion-resolution plan and use it everywhere

The project will have one GUI-free target-aware planning operation for an Animation Set motion binding. Conceptually it accepts:

- the persisted `MotionSourceRef`;
- the canonical Native clip and its source skeleton identity;
- the target skeleton identity;
- availability and validity of an explicit RetargetMap for that pair;
- availability of the Native clip's deterministic HumanoidMotion sibling; and
- availability and validity of the target HumanoidProfile.

It returns a structured plan rather than a runtime clip handle. The successful plan kinds are conceptually:

```text
NativeDirect
ExplicitRetargetMap
HumanoidBake
```

and an unsupported result carries a structured reason describing which required input was absent or invalid. The exact Rust type names are implementation details.

The planner is the single owner of ADR 0110 Decision 5 precedence. Scene Bridge conversion, editor scene validation, build/package planning, contextual Animation Set UI, and tests must consume that same decision contract instead of independently reproducing the ordering. The actual Native retarget bake and Humanoid bake remain specialized executors after the plan selects a route; this ADR unifies route selection, not the animation math.

The shared planning contract must remain GUI-free. Because the decision requires imported asset metadata plus animation/retarget/Humanoid domain information, it may be exposed through an `engine` composition service or a lower reusable domain service where dependency direction permits. The `editor` must not own a second copy of the compatibility rules, and `authoring` must not gain a dependency on runtime/import infrastructure merely to run this check.

### 2. Preserve the existing explicit and automatic semantics exactly

Resolution keeps ADR 0110's current meaning.

For `Auto`:

1. use Native directly when the Native clip already targets the entity skeleton;
2. otherwise use an applicable explicit RetargetMap;
3. otherwise use the Native clip's HumanoidMotion sibling plus the target HumanoidProfile;
4. otherwise report unsupported.

For explicit `Native`:

- direct Native remains valid on the same skeleton;
- explicit generic RetargetMap adaptation remains available for a different skeleton;
- Humanoid is never used as an implicit fallback.

For explicit `Humanoid`:

- the authored HumanoidMotion is used even when a Native or explicit-map route also exists;
- the target must have a structurally usable HumanoidProfile; and
- failure does not silently fall back to Native or a RetargetMap.

A planner refactor must not reinterpret an existing `MotionSourceRef`, an old Native clip reference, or an explicit choice based on which alternatives happen to exist.

### 3. Validate target compatibility as scene state, not as an Animation Set save invariant

An Animation Set by itself does not name one target rig. The same Set may intentionally be compatible with one model and incompatible with another. Therefore target compatibility is not a condition for saving the `*.animset.json` document.

Animation Set save validation continues to enforce target-independent invariants such as current schema, valid imported sub-asset kinds, stable references, duplicate layers, and event validity. It may additionally verify the deterministic Native/Humanoid sibling relationship when presenting or writing a Humanoid variant, but it must not require every project model to be a valid target.

Target-aware validation runs whenever sufficient scene context exists, including after relevant edits and before Play/build conversion. For each enabled Animation Controller whose graph and Animation Set are assigned, validation resolves the entity's target skeleton and checks every required primary binding and overlay with the shared planner. Changes to the controller, Skinned Model/skeleton, Animation Set, referenced motion metadata, HumanoidProfile, or RetargetMap must invalidate the relevant validation result.

An unsupported authored binding is an authoring **error** because Play/build cannot produce the required target-bound clip. It is shown in Problems before runtime conversion, targeted at the Animation Controller component, with related targets for the Animation Set, motion source, target skeleton/model when available, and any relevant RetargetMap or Humanoid configuration repair surface. The message must include the authored variant, target, and the failed route/reason rather than reporting only a generic missing asset.

The error does not prevent editing or saving the Scene or Animation Set. It does prevent a preflighted Play/build from claiming the scene is runnable until the binding is repaired. Runtime/Scene Bridge resolution remains defensive and rechecks the contract because files or imported state can change after validation; generic `editor.runtime.*` asset errors remain fallback diagnostics for unexpected I/O or state races, not the normal first discovery point for known motion incompatibility.

A structurally valid route with fidelity warnings remains usable. Existing diagnostics such as uncertain Humanoid mappings or excluded non-Humanoid channels stay warnings and do not become incompatibility errors merely because Humanoid is selected.

### 4. Present one logical Native clip with its own variants

Animation Set motion pickers must be organized around the canonical Native Animation sub-asset, not around three project-wide flat variant lists. For each logical Native clip the editor computes the variants that actually belong to that clip. A representative presentation is:

```text
Walk - Source Model
  Auto
  Native [Source/Target provenance]
  Humanoid [Reusable on valid Humanoid rigs]
```

The exact widget may use grouped menus, a two-stage logical-motion/variant selector, disclosure rows, or equivalent UI. The authoring contract is:

- `Auto` and `Native` are choices rooted at that Native clip ID;
- `Humanoid` is offered only when the deterministic HumanoidMotion child of that Native clip exists in the current imported catalog;
- Humanoid motions belonging to unrelated Native clips are never mixed into the current logical motion's variant group;
- target-specific Native provenance, including ADR 0099 `target_model_source` metadata, remains visible enough to distinguish multiple VMD output clips; and
- the Humanoid label communicates portability without implying full-fidelity transfer of source-specific channels.

For an ADR 0099 VMD source with several target-specific Native clips, each canonical target-specific Native `AssetId` remains its own logical variant root. If each root has a HumanoidMotion child, that Humanoid child remains associated with the Native bake it was derived from. The UI may group those roots under the same registered motion source for navigation, but it must not erase their distinct provenance or IDs.

When a target entity/skeleton context is available, the picker or adjacent detail UI should show the planner result (`Native`, `Retarget Map`, `Humanoid`, or `Unsupported`) for `Auto` and compatibility status for explicit choices. Without target context, the picker must not claim that `Auto` is globally resolvable; it describes policy and available source variants only.

### 5. Unify the compatibility catalog, not `motion_model_sources` persistence

Humanoid remains a first-class portable motion representation from ADR 0110. It is **not** persisted as a synthetic entry in `ImportSettings::motion_model_sources`, and no fake Humanoid model `AssetId` or default Humanoid skeleton is introduced.

Instead, authoring/UI code may expose a computed motion capability catalog for each canonical Native clip. Conceptually that catalog can describe:

- the Native clip and its concrete target-model/skeleton provenance;
- explicit RetargetMap routes to concrete targets; and
- a portable Humanoid capability when the Native clip's HumanoidMotion sibling exists.

This is the unified "what targets can this motion reach?" view. Concrete model targets and the Humanoid target class can appear together in presentation or diagnostics, while their persisted source-of-truth mechanisms remain distinct.

`motion_model_sources` keeps the ADR 0099 meaning: an ordered authoring selection of concrete model sources whose rigs receive target-specific Native import outputs. RetargetMap assets keep the ADR 0079 meaning: reviewable explicit conversion between concrete source and target skeletons. HumanoidProfile/HumanoidMotion keep the ADR 0110 meaning: semantic biped compatibility without an N-by-M explicit map matrix.

This means the dedicated Humanoid **bake mechanism** remains, but the dedicated spawn-only **route-selection mechanism** does not. After implementation, Scene Bridge should execute a shared resolution plan rather than privately deciding whether to enter `resolve_humanoid_animation_binding_clip`.

### 6. Use the same planner for packaging and preflight

Build/package reachability must evaluate reachable Animation Controller + Animation Set + target skeleton combinations with the same resolution planner used by editor validation and Scene Bridge. A successful build packages or bakes the route selected for each reachable target. A static incompatibility that the editor can diagnose must not be rediscovered only after launching the Player.

This does not make an uninstantiated Animation Set globally target-specific. Validation and packaging are performed for reachable target contexts. Existing explicit mechanisms for dynamically assigned/untraceable content remain the escape hatch where static reachability cannot know a runtime target; this ADR does not invent hidden name-based target inference.

### 7. Diagnostics are stable, actionable, and target-aware

Implementation introduces one stable incompatibility diagnostic family rather than translating expected compatibility failures into `editor.runtime.missing_asset`. A recommended primary code is:

```text
anim.motion_binding_unsupported
```

The diagnostic context should include, when known, the Animation Set ID, Motion Slot ID, authored variant, source motion ID, source skeleton ID, target skeleton ID, and planner reason/attempted routes. Related targets should make the Animation Set and relevant model/Humanoid configuration directly navigable from Problems.

Route-specific details may use structured context or additional narrowly scoped codes if implementation discovers a durable repair distinction, but UI code must not parse human-readable diagnostic messages to recover semantics.

### 8. Tests use synthetic assets only

Implementation tests for this decision must be self-contained in the workspace. They must not search for, download, or depend on real PMX/VMD/FBX/glTF character assets or animation clips.

At minimum, synthetic fixtures must cover:

- `Auto`: same skeleton resolves Native directly;
- `Auto`: explicit RetargetMap wins over an available Humanoid route;
- `Auto`: Humanoid is selected when Native is cross-skeleton, no explicit map exists, and both Humanoid inputs are valid;
- `Auto`: no valid route produces the early target-aware unsupported diagnostic;
- explicit `Native`: never falls back to Humanoid;
- explicit `Humanoid`: remains Humanoid even when a Native/Retarget route exists;
- an invalid or missing target HumanoidProfile is reported before runtime spawn when Humanoid is required;
- picker grouping offers only the HumanoidMotion deterministically derived from the selected Native clip and excludes unrelated project Humanoid motions;
- multiple ADR 0099 target-specific Native clips retain distinct target provenance while each groups with its own Humanoid child; and
- primary bindings and overlays use identical planning rules.

Skeleton tests should construct `SkeletonAsset`/`BoneDef` values and Humanoid profiles directly in Rust test code, following the existing synthetic humanoid-test pattern. Manifest/catalog tests should build minimal in-memory metadata rather than requiring third-party files.

## Consequences

- Authors learn that an Auto/Native/Humanoid binding cannot drive a specific character while editing the scene, rather than first learning it during spawn or Play.
- `Auto` stays reusable and target-dependent; the editor explains which path it will take without freezing that path into the Animation Set.
- Animation Set save remains usable for partially authored and intentionally multi-target content.
- One shared plan prevents Editor, build, and Scene Bridge from drifting on Native/Retarget/Humanoid precedence.
- The picker becomes consistent with ADR 0110 Decision 6 without changing `MotionSourceRef` persistence.
- VMD `motion_model_sources` keeps its concrete import-output meaning and does not acquire a sentinel or polymorphic target entry.
- HumanoidMotion remains a first-class portable asset and dedicated bake representation, preserving source-specific Native fidelity and ADR 0110's cache model.
- Problems can offer specific repair navigation for missing Retarget Maps or Humanoid configuration instead of a generic runtime asset error.
- Target-aware validation has to invalidate when imported model/profile/map state changes, so implementations must avoid stale compatibility caches.

## Alternatives Considered

### Keep compatibility checks only in Scene Bridge spawn

Rejected. It preserves one implementation site but leaves a deterministically knowable authoring error undiscovered until Play/runtime conversion and produces poor repair context.

### Block Animation Set save unless every project model is compatible

Rejected. An Animation Set intentionally has no single target rig and may be reused only by a subset of project characters. Global compatibility is neither required nor stable as models are added to a project.

### Resolve Auto at Animation Set save time and persist the chosen target clip

Rejected. That would make a reusable Set depend on whichever model happened to be active during editing and would destroy ADR 0110's target-dependent automatic precedence.

### Put a Humanoid sentinel directly into `motion_model_sources`

Rejected. The list is an import setting for concrete target models and target-specific Native outputs. A HumanoidMotion is not one concrete output target; it is a portable intermediate that can later produce many target-bound clips. A sentinel would mix lifetimes, identity rules, dependency tracking, and cache inputs inside one field and would require a persisted-schema semantic change without improving the underlying resolution contract.

### Replace HumanoidMotion with generated Retarget Maps

Rejected for the same reason recorded by ADR 0110. Pairwise generated maps do not provide a first-class skeleton-independent motion, and they turn broad biped compatibility back into an N-by-M target matrix.

### Keep separate Native and Humanoid resolvers but only regroup the picker UI

Rejected as the complete fix. It solves the visual confusion but leaves Editor validation, packaging, and Scene Bridge free to disagree about route availability and precedence. Specialized bake executors remain appropriate; duplicated route selection does not.

### Make Humanoid the universal canonical animation representation

Rejected. Native clips may contain twist helpers, hair, skirts, tails, weapons, morphs, and other source-specific channels. ADR 0110's Humanoid layer remains a compatibility fallback or explicit fidelity choice, not a replacement for Native animation or generic Retarget Maps.

## Compatibility and Migration

This ADR is an architecture proposal and does not itself change code or persisted project data.

The intended implementation requires **no Animation Set schema change**. `MotionSourceVariant::{Auto, Native, Humanoid}`, `MotionSourceRef { variant, asset }`, current Native clip IDs, deterministic HumanoidMotion child IDs, `motion_model_sources`, HumanoidProfiles, and RetargetMap assets retain their current persisted meanings. Existing explicit Native/Humanoid choices and Auto precedence remain unchanged.

The UI and validation layers may add computed catalog/plan state, but that state is transient and must not be serialized merely to cache presentation. If implementation later discovers a requirement that truly cannot be represented by the current schema, that change requires a separate explicit decision and ADR 0091/0115 current-format migration handling; it must not be smuggled into this implementation.

Because this ADR is Proposed, it does not amend the canonical authoring specification until accepted. If accepted and implemented in a way that changes normative authoring behavior, `docs/AI_FRIENDLY_AUTHORING_SPEC.md` must be updated in the implementation change as required by the ADR registry policy.
