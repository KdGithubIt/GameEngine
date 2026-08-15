# ADR 0034 — Build / Packaging Strategy

## Status: Accepted

## Context

Phase 39 generates a runnable desktop package from an editor project.  ADR 0028
§Decision 6 requires an ADR before touching the CLI / MCP surface or the
runtime asset-loading contract.

## Decision

### Analysis step (pure Rust, no subprocess)

`analyze_build(config: &BuildConfig, manifest: &AssetManifest) -> BuildReport`
performs a reachability analysis:

1. If `config.start_scene` is `None`, emit a blocking `MissingStartScene`
   diagnostic and return early.
2. **v1 conservative policy**: all manifest entries are considered reachable.
   Fine-grained scene-file parsing (to exclude unused assets) is deferred.
3. Manifest entries whose `path` field does not resolve to a file on disk
   produce a `MissingAsset` diagnostic (non-blocking in v1).

`analyze_build` is pure — it performs no I/O, no subprocess calls, and is
fully unit-testable.

### Build invocation (separate function)

`build_project(config: &BuildConfig) -> Result<BuildReport, BuildError>`
calls `std::process::Command::new("cargo").args(["build", "--release", ...])`.
This function is intentionally NOT unit-tested; it is exercised manually.

### Package layout

```
<output_dir>/
  game               (or game.exe on Windows)
  assets/
    <all reachable asset files, directory structure preserved>
```

### Out of scope in v1

- WASM packaging.
- Fine-grained dead-asset elimination.
- Asset embedding into the executable binary.

## Consequences

- Unit tests cover `analyze_build` only; the `build_project` function requires
  a full Rust toolchain and is not tested in CI by this phase.
- The v1 conservative policy may include unused assets in the package, which
  is safe but not optimal.
- The asset-loading contract in `crates/engine` does not change.
