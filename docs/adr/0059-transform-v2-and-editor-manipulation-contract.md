# ADR 0059: Transform v2 and Editor Manipulation Contract

Status: Accepted

## Context

The built-in `engine.transform` authoring component only stored translation.
The runtime transform already supports rotation and scale, while the editor
advertised Rotate and Scale tools that could not commit either value. Existing
scene documents must continue to load without a migration-only rewrite.

## Decision

1. `engine.transform` schema version 2 retains the `x`, `y`, and `z` translation
   fields and adds Euler rotation fields `rotation_x_degrees`,
   `rotation_y_degrees`, and `rotation_z_degrees`, plus `scale_x`, `scale_y`,
   and `scale_z`.
2. Missing rotation fields mean zero degrees and missing scale fields mean one.
   This makes every schema-v1 scene valid input to the schema-v2 runtime bridge.
3. The runtime bridge converts authored XYZ Euler degrees to its quaternion
   representation. Authoring data remains human-readable; runtime quaternion
   details do not leak into the serialized component contract.
4. The editor exposes Position, Rotation, and Scale groups and all three gizmo
   modes commit one authoring transaction per completed drag.
5. Camera orbit and pan consume per-frame pointer movement. The Scene View
   provides explicit Focus Selection and Reset Camera operations.

## Consequences

- Existing scenes keep their current appearance with identity rotation and
  unit scale.
- New and edited scenes can express the complete runtime transform without a
  separate component.
- Scale values are clamped away from zero by editor gizmos to avoid producing
  singular transform matrices; direct authoring remains validated separately.
- Undo and redo treat a complete gizmo drag as one user action.

## Compatibility

The scene document schema remains version 1. The component schema change is
additive and uses runtime defaults for absent fields, so existing serialized
documents and tools that only understand translation remain readable.
