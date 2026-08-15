# Phase 30 — Console / Problems / Validation

## Goal

Present structured engine and authoring diagnostics in a Console panel and a
Problems panel, with severity filtering and navigation to the source entity,
asset, or graph node.

## Why

Diagnostics currently appear in an unstructured list.  As scene complexity
grows, designers need to see warnings and errors grouped by severity, identify
which asset or entity triggered them, and navigate directly to the source
without manually scanning the Hierarchy.

## Scope

| Item | Location |
|------|----------|
| Console panel: severity / code / message / timestamp | `crates/editor/src/console.rs` (new) |
| Problems panel: persistent errors grouped by code | `crates/editor/src/problems.rs` (new) |
| Severity filter (Error / Warning / Info) | each panel |
| "Navigate to source" for entity / asset / graph node | panel navigation handler |
| Project validation: missing asset, invalid reference, no camera | `crates/authoring/src/validation.rs` |
| Trigger validation on project open and before Play | `crates/editor/src/session.rs` |

## Key Constraints

- Diagnostic codes are stable string identifiers (existing `Diagnostic.code`
  field); no new code format is introduced.
- "Navigate to source" must not panic if the target has since been deleted.
- Project validation runs on `AuthoringScene`; it does not start the runtime
  or touch GPU resources.

## Completion Criteria

- Console and Problems display structured diagnostics filtered by severity /
  code / target.
- Clicking a diagnostic navigates to the scene entity, asset, or graph node.
- Project validation detects missing asset, invalid reference, and no camera.

## Feeds Into

Phase 31 (Asset Database v2 — needs the missing/orphan diagnostic pipeline),
Phase 34 (Project Settings — validation of missing Start Scene).
