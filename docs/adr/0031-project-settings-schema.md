# ADR 0031: Project Settings Schema

Status: Accepted
Date: 2026-06-14

## Context

Phase 34 adds project-wide settings as a versioned document.  The settings
cover:

- **Tags**: named strings that can be attached to entities for filtering.
- **Layers**: named render / collision layer slots (up to 32).
- **Input Actions**: named logical actions mapped to one or more key codes
  (implements Phase 12-D deferred Action Mapping).
- **Start Scene**: the `*.scene.json` asset loaded when Play or Build starts.
- **System Settings**: Update and FixedUpdate stable-ID order and disabled
  entries, as defined by ADR 0051.

The `PlayerController` currently reads `Input<KeyCode>` directly; remapping
requires code changes.  This ADR defines the binding data model, persistence
format, defaults, and runtime lookup contract so that implementation can begin.

## Decision

### File layout

Project-wide settings are stored in `project_settings.json` at the project
root (alongside `project.json`).  The file is optional; if absent, all
settings use defaults.

```json
{
  "schema_version": 1,
  "tags": ["Player", "Enemy", "Pickup"],
  "layers": [
    { "index": 0, "name": "Default" },
    { "index": 1, "name": "UI" }
  ],
  "input_actions": [
    {
      "name": "move_forward",
      "keys": ["KeyW", "ArrowUp"]
    },
    {
      "name": "move_back",
      "keys": ["KeyS", "ArrowDown"]
    },
    {
      "name": "move_left",
      "keys": ["KeyA", "ArrowLeft"]
    },
    {
      "name": "move_right",
      "keys": ["KeyD", "ArrowRight"]
    }
  ],
  "start_scene": "scenes/main.scene.json"
}
```

### Runtime lookup contract

At runtime, an `InputActionMap` resource is inserted into the ECS world.
`PlayerController` queries this resource to resolve logical action names
(e.g., `"move_forward"`) to keyboard, gamepad-button, and gamepad-axis state.
Editor Play and packaged Player compile the same `ProjectSettings` data through
the same function. ABI v3 project systems receive only actions declared in
their access manifest. When `InputActionMap` is absent, `PlayerController`
falls back to legacy hardcoded keys so existing code-only examples still work.

### Validation

- Start scene references are validated via Phase 30 diagnostics: if
  `start_scene` is set to a path that does not exist in `assets/`, the
  Problems panel shows a `validation.start_scene_not_found` error.
- Unknown key strings and gamepad indices produce non-blocking diagnostics and
  are ignored individually. Duplicate action names keep the first definition
  and report the later definition.

### Defaults

| Setting | Default when absent |
|---------|-------------------|
| tags | empty |
| layers | `[{index:0, name:"Default"}]` |
| input_actions | built-in WASD bindings (see below) |
| start_scene | null (Play opens the currently loaded scene) |

Built-in defaults for input actions (used when file is absent):

| Action | Keys |
|--------|------|
| `move_forward` | `KeyW` |
| `move_back` | `KeyS` |
| `move_left` | `KeyA` |
| `move_right` | `KeyD` |

## Consequences

- Phase 12-D is now fully implemented: `PlayerController` uses
  `InputActionMap` and the built-in WASD fallback keeps existing behaviour
  when no project settings file exists.
- The `start_scene` field is shared by Play, Build (Phase 39), and
  Validation (Phase 30 Problems panel).
- Tags and Layers are defined here but their use by entity components is
  left to a later phase.

## Alternatives Considered

### Merge settings into `project.json`

Rejected. `project.json` is already established by ADR 0023 as the minimal
project identity file.  Adding Tags, Layers, Input Actions, and Start Scene
would bloat it and couple unrelated concerns.  A separate file allows
independent versioning.

### Use a TOML or YAML format

Rejected. The project already uses JSON for all persisted documents.
Introducing a second format increases tooling complexity for no material
benefit at this project scale.

## Compatibility and Migration

`project_settings.json` is a new file; existing projects without it work
normally (all defaults apply).  The `schema_version` field ensures a future
version 2 can migrate version 1 files gracefully.

ADR 0051 adds an optional `system_settings` field. Existing version 1 files
deserialize it as empty preferences, while the nested scheduling document has
its own schema version for independent migration.
