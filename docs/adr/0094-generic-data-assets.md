# ADR 0094 — Generic Data Assets

- Status: Accepted
- Date: 2026-07-28

## Context

Reusable gameplay configuration currently has no author-owned equivalent to a
Unity `ScriptableObject`. Runtime `Assets<T>` values are process-local and their
handles must not be persisted, while prefabs represent entity hierarchies rather
than standalone data.

The project needs reusable values that:

- live outside scenes and prefabs;
- retain stable manifest identity across rename and move operations;
- can be referenced from project `GameComponent` fields;
- can be assigned and edited without exposing raw stable IDs in the Inspector;
- do not change the native Game Module ABI.

## Decision

### 1. Persist generic data as `*.data.json`

A data asset is a versioned `DataAssetDocument` containing a display name and a
deterministically ordered map of author-owned `Value` fields. Version 1 supports
the existing authoring value model and rejects invalid field identifiers and
non-finite floating-point values.

Each file is registered in `asset_manifest.json`. Scene and component data store
the stable `AssetId`, never a filesystem path or runtime `Handle<T>`.

### 2. Represent component references with `DataAssetRef`

`DataAssetRef` is an optional stable reference that implements the existing
`GameField` contract. It serializes as a marked authoring object:

```json
{
  "$type": "data_asset_ref",
  "asset": null
}
```

An assigned value replaces `null` with the normal tagged `asset_ref` value. The
wrapper uses the existing `FieldType::Object`, so no Game Module ABI revision is
required. A default-constructed project component can remain unassigned.

### 3. Provide a dedicated Inspector surface

The Inspector owns data-asset creation, field editing, and reference assignment.
It discovers marked `DataAssetRef` values recursively inside selected entity
components and replaces the complete reference object through the existing
reversible authoring command path.

The generic component value remains valid authoring data even when the editor is
not present. The dedicated Inspector is presentation and workflow logic only.

### 4. Keep runtime resolution explicit

`DataAssetRef` exposes its optional stable ID. `DataAssetDocument::load_registered`
resolves that ID through an `AssetManifest` and assets root. This keeps authoring
identity separate from process-local runtime handles and avoids hidden filesystem
access in project Game System invocation.

## Consequences

- Project components can declare `DataAssetRef` fields without a custom macro
  attribute or ABI extension.
- New data assets can be created and edited from the Inspector.
- The selected entity exposes filtered assignment controls for every nested
  `DataAssetRef` field.
- Data assets are manifest-owned but version 1 does not add a dedicated Asset
  Browser tile category; the Inspector is the authoritative authoring surface.
- Runtime systems that need resolved documents must receive the manifest and
  assets root through an appropriate host/service boundary rather than assuming
  an editor path.
