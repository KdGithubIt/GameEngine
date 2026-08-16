# Editor Visual Validation

Status: Accepted  
Version: 1.0.0  
Canonical location: `docs/EDITOR_VISUAL_VALIDATION.md`

## Purpose

GameEngine desktop visual validation produces screenshot evidence for changes
whose correctness cannot be established by compiler, Clippy, tests, or rustdoc
alone. It is deliberately opt-in: ordinary pull requests continue to use the
normal Windows validation path without launching the Launcher or Editor.

This workflow supplements `affected`, `full`, and `docs` validation. It does
not replace any command required by `docs/DEVELOPMENT_WORKFLOW.md`, and a visual
capture result MUST NOT be reported as a Rust validation result.

## Requesting visual validation

A same-repository `chatgpt/gameengine-*` pull request targeting `main` requests
visual validation by containing exactly one of these hidden markers in its pull
request body:

```text
<!-- gameengine-visual-validation: auto -->
<!-- gameengine-visual-validation: editor -->
<!-- gameengine-visual-validation: launcher -->
<!-- gameengine-visual-validation: both -->
```

Omitting the marker is the normal case and does not schedule the Windows visual
capture job. ChatGPT SHOULD add the `auto` marker only when a change affects
human-visible desktop UI and SHOULD omit it for logic-only, documentation-only,
or otherwise non-visual changes.

`auto` maps changes under `crates/editor/**` to Editor capture, changes under
`crates/launcher/**` to Launcher capture, and changes touching both to both.
When the requested change is visual but cannot be classified to either desktop
crate, `auto` captures both rather than silently producing no evidence.

The explicit `editor`, `launcher`, and `both` targets are available when the
caller already knows which desktop surface needs review.

## Trusted workflow boundary

`.github/workflows/gameengine-editor-visual-validation.yml` uses
`pull_request_target` so the executable workflow definition comes from the
trusted default branch. Before a Windows job is selected, the workflow requires:

- the pull request head repository to be `KdGithubIt/GameEngine`;
- the base branch to be `main`;
- the head branch to use the `chatgpt/gameengine-*` namespace;
- one valid visual-validation marker; and
- exact 40-character base and head commit SHAs from the pull request event.

The Windows job checks out that exact head SHA with persisted Git credentials
disabled. It may use the same trusted self-hosted Windows runner configuration
as normal GameEngine Windows validation; otherwise it uses `windows-latest`.

Normal ChatGPT Patch Dispatcher requests still MUST NOT modify `.github/**` or
`.chatgpt-requests/**`. Changes to this visual-validation infrastructure follow
the repository's dedicated infrastructure-branch and Draft-PR rule.

## Capture implementation

The Launcher and Editor expose a non-default Cargo feature named
`visual-validation`. That feature enables eframe's screenshot support only for
the capture invocation. Normal application builds do not enable the feature.

`scripts/ci/Invoke-EditorVisualValidation.ps1` builds and launches only the
requested desktop executable with the feature enabled and stores the resulting
PNG in the validation artifact directory.

Editor capture needs a valid current-format project because ADR 0117 requires
`engine-editor --project <path>`. By default the script creates a temporary
project through `engine-project-lifecycle::create_standard_project`; it does not
hand-write `project.json`, duplicate scaffold policy, or add validation-only
project data to the repository. The script also accepts a repository-relative
project path for targeted/manual use and rejects paths that escape the
workspace root.

## Artifacts

A successful or partially successful run uploads an artifact named like:

```text
gameengine-editor-visual-validation-<run-id>-<attempt>
```

It contains whichever screenshots were requested:

```text
editor.png
launcher.png
summary.json
```

`summary.json` records the resolved target, project source, screenshot byte
size, SHA-256 digest, and generation timestamp. The PNG files are the visual
evidence that ChatGPT or a human reviewer should inspect.

A screenshot being generated successfully proves that the requested desktop
application built, started, rendered a frame, and exported that frame. It does
not by itself prove that the UI looks correct. Visual correctness is established
only after the PNG is actually reviewed.

## Current scope and extensions

Version 1 captures the deterministic initial Launcher or Editor window. It is
intended for shell layout, toolbar, panels visible at startup, typography,
colors, clipping, spacing, and similar initial-state regressions.

UI states that require interaction, a specific document, a particular tool
window, or a sequence of inputs need an explicit future visual scenario rather
than hidden sleeps or coordinate-based automation. Such scenarios should keep
the same principles: deterministic setup, an explicit opt-in request, exact-head
execution, and screenshot artifacts that can be reviewed independently of the
normal Rust validation result.
