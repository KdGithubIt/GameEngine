# ADR 0067: Unified Script Assets and Generated Cargo Host

- Status: Accepted (the legacy source migration is removed by ADR 0091; the
  fixed script categories are superseded by ADR 0092)
- Date: 2026-07-19

> ADR 0091 removes the compatibility path described here. Rhai
> sources are recognized only under `assets/scripts/rhai`, and the
> one-time migration of `game/src/{components,resources,systems}` into
> the asset tree is gone.
>
> ADR 0092 replaces the four fixed Rust categories and their per-category
> generated bridges with one free-form module tree below
> `assets/scripts/rust/`. Folders are recommendations, folder structure is the
> Rust module path, and a source's kind comes from its declarations.

## Context

The editor previously presented runtime assets below `assets/` and native Rust
gameplay sources below `game/src/` as different physical roots. That made the
Asset Browser incomplete and made a path shown by the editor differ from the
path users found on disk. Rust also cannot safely support arbitrary moves: a
move may change a module path, component identity requires an adjacent
sidecar, and ordinary modules have explicit `mod` relationships.

## Decision

User-authored scripts use this physical layout and appear in the same Asset
Browser tree as scenes, textures, meshes, and other assets:

```text
assets/scripts/
├─ rhai/**/*.rhai
└─ rust/
   ├─ components/**/*.rs
   ├─ resources/**/*.rs
   ├─ systems/**/*.rs
   └─ shared/**/*.rs
```

`game/` remains an internal standalone Cargo workspace. Its `src` tree contains
only generated module bridges and crate integration. The editor regenerates
those bridges from the physical asset sources before Cargo builds. Rust source
files and component sidecars are source assets, not runtime manifest rows, and
are not copied by ordinary runtime-asset packaging.

Creation and relocation are category constrained:

- `scripts/rhai` accepts Rhai scripts.
- `rust/components`, `rust/resources`, and `rust/systems` accept their matching
  generated Rust kind.
- Files and folders in those four categories may move only within the same
  category.
- Component moves carry the adjacent `.rs.meta.json` sidecar atomically, so the
  stable `game.c_<ULID>` identity is unchanged.
- `rust/shared` contains ordinary Rust modules. Generic Asset Browser moves are
  rejected because a filesystem move alone cannot safely rewrite arbitrary
  Rust module relationships.
- Regular non-code assets retain their existing free folder movement.

The editor keeps legacy Rhai files directly below `assets/scripts` visible,
but new creation targets `assets/scripts/rhai`, and a legacy Rhai file may move
only into that canonical category. Opening or initializing a legacy project
migrates user source files from `game/src/components`, `resources`, and
`systems` into their matching asset categories without changing component
sidecars. A destination collision stops migration rather than overwriting.

## Consequences

- The Asset Browser matches the physical project tree and is the single
  authoring surface for both code and ordinary assets.
- Cargo and rust-analyzer still have a conventional internal crate host.
- Generated bridge files may contain machine-local absolute `#[path]` values;
  they are internal derived data and are regenerated on project open or source
  mutation.
- Rust diagnostics may point either at asset sources or generated host files;
  navigation accepts only those two project-owned roots.
- ADR 0061's `game/src` user-source discovery location is superseded. ADR 0066
  component identity and sidecar rules remain in force at the new source root.

