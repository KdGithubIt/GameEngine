# GameEngine Patched Goose Build

This directory defines the temporary, reproducible Goose ACP build used by GameEngine while upstream mid-turn compaction fixes remain unmerged/unreleased.

The build is not a Goose fork. It starts from one exact upstream release commit and applies the ordered patch series in `patches/`. The static source provenance is in `provenance.json`; the Windows workflow emits a separate build provenance document containing the workflow run identity and artifact hashes.

## Baseline

- upstream repository: `https://github.com/aaif-goose/goose`
- upstream release: `v1.45.0`
- exact upstream commit: `4dc0420f5704a92806c6628c8f0a3497d7a88759`
- GameEngine patch revision: `gameengine-p1`

The baseline is the current stable release as of 2026-08-22. PR #11075 is still Draft/unmerged and the mid-turn compaction change is not in a stable Goose release.

## Patch policy

Only the responsibilities needed by GameEngine Managed Local are carried here:

1. structured context-overflow 400 classification;
2. compaction-summary output-limit recovery;
3. preservation of streamed output-limit metadata;
4. legacy-agent mid-turn proactive compaction after a completed tool result;
5. regression coverage for the safe boundary, tool-pair preservation, same-turn continuation, reduced retained context, truncated summaries, and structured overflow behavior.

The provider-declared limit precedence and desktop token-bar/model-picker changes from upstream PR #11075 are intentionally excluded. GameEngine already owns the managed physical context contract and exports the same 32K context / 4096 output budget through llama-server, the custom provider document, and Goose environment variables.

## Build contract

`.github/workflows/gameengine-goose-patched-build.yml` verifies the exact upstream commit, applies every patch with `git apply --check --whitespace=error-all`, runs Rust validation, builds Windows `goose.exe`, probes the patched runtime identity, creates a ZIP, calculates SHA-256 for the executable and ZIP, and uploads build provenance.

Pull requests can exercise the build with read-only repository permissions. Publishing an immutable GitHub Release asset is a separate explicit `workflow_dispatch` option and is guarded so an existing release/tag is never overwritten.

The patched runtime identifies itself as `1.45.0-gameengine-p1` through the CLI version string while retaining the upstream Cargo dependency graph and lockfile.
