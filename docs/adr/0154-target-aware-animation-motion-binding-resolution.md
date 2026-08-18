# ADR 0154: Animation Motion Candidates, Import-Owned Humanoid Variants, and Target-Aware Resolution

Status: Accepted
Date: 2026-08-18
Amends: ADR 0085, ADR 0099, ADR 0110
Relates to: ADR 0024, ADR 0079, ADR 0091, ADR 0115, ADR 0137

## Context

ADR 0085 makes an Animation Set bind stable Motion Slots to reusable animation content. ADR 0110 added skeleton-independent `HumanoidMotion` and currently represents the authored motion source with `MotionSourceVariant::{Auto, Native, Humanoid}`. For `Auto`, Scene Bridge resolves the actual target at conversion time in lossless-first order: direct Native, an explicit ADR 0079 RetargetMap, Humanoid adaptation, or unsupported.

That representation mixes two different questions. The author needs to choose **which animation candidate** a Motion Slot uses. Native, Retarget, Humanoid, and Failed are usually **results of applying that selected candidate to a particular target model**. Showing `Auto` and `Native` as separate rows for the same concrete clip makes an internal resolution policy look like different animation content.

The distinction is especially visible for ADR 0099 motion sources. One logical motion may have several target-specific Native clips. If logical motions `a` and `b` have Native clips for models `x` and `y`, the useful Animation Set candidates are conceptually:

```text
a / x
a / y
a / Humanoid

b / x
b / y
b / Humanoid
```

Here `a / x` means that the x-bound Native clip is the selected source candidate. It does not mean that playback is forced to stay Native. On a different target, that same candidate may resolve through an explicit RetargetMap or through Humanoid adaptation.

The current Editor also discovers compatibility too late. The real target skeleton is known when the Animation Set is used by an Animation Controller, but the route is primarily decided inside Scene Bridge spawn/conversion. An otherwise deterministic failure can therefore be discovered only when Play/runtime conversion reaches the binding. The existing Problems architecture already supports semantic, pre-Play diagnostics and should own this feedback instead of leaving expected incompatibility to a generic runtime asset error.

There is a second import-side gap. Model-contained animation import can already derive a `HumanoidMotion` when the source skeleton has a usable HumanoidProfile. Motion-only sources such as VMD currently produce model-specific Native clips but do not produce the corresponding portable logical `a / Humanoid` candidate. If Humanoid is meant to be a selectable property of the imported animation itself, creating it lazily when an Animation Set happens to use the motion is the wrong ownership boundary. Import/reimport must create the portable candidate as soon as sufficient Humanoid source context exists.

No real PMX, VMD, FBX, glTF, or other third-party model fixture is required to define or test this contract. Implementation tests must use synthetic skeletons, profiles, motions, manifest metadata, and RetargetMaps.

## Decision

### 1. Animation Set pickers select animation candidates, not adaptation modes

The primary Animation Set picker is organized by logical motion and the concrete candidates available for that motion. A representative list is:

```text
a / x
a / y
a / Humanoid

b / x
b / y
b / Humanoid
```

For a model-bound candidate such as `a / x`:

- `a` identifies the logical source motion;
- `x` identifies the concrete model/skeleton provenance of the selected Native `Animation` sub-asset; and
- persistence refers to that imported candidate by stable `AssetId`.

`Auto` and `Native` are not separate picker rows. Every model-bound candidate uses the one automatic target-resolution policy in Decision 2, so the normal UI does not need an `Auto` label at all.

`a / Humanoid` is a separate candidate because it is different animation content: the portable `HumanoidMotion` representation owned by logical motion `a`. Selecting it explicitly means to use the Humanoid representation.

Logical grouping and provenance come from stable import/catalog metadata, never from display-name equality. ADR 0099 target-specific clips remain distinct candidates and distinct AssetIds even when they have the same logical motion name.

### 2. A model-bound candidate always resolves Native -> Retarget -> Humanoid -> Failed

For the actual target skeleton, every selected model-bound `Animation` candidate uses exactly this order:

1. **Native** when the selected clip is already bound to the target skeleton;
2. **Retarget** when an applicable explicit ADR 0079 RetargetMap exists from the selected clip's source skeleton to the target skeleton;
3. **Humanoid** when the logical motion owns a usable imported Humanoid variant and the target has a structurally usable HumanoidProfile;
4. **Failed** when none of those routes is valid.

Selecting `a / x` therefore means "start from the x-bound version of a", not "force Native". On target `x` it normally reports Native. On target `y` it may report Retarget, Humanoid, or Failed according to the actual project data.

This is the only automatic policy for model-bound candidates. The current explicit Native-only authoring policy is removed from the new authoring contract. If the computed result is Humanoid and the author does not want that result, the Editor makes that fact visible and the author can choose another candidate that resolves Native or Retarget. A Native-only switch would only turn an otherwise usable Humanoid route into Failed; it cannot create a missing Native or Retarget route.

Same-skeleton Native remains highest priority. An explicit RetargetMap remains higher priority than automatic Humanoid adaptation. The lossless-first ordering from ADR 0110 therefore remains, while the redundant author-facing Auto/Native distinction does not.

### 3. Selecting `a / Humanoid` means Humanoid only

Selecting the portable Humanoid candidate explicitly produces:

- **Humanoid** when the target HumanoidProfile is structurally usable; or
- **Failed** otherwise.

An explicitly selected `HumanoidMotion` does not silently switch to a model-bound Native clip or RetargetMap. The author can already choose `a / x` or `a / y` when the model-bound automatic route is desired.

### 4. Each logical imported animation owns at most one portable Humanoid variant

The portable Humanoid candidate is import-owned content. It is generated during import/reimport, not on first Animation Set assignment, Scene conversion, Play, or runtime spawn.

Conceptually one logical animation owns:

```text
a
  Native candidates: a / x, a / y, ...
  Portable candidate: a / Humanoid   (optional, at most one)
```

The portable representation remains a first-class `HumanoidMotion` sub-asset rather than being embedded as mutable data inside every Native `AnimationClip`. This preserves ADR 0110's derived/imported asset and bake/cache boundaries while making ownership visible as one logical clip plus its portable sibling.

For model-contained sources such as FBX, glTF, or PMX animation data where the animation's source skeleton is already known, import uses that source skeleton and its usable HumanoidProfile. When conversion succeeds, import publishes the logical animation's Humanoid sibling in the same import/reimport operation that publishes its Native animation catalog.

A missing or structurally invalid source HumanoidProfile means that import publishes the Native candidate(s) but no Humanoid candidate. Configuring/fixing the profile and reimporting creates `a / Humanoid` before any Animation Set uses it.

The Editor and Scene Bridge never create a `HumanoidMotion` as a side effect of resolving a binding.

### 5. Motion-only sources use stable Humanoid source-model provenance

A motion-only format such as VMD has animation curves but no complete source skeleton/rest-pose HumanoidProfile of its own. It therefore cannot produce a semantically correct `a / Humanoid` until a Humanoid-capable model has been associated with the motion.

Motion-only import settings gain one optional stable **Humanoid source model** provenance, conceptually `motion_humanoid_source_model`. It identifies the registered model whose baked motion and HumanoidProfile define conversion into portable Humanoid semantics.

Rules are:

- the value must identify a registered associated model with a structurally usable HumanoidProfile;
- when `motion_original_model_source` is configured and usable, the Editor should offer it as the natural default Humanoid source;
- when exactly one associated Humanoid-capable model exists, the Editor may offer that model as the unambiguous choice;
- when several candidates exist, target-list order or display-name order must not silently choose one; the source provenance must be explicit and persisted;
- once selected, reordering or adding `motion_model_sources` does not silently change the Humanoid source; and
- changing the Humanoid source is an import-content change and triggers regeneration of the portable motion and dependent derived bakes.

When no usable Humanoid source model is configured, import still produces the concrete Native candidates such as `a / x` and `a / y`, but it produces no `a / Humanoid`. Once a valid source is configured, the next import/reimport creates the portable candidate immediately.

This means the expected authoring state is exactly what the candidate list communicates: a motion can temporarily have `a / x` and `a / y` without `a / Humanoid`; after Humanoid source context becomes available and reimport completes, the single `a / Humanoid` candidate appears.

### 6. The portable Humanoid identity is logical-motion-scoped, not target-scoped

ADR 0099 model-bound clips keep their existing target-specific identity based on motion source, target model, and source animation index. Those are the identities behind `a / x`, `a / y`, and so on.

The portable `a / Humanoid` candidate must not be duplicated as `a / Humanoid (x)` and `a / Humanoid (y)`. Its stable identity is target-independent and scoped to the registered motion source plus logical source animation index, with the Humanoid variant derivation. In imported catalog metadata it therefore has no `target_model_source`.

This aligns with the existing imported-sub-asset identity machinery: a `HumanoidMotion` with no target-model provenance derives from the source's logical Animation identity and then the `humanoid` child derivation, while model-specific Native clips retain `target_model_source`.

The chosen Humanoid source model is an import-content/provenance input, not part of the portable candidate's user-facing target identity. Changing that source regenerates the content behind the same logical `a / Humanoid` identity and invalidates dependent caches.

### 7. One shared target-aware planner owns the computed route

The project will have one GUI-free target-aware planning operation for Animation Set motion resolution. It accepts the selected candidate, target skeleton, and relevant imported metadata, RetargetMap state, HumanoidMotion availability, and target HumanoidProfile state.

It returns a structured result conceptually equivalent to:

```text
Native
Retarget { map }
Humanoid
Failed { reason }
```

The exact Rust type names are implementation details. Scene Bridge, Editor candidate preview, Problems validation, build/package planning, and tests must consume the same planner instead of reimplementing precedence independently.

The planner selects a route only. Existing Retarget and Humanoid bake implementations continue to execute the selected route and produce ordinary target-bound `AnimationClip` data before runtime animation sampling.

The planner must never find a Humanoid fallback by searching unrelated project assets for a matching display name. It resolves only the portable variant that belongs to the selected logical motion.

### 8. The Animation Set editor shows the route for a selected target model

The Animation Set editor provides target context for inspecting candidates. When opened from a scene/controller context it may use that entity's model as the initial preview target. Otherwise it provides a Target Preview model selector.

For target `x`, the candidate list can conceptually show:

```text
Target Preview: x

a / x          Native
a / y          Retarget
a / Humanoid   Humanoid
b / x          Failed
b / y          Humanoid
b / Humanoid   Humanoid
```

The exact widget layout is Editor-owned, but `Native`, `Retarget`, `Humanoid`, and `Failed` are first-class computed results. `Retarget` should expose which map is selected, while `Failed` exposes the structured reason. The currently selected binding should also keep its route visible in adjacent detail UI.

Changing Target Preview recomputes the results and does not rewrite the Animation Set. Without a target context, the picker shows candidates and provenance but does not claim a target-specific route.

One Animation Set may legitimately resolve the same candidate differently for different scene entities. The target result is therefore computed state, never persisted as the Animation Set's target.

### 9. Target incompatibility is reported before Play without blocking saves

An Animation Set remains target-independent and reusable, so target compatibility is not a condition for saving `*.animset.json`.

Whenever scene context identifies an Animation Controller, Animation Set, required Motion Slots, and target skeleton, Editor validation runs the shared planner for primary bindings and overlays. A `Failed` route is an authoring **error** in Problems because that entity cannot produce the required target-bound clip.

The diagnostic is reported before Play/build conversion and should identify the Animation Set, Motion Slot, selected candidate, target model/skeleton, attempted routes, and failure reason. Problems provides navigation to the relevant Animation Set/model/Humanoid/Retarget repair surfaces in accordance with ADR 0137.

The error does not prevent editing or saving the Scene or Animation Set. Play/build preflight must not claim the reachable scene is runnable while a required binding is Failed. Scene Bridge keeps a defensive recheck for stale files, changed import state, I/O failures, and races after validation.

A usable Humanoid route remains usable even when it carries fidelity warnings. Existing warnings for uncertain Humanoid mappings or excluded source-specific channels remain warnings rather than becoming Failed merely because Humanoid was selected.

### 10. Animation Set persistence stores candidate identity, not route policy

Implementation of this ADR changes the current Animation Set authoring contract and bumps the current `*.animset.json` schema from version 2 to version 3.

`MotionSourceVariant::{Auto, Native, Humanoid}` is removed from the current persisted binding contract. A motion source reference persists the stable selected imported sub-asset AssetId. The imported catalog kind identifies the candidate category:

- `ImportedSubAssetKind::Animation` is a model-bound candidate and always uses Decision 2;
- `ImportedSubAssetKind::HumanoidMotion` is the explicit portable candidate and uses Decision 3; and
- other imported sub-asset kinds are invalid for an Animation Set motion binding.

The same rule applies to overlay sources. Display names never determine the persisted category.

The exact serialized shape may continue to wrap the AssetId in an object for type clarity, but schema 3 does not persist an Auto/Native policy flag.

Because ADR 0115 defines a current-format-only baseline, schema-2 documents are not silently reinterpreted. Repository-owned fixtures/documents are updated with the implementation. An old explicit Humanoid reference maps conceptually to the same HumanoidMotion candidate. An old Auto reference maps conceptually to the same model-bound Animation candidate. An old explicit Native-only reference has no semantically identical schema-3 mode because this ADR deliberately removes Native-only fallback suppression.

### 11. `motion_model_sources` stays concrete; Humanoid is separate import output

ADR 0099 `motion_model_sources` keeps its existing meaning: concrete model sources whose rigs receive target-specific Native clips. It remains the source of candidates such as `a / x` and `a / y`.

Humanoid is not inserted into `motion_model_sources` as a fake target model. The optional Humanoid source-model provenance from Decision 5 answers a different question: which real associated rig supplies semantic conversion context for the one target-independent portable motion.

The manifest/import catalog therefore exposes both kinds of result without conflating them:

- zero or more concrete target-specific Native `Animation` candidates; and
- zero or one target-independent `HumanoidMotion` candidate for each logical source animation.

### 12. Packaging uses the same planner and imported Humanoid content

Build/package reachability evaluates each reachable Animation Controller + Animation Set + target skeleton combination with the shared planner. The selected route determines which target-bound Native/Retarget/Humanoid derived clip must be available in the package.

A statically knowable Failed route is rejected during preflight rather than being rediscovered only after Player launch. HumanoidMotion generation itself remains an import/reimport responsibility; packaging may bake an imported HumanoidMotion for reachable target skeletons but does not synthesize the portable source motion for the first time.

### 13. Tests use synthetic assets only

Implementation tests must not search for, download, or depend on real PMX/VMD/FBX/glTF assets. Synthetic fixtures construct `SkeletonAsset`, `BoneDef`, HumanoidProfiles/HumanoidMotion, manifest entries, target provenance, and RetargetMaps directly in code.

At minimum tests cover:

- `a / x` on skeleton x reports Native;
- a cross-skeleton model-bound candidate with an explicit map reports Retarget even when Humanoid is available;
- a model-bound candidate with no map but a valid logical Humanoid variant and target profile reports Humanoid;
- no valid route reports Failed with the early structured diagnostic;
- selecting `a / Humanoid` reports Humanoid and never switches to Native/Retarget;
- changing Target Preview changes computed badges without changing persisted Animation Set data;
- ADR 0099 target clips retain distinct `a / x` and `a / y` stable identities/provenance;
- model-contained import with a usable source profile publishes `a / Humanoid` during import;
- motion-only import without Humanoid source context publishes Native targets but no Humanoid candidate;
- configuring a valid Humanoid source model and reimporting publishes exactly one target-independent `a / Humanoid`;
- adding/reordering `motion_model_sources` does not silently change persisted Humanoid source provenance;
- `a / x` and `a / y` do not create duplicate Humanoid candidates;
- primary bindings and overlays use identical planner rules; and
- Editor validation and Scene Bridge planning agree for identical synthetic inputs.

## Consequences

- Animation Set candidate rows answer which actual animation representation the author is selecting rather than exposing internal Auto/Native policy terminology.
- `Native`, `Retarget`, `Humanoid`, and `Failed` become visible target-specific results.
- `Auto` disappears from normal UI because every model-bound candidate uses one automatic policy.
- Explicit Native-only authoring is intentionally removed; explicit Humanoid remains a distinct portable candidate.
- Each logical animation can expose one portable Humanoid sibling before any Animation Set uses it.
- VMD and other motion-only sources can gain that sibling once a stable Humanoid-capable source model is configured, without creating one Humanoid row per target model.
- Humanoid generation remains import-owned, while target-specific Humanoid baking remains derived/cache-owned.
- Animation Sets remain reusable across models because preview/result target state is not persisted in the Set.
- Problems can report incompatibility before Play without making asset saving globally target-dependent.
- One planner prevents Editor, build, and Scene Bridge from drifting on resolution precedence.
- Implementation requires coordinated Animation Set schema, importer/catalog, Editor, validation, packaging, and Scene Bridge changes.

## Alternatives Considered

### Keep Auto, Native, and Humanoid as three rows

Rejected. Auto and Native are policies over the same concrete Animation candidate, while Humanoid is separate portable content. The presentation mixes candidate identity with target-resolution behavior.

### Keep an advanced Native-only switch

Rejected. The target-aware result already shows when Humanoid would be used. The author can select another concrete candidate when a Native/Retarget route exists. The switch cannot create such a route when it does not exist.

### Generate HumanoidMotion lazily when an Animation Set resolves

Rejected. Portable Humanoid content belongs to the imported logical animation. Lazy generation would make the asset catalog depend on whether a Scene/Animation Set happened to touch the motion and would preserve the current late-discovery problem.

### Generate one HumanoidMotion per target-specific Native clip

Rejected for motion-only multi-target sources. It would turn one logical `a / Humanoid` into `a / Humanoid (x)`, `a / Humanoid (y)`, and so on, even though the purpose of HumanoidMotion is to provide one portable representation. Concrete x/y provenance belongs to Native candidates; the portable variant has one explicit source provenance and target-independent identity.

### Choose the first Humanoid-capable `motion_model_sources` entry automatically

Rejected. ADR 0099 defines target order as non-semantic. Reordering a display/authoring list must not silently change portable animation content. The source provenance is persisted explicitly when more than one choice exists.

### Put Humanoid into `motion_model_sources`

Rejected. That list names concrete models receiving Native outputs. Humanoid is a portable semantic representation, not a concrete target model.

### Flatten all project HumanoidMotion assets into one picker list

Rejected. It loses logical ownership and can present unrelated motions as interchangeable. Candidate grouping uses stable imported identity and logical source metadata.

### Block Animation Set save when the preview target is Failed

Rejected. The preview target is not part of Animation Set identity. A Set may be valid for a different model and must remain editable/saveable while content is incomplete.

## Compatibility and Migration

This ADR is a design proposal and does not itself modify runtime, importer, Editor, or serialized project data.

If accepted, implementation changes the current Animation Set schema to version 3, removes the author-facing/persisted Auto-versus-Native policy distinction, adds the motion-only Humanoid source-model provenance required by Decision 5, and updates repository-owned current-format fixtures/documents in the same change. ADR 0115 remains authoritative: obsolete schema-2 authoring shapes are rejected rather than maintained through permanent compatibility aliases.

Stable MotionSlotIds, concrete ADR 0099 target-specific Native clip IDs, RetargetMap assets, Skeleton identities, and HumanoidProfile semantics remain unchanged. The portable Humanoid candidate uses one logical-motion-scoped imported identity rather than one identity per concrete target.

When accepted, this ADR amends ADR 0110 Decision 3 so HumanoidMotion is explicitly import/reimport-owned and available before Animation Set resolution; amends Decision 5 by applying lossless-first precedence uniformly to every model-bound candidate while removing the explicit Native-only exception; replaces Decision 6 with candidate rows plus target-result presentation; and replaces Decision 7's three-way policy reference with stable candidate identity. ADR 0110's semantic motion representation, fidelity limits, target bake/cache, and runtime AnimationClip boundary otherwise remain in force.

When accepted and implemented, `docs/AI_FRIENDLY_AUTHORING_SPEC.md` must be updated with the new current Animation Set authoring contract and motion-only Humanoid import provenance.
