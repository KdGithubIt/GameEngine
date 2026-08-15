# OSS Pre-Public Feature Plan

Status: Draft
Scope: Work that should be decided or completed before the first public
Apache-2.0 release.

This plan is intentionally smaller than the full engine roadmap. The goal is
not to add every planned engine feature before publication. The goal is to make
the first public user experience coherent, testable, and honest about current
limitations.

## Release Target

The first public release should be a source-first `v0.1.0` release.

Policy decisions:

- CLA: none.
- DCO: required.
- License: MIT OR Apache-2.0, leaning Apache-2.0 when policy is ambiguous.
- Rust API stability: breaking changes are allowed before `1.0`.
- Scene/project schema stability: best-effort compatibility.
- Publication flow: private staging repository -> scan/CI/tag -> public.

It should guarantee:

- The workspace builds and tests from a clean checkout.
- The editor launches.
- One official sample project opens in the editor and runs in Play mode.
- One standalone example runs with `cargo run --example ...`.
- Known limitations are documented instead of hidden.

It does not need to guarantee:

- Crates.io publication.
- Binary installers.
- Complete packaging/export flow.
- Complete WASM support.
- Full commercial-engine feature parity.

## P0 Workstream 1: Official Sample Path

Goal: Provide one reliable first-run experience.

Recommended decision:

- Make `examples/tiny_goal_project` the official editor-openable sample.
- Keep `crates/engine/examples/sample_game.rs` as the official standalone
  Rust example if it remains reliable.
- Treat `spire_lite.rs` as experimental until it has explicit acceptance
  criteria and public documentation.

Tasks:

1. Choose the official editor sample.
2. Ensure the sample project is tracked in the new public repository.
3. Verify it has no private, generated, or license-unclear assets.
4. Confirm the sample has:
   - `project.json`
   - `asset_manifest.json`
   - one start scene under `assets/scenes/`
   - only small, inspectable assets
5. Add a README section that explains:
   - how to launch the editor
   - how to open the sample project
   - how to enter Play mode
   - expected controls
   - expected success condition
6. Add a lightweight acceptance test or documented manual check:
   - editor opens the sample
   - Play starts without blocking diagnostics
   - player movement works
   - sample-specific goal state is installed

Completion criteria:

- `cargo test --workspace` includes coverage for starting Play from the sample.
- A clean clone can run the documented sample flow.
- The README names exactly one official editor sample for first-time users.

Public promise:

- "This repository includes one small editor-openable sample project that can
  be opened and played from the editor."

Do not promise:

- A full template system.
- A polished game.
- Stable sample asset formats beyond documented schema versions.

## P0 Workstream 2: Transform Hierarchy Decision

Status: Resolved 2026-07-04. Option A was implemented. Runtime parent-child
transform propagation lives in `crates/engine/src/transform.rs`
(`transform_propagation_system`), and `spawn_from_authoring_scene` attaches
`Parent`/`Children` components from authoring parent references. Missing
parents are treated as roots, runtime cycles fall back to local transforms
without panicking, and resolution is deterministic. Covered by unit tests in
`transform.rs` and `scene_bridge.rs`.

Goal: Avoid a misleading editor hierarchy experience.

Current risk:

- Authoring entities support parent relationships.
- The editor exposes scene hierarchy concepts.
- Runtime transform propagation currently writes local transforms directly to
  global transforms.
- Users may expect child entities to follow parent transforms.

Decision options:

### Option A: Implement Minimal Runtime Propagation

Implement parent-child transform propagation for runtime entities spawned from
authoring scenes.

Minimum behavior:

- Root entity global transform equals local transform.
- Child global transform equals parent global transform multiplied by local
  transform.
- Missing or invalid parent references produce diagnostics instead of panic.
- Cycles remain blocked by authoring validation.
- Propagation order is deterministic.

Tests:

- Root transform propagates to global transform.
- Child follows translated parent.
- Multi-level hierarchy propagates correctly.
- Reparented authoring scene spawns with correct runtime parent relationship,
  if reparenting is supported by the public editor flow.
- Invalid parent state does not panic.

Completion criteria:

- Parent-child scene transforms behave as users expect during Play mode.
- README does not need to warn that hierarchy is visual-only.

### Option B: Publicly Defer Runtime Propagation

Keep current runtime behavior but explicitly mark hierarchy transform
propagation as unsupported in `v0.1.0`.

Required documentation:

- README known limitations.
- Editor docs or sample docs.
- Roadmap issue for hierarchy transform propagation.

Required UX guardrail:

- Avoid presenting hierarchy as a runtime transform feature in first-run docs.
- Do not use parent-dependent transforms in official samples.

Completion criteria:

- No official sample depends on parent transform propagation.
- Known limitation is visible before users hit it.

Recommended decision:

- Prefer Option A if the implementation stays small and contained.
- Choose Option B only if publication speed is more important than hierarchy
  correctness for `v0.1.0`.

## P0 Workstream 3: Build / Packaging Scope

Goal: Decide whether packaging is a supported public feature in `v0.1.0`.

Current risk:

- Build analysis exists.
- Asset reachability is conservative.
- Missing asset diagnostics are non-blocking in the current v1 analysis.
- Actual package layout and runnable output need end-to-end verification before
  being advertised.

Decision options:

### Option A: Exclude Packaging From v0.1.0 Public Promise

Recommended for first publication.

Tasks:

1. Mark packaging/export as experimental or roadmap-only.
2. Do not advertise package generation in README quickstart.
3. Keep build analysis internal to the editor until end-to-end packaging is
   verified.
4. Create a roadmap issue for packaging.

Completion criteria:

- README only promises source builds and examples.
- Packaging limitations are documented.
- No public release note implies production-ready export.

### Option B: Make Minimal Packaging Supported

Tasks:

1. Define the package layout:
   - executable
   - `assets/`
   - config/start scene
   - license files
2. Make missing required assets blocking.
3. Verify a sample project produces a runnable desktop package.
4. Add tests for package planning.
5. Add a manual release checklist for package execution.

Completion criteria:

- One sample project packages and runs from the output directory.
- Missing start scene blocks packaging.
- Missing required asset blocks packaging.
- README includes exact packaging commands and limitations.

Recommended decision:

- Use Option A for `v0.1.0`.
- Move Option B to a post-public milestone unless packaging is central to the
  launch message.

## P1 Workstream 4: Public CLI Shape

Goal: Give users a non-GUI way to validate project files.

Current CLI shape:

- Behavior Tree commands are available.
- AI agent bridge commands are available.
- There is no obvious public command for validating a scene or project.

Recommended minimal additions before or shortly after publication:

- `engine-cli scene validate <scene.json>`
- `engine-cli project validate <project_root>`
- `engine-cli project assets <project_root>`

Minimum behavior:

- Commands return deterministic JSON.
- Exit code `0` means no blocking diagnostics.
- Exit code `2` means invalid input or blocking diagnostics.
- Output includes stable diagnostic codes.

Tests:

- Valid scene returns success.
- Malformed scene returns input error.
- Unsupported schema version returns input error.
- Missing asset manifest is reported according to the public policy.
- Missing referenced asset is reported deterministically.

Completion criteria:

- README can document CLI validation without requiring the editor.
- CI can call at least one validation command against the official sample.

Release positioning:

- This is useful but not strictly required for the first public source release.
- If omitted from `v0.1.0`, document it as a near-term roadmap item.

## P1 Workstream 5: Example Matrix

Goal: Stop every example from becoming an accidental public guarantee.

Recommended classification:

| Example | Public status | Required before v0.1.0 |
| --- | --- | --- |
| `hello_window` | Smoke test | Must compile; README may mention as minimal rendering check |
| `minimal_playable` | Basic gameplay example | Must compile; fix visible mojibake before documenting |
| `sample_game` | Standalone showcase | Document only if manually verified |
| `spire_lite` | Experimental | Do not document as supported until acceptance criteria exist |

Tasks:

1. Classify every example as supported, smoke test, or experimental.
2. Document only supported examples in README quickstart.
3. Add a table of experimental examples if useful.
4. Fix visible user-facing mojibake in supported examples.
5. Add manual verification steps for graphics examples.

Completion criteria:

- Users know which example to run first.
- Experimental examples do not define the quality bar for the public release.

## P1 Workstream 6: First-Run Editor Experience

Goal: Reduce confusion when the editor starts for the first time.

Minimum acceptable state:

- README gives exact instructions to open the official sample.
- Editor can open a project and start Play mode.
- Problems/diagnostics are visible when Play cannot start.

Better state:

- The project hub exposes a clear "Open Project" flow.
- The official sample path is documented, not hard-coded to a local machine.
- Unsaved-change dialogs and reload behavior are covered by tests.

Completion criteria:

- A contributor can follow the README without prior private context.
- First-run editor behavior does not depend on local files outside the public
  repository.

## P0 Documentation Work

These are not engine features, but they block a credible public release.

Tasks:

1. Fix or remove mojibake in user-facing docs and supported examples.
2. Add known limitations:
   - packaging/export status
   - hierarchy transform status
   - WASM status
   - supported asset formats
   - editor maturity
3. Add a quickstart that uses only public files.
4. Add a "What is stable in v0.1.0" section.
5. Add a "What is experimental" section.

Completion criteria:

- No public quickstart references private paths or internal-only phase names.
- Known limitations are explicit and accurate.

## Suggested Milestones

### Milestone A: Public Minimum

Target: Source-first public repository can be opened safely.

Included:

- Official editor sample selected and documented.
- Sample Play mode verified.
- Transform hierarchy decision made.
- Packaging public scope decided.
- User-facing mojibake removed from supported paths.
- `cargo test --workspace` passes.

Exit criteria:

- A new contributor can run the official sample from README.
- No unsupported feature is advertised as complete.

### Milestone B: Developer Usability

Target: Public contributors can validate changes without the editor.

Included:

- Scene/project validation CLI, or a documented reason it is deferred.
- Example support matrix.
- CI checks against the official sample where possible.

Exit criteria:

- CI and README share the same validation commands.

### Milestone C: Public v0.1.0 Release

Target: The project can be made public under Apache-2.0.

Included:

- Public minimum is complete.
- License and repository hygiene work from `OSS_PUBLICATION_PLAN.md` is
  complete.
- README, CONTRIBUTING, SECURITY, and CI are present.
- First release notes list supported and experimental features.

Exit criteria:

- Clean clone builds, tests, launches editor, and runs the official sample.

## Recommended Final Cut for v0.1.0

Ship:

- Editor-openable Tiny Goal sample.
- Standalone smoke/example path.
- Existing engine/editor/authoring/CLI crates.
- Behavior Tree CLI and AI agent bridge as current advanced features.
- Explicit known limitations.

Do not ship as supported yet:

- Production packaging/export.
- Crates.io publication.
- Binary installer.
- Experimental examples without documentation.
- Any feature that requires private context to use.
