# ADR 0115: Current-Format-Only Baseline

Status: Accepted
Date: 2026-08-15
Amends: ADR 0020, ADR 0031, ADR 0066, ADR 0091, ADR 0092, ADR 0099

## Context

GameEngine is still under active development and does not need to preserve old
project-file, authoring-API, imported-sub-asset, or generated-host compatibility
solely so earlier engine revisions keep loading or compiling unchanged content.
Those compatibility paths increase the number of accepted states, keep obsolete
IDs and API names alive, and force tests to protect behavior that is no longer
part of the intended product.

The goal is not to replace the engine architecture. The current crate
boundaries, authoring/runtime split, stable-ID model, editor workflow, public
`engine` facade, and accepted domain responsibilities remain authoritative.
This decision only narrows those systems to one current data and behavior
contract.

## Decision

### 1. Versioned documents accept exactly the current schema

A persisted document with a `schema_version` accepts only the version declared
current by that document type. A missing, lower, or higher version is rejected.
Readers MUST NOT infer an old version from a missing field.

When the current writer always emits a field, a deserialization default that
exists only to load an older writer's output is compatibility code and MUST be
removed. Tests that assert that old representation loads successfully are
removed or replaced by rejection tests.

This amends the version-1 exception retained by ADR 0091 section 3 and the
missing-scene-version behavior originally allowed by ADR 0020. It also
supersedes the compatibility sentence in `AI_FRIENDLY_AUTHORING_SPEC.md` §7.2
that allowed entities predating `display_name` and `description` to deserialize
with empty-string defaults. Those fields are part of the current canonical
entity shape and are required even when their values are empty.

### 2. Current optionality remains current behavior

This cleanup does not turn every optional field into a required field.
`Option`, empty collections, or omitted default values that the current
canonical writer intentionally emits as absent remain part of the current
format. They are not compatibility paths merely because deserialization uses a
default.

The deciding question is whether the omission is produced by the current
writer and belongs to the current product contract, not whether serde happens
to use `default`.

### 3. Compatibility-only aliases and migration paths are removed

When a current API, identifier, serialized field, generated layout, or persisted
identity format has replaced an older one, the older form is removed instead of
being maintained indefinitely. Callers and fixtures inside GameEngine are
migrated to the current form in the same change.

For VMD authoring this means `motion_model_sources` is the only model-target
field. The pre-ADR-0099 singular `motion_model_source` fallback and the hidden
pre-ADR-0099 animation sub-asset ID alias are not retained.

For Animation Graph authoring, typed parameters and explicit transition
conditions are the current contract. Compatibility-only Bool API aliases and
legacy bare-condition parsing are not part of the baseline.

For project Rust components, sidecar identity uses the current opaque
`game.c_<lowercase ULID>` form. Older dotted `game.*` component IDs are not
accepted merely to preserve historical scene or prefab references. This amends
the migration compatibility retained by ADR 0066.

For the project Rust build host, the unified module tree from ADR 0092 is the
only supported layout. Existing hosts using the older per-category bridges,
legacy root declarations, or generated `mod.rs` indexes are not silently
rewritten during initialization. They must be brought to the current host shape
before normal initialization/index refresh succeeds. This supersedes ADR 0092's
Compatibility section while keeping its unified-tree architecture unchanged.

### 4. Current crate architecture and public facade remain intact

This cleanup does not undo ADR 0113's runtime-domain decomposition or its
current `engine` public facade. Public `engine::*` re-exports that intentionally
form the supported umbrella API remain part of the current architecture even
when their implementation lives in an owning domain crate.

Compatibility-only hidden namespaces inside extracted crates may be removed
when they exist solely to let pre-extraction source keep referring to a former
module path. Internal GameEngine callers should use the owning crate directly
when doing so preserves the dependency DAG. This distinction prevents an old
migration shim from becoming permanent without turning this format cleanup into
a separate public-API redesign.

### 5. Tests protect only the current contract

Tests MUST continue to cover current serialization, validation, stable IDs,
runtime semantics, and intentional current defaults. Tests whose only purpose
is proving that an obsolete representation or alias still works are removed.

Negative tests may remain when they prove obsolete input is rejected rather
than silently interpreted as current data.

## Consequences

Current-format readers and initializers have a narrower input surface. Stale
authoring representations are rejected instead of silently translated, while
the current runtime crate architecture and supported `engine` umbrella API stay
unchanged. Projects that still use removed persisted representations or generated
host layouts must be updated to the current canonical contract before using this
engine revision.
