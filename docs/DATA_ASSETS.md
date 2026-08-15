# Data Assets

Data assets are reusable project values stored outside scenes and prefabs. They
are the GameEngine equivalent of a simple Unity `ScriptableObject` workflow.

## Create and edit

1. Open a project and expand **Data Assets** in the Inspector.
2. Enter a display name and choose **New Data Asset**.
3. Add Bool, Integer, Number, Text, Vec2, or Vec3 fields.
4. Edit field values directly in the same Inspector section.

The editor creates `assets/data/<name>.data.json` and registers it in
`asset_manifest.json` with a stable `AssetId`.

## Reference from a GameComponent

Use `engine::data_asset::DataAssetRef` as an explicitly authored component field:

```rust
use engine::{data_asset::DataAssetRef, GameComponent};

#[derive(Debug, Clone, Default, GameComponent)]
#[game_component(display_name = "Enemy Config", category = "Gameplay")]
pub struct EnemyConfig {
    #[game_field]
    pub stats: DataAssetRef,
}
```

After building the project Game module, add **Enemy Config** through the normal
**Add Component** menu. The selected entity's Inspector shows a **Data Asset
References** section where `stats` can be assigned, changed, or cleared.

`DataAssetRef::default()` is intentionally unassigned, so new components do not
need a fake placeholder ID.

## Resolve values

A reference exposes its stable ID through `DataAssetRef::asset_id`. Code that has
an `AssetManifest` and assets root can load the document explicitly:

```rust
let document = reference.load(&manifest, &assets_root)?;
if let Some(document) = document {
    let health = document.fields.get("health");
}
```

Runtime process-local `Handle<T>` values are deliberately not persisted in scene
or component data.
