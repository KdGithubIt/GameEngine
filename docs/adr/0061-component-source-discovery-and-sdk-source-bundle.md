# ADR 0061: Component Source Discovery and SDK Source Bundle

Status: Accepted

Project-source discovery in this record is superseded by ADR 0066. The SDK
source bundle and explicit external-editor decisions remain active.

Date: 2026-07-19

## Context

Inspector component identity is a persisted stable component ID, while Rust
file names and type names are mutable. Project source is editable, but the
engine implementation must remain read-only in generated projects and packaged
editors cannot rely on an engine workspace checkout.

## Decision

The editor indexes `game/src/**/*.rs` by explicit
`#[game_component(id = "...")]` declarations. The editor-local index stores the
stable ID, `game/src`-relative path, Rust type, and one-based declaration line.
It is never serialized into scenes, prefabs, assets, or the GameModule ABI.
Duplicate or malformed declarations are Problems diagnostics; an ambiguous ID
cannot be opened.

Built-in IDs resolve to a version-matched SDK-relative source path. An internal
read-only viewer loads that bundle and labels it engine-owned. Development
builds may use the workspace as the bundle root. Packaged builds use
`GAMEENGINE_SDK_SOURCE_ROOT` or their installed SDK bundle. A missing bundle
does not disable component editing and instead exposes the expected version and
a repair action.

External programs launch only from an explicit user action using editor-local
executable and argument-template preferences. Script generation never invokes
an OS association automatically.

## Consequences

Source navigation survives file and type renames when the stable ID remains
unchanged. Machine-specific paths do not enter authoring data. Engine source is
inspectable without creating an unsafe project component with a colliding ID.
