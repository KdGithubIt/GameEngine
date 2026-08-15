# ADR 0105: Model-level Material and Texture Sub-asset Overrides

- Status: Accepted
- Date: 2026-08-13

## Context

ADR 0101 introduced extraction for imported Material sub-assets. Its
`material_remaps` entry already preserves the imported sub-asset ID while
redirecting runtime loading to a standalone Material file. However, the editor
only exposed that mapping as an extraction result. Authors could not select an
existing Material, could not apply the duplicated Material from model B to a
material slot owned by model A at the asset level, and could not see the
mapping in the Asset Browser list. Texture sub-assets had no equivalent
override at all.

Per-entity Material assignment remains useful for one-off instances, but it is
the wrong boundary when every instance of an imported model should share the
same replacement. Arbitrary replacement of Mesh, Skin, Skeleton, Morph, or
RigidBodyRig sub-assets is unsafe without compatibility validation and is not
part of this decision.

## Decision

Imported Material and Texture sub-assets support model-level overrides:

- `material_remaps` continues to map an imported Material ID to a standalone
  Material ID. The editor now permits choosing any compatible registered
  `.material.json` asset, including a Material duplicated from another model,
  as well as the built-in Materials.
- `ImportSettings` gains additive `texture_remaps`, mapping an imported Texture
  ID to a registered standalone Texture ID.
- Override pickers intentionally exclude imported sub-assets as targets. This
  prevents author-created remap chains and cycles and gives every target a
  concrete independently registered file. Runtime still detects cycles so a
  manually edited malformed manifest fails with a diagnostic fallback.
- Material texture resolution uses one path for directly imported and
  extracted Materials. It first applies `texture_remaps`, then decodes either
  the standalone replacement file or the original imported texture through
  the shared model-import cache.
- Asset Browser sub-asset rows display `overridden -> <target>`. The Asset
  Inspector displays `Imported` or the effective target, offers compatible
  choices, and lists references in the currently open scene. A listed source
  reference is marked as one to which the model override applies; a direct
  reference to the replacement is distinguished separately.
- Material and Texture remap maps participate in the Scene View preview key.
  Saving an edited standalone Material or changing an override explicitly
  invalidates the persistent preview world while retaining reusable model and
  GPU caches, so Edit Mode and Play resolve the same value immediately.
  Continuous Material controls are coalesced over a short quiet period before
  their file write, validation, and preview rebuild; this prevents a color
  picker drag from synchronously replacing the file and rebuilding the whole
  preview world once per frame.

The mapping belongs to the imported source's manifest entry, not to an entity.
Changing it therefore affects every scene entity, prefab instance, and runtime
load that keeps referencing that imported sub-asset ID. Reset removes only the
mapping and immediately restores the imported value; it does not delete a
duplicated or extracted asset.

A duplicated imported Material is written beside its owning model source. If
the slot already overrides to a standalone Material, another duplicate is
written beside that effective Material instead. Built-in Materials have no
source folder, so their duplicates use `assets/materials/` as the fallback.

## Consequences

- A duplicated Material from model B can be selected on model A's Material
  sub-asset once, and all instances of model A use it without per-entity edits.
- Authors can identify overridden children and their targets without first
  opening each sub-asset, and can inspect usage in the current scene.
- Existing manifests remain compatible because `texture_remaps` defaults to an
  empty omitted map and `material_remaps` retains its serialized meaning.
- Reimport preserves overrides while deterministic sub-asset IDs remain
  stable. Orphaned mappings remain harmless if a source removes a sub-asset,
  following ADR 0101.
- Mesh, Animation, Skeleton, Skin, Morph, and RigidBodyRig replacement remains
  unchanged. Animation continues to use Animation Sets; geometry and rig data
  require dedicated compatibility contracts before model-level replacement is
  safe.

## Alternatives Considered

### Assign every entity or save a customized prefab

This remains valid for instance-specific changes, but does not solve the
asset-level intent and obscures which imported slot has been standardized for
all instances.

### Allow any imported sub-asset as an override target

Rejected for the authoring UI. It permits remap chains and cycles and makes
ownership harder to understand. Extracting or duplicating to a standalone
asset first provides a clear target with an independent lifecycle.

### Generalize the same map to every imported sub-asset kind

Rejected for now. A Material or Texture replacement has a simple kind
contract. Meshes require vertex/layout compatibility, Skins and Skeletons
require rig compatibility, Morphs require target geometry compatibility, and
RigidBodyRigs require bone/body compatibility. A single unchecked pointer map
would make these failures occur late in rendering or animation.

## Compatibility and Migration

- `texture_remaps` is optional, defaults to empty, and is omitted during
  serialization when unused.
- No Stable ID derivation, Scene/Prefab schema, or existing command semantics
  change.
- Existing `material_remaps` entries immediately appear in the generalized UI
  and continue to resolve exactly as before.
