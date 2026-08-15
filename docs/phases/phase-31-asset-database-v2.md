# Phase 31 — Asset Database v2 / Import Settings

## Goal

Extend asset records to carry import settings and preview state.  Decide and
document via ADR whether the existing manifest (ADR 0021) is extended or
`.meta` sidecar files are introduced.

## Why

The Phase 14/16 manifest records only `AssetId → relative path`.  Import
settings (texture compression, mesh LOD targets, audio format) and reimport
tracking require richer per-asset metadata.  The data model affects serialized
project data and cannot be changed without an ADR.

## Scope

Exact scope depends on the ADR.  Likely items:

| Item | Decision point |
|------|---------------|
| Manifest extension vs. `.meta` sidecar strategy | **ADR required before any implementation** |
| Import settings fields (initial set) | Defined in the ADR |
| Reimport / missing / orphan diagnostics in project validation | `crates/authoring/src/validation.rs` |
| Preview state persistence | Editor preference or asset record (decided in ADR) |

## Key Constraints

- **An ADR is required before any implementation.**  The choice between
  manifest extension and `.meta` files affects serialized project data and
  must be frozen before code is written (ADR 0028 §Decision 6).
- Existing `AssetId → path` mapping from ADR 0021 must remain valid after
  migration; a compatibility story is required in the ADR.

## Completion Criteria

- ADR is Accepted and implementation follows it exactly.
- Asset records carry import settings and preview state.
- Reimport / missing / orphan diagnostics appear in project validation (Phase 30).

## Feeds Into

Phase 32 (Drag & Drop — needs stable asset metadata),
Phase 35 (Material Editor — needs texture import settings),
Phase 36 (glTF import — needs deterministic sub-asset IDs and the import
settings contract).
