# Editor Visual Validation

Status: Accepted
Version: 1.5.0
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

### Authoring-tool scenario

A PR that needs one modeless Editor authoring window visible in the screenshot
MAY add exactly one secondary marker alongside an explicit `editor` or `both`
target:

```text
<!-- gameengine-visual-validation: editor -->
<!-- gameengine-visual-authoring-tool: Ability Designer -->
```

The value is the human-readable [`AuthoringTool`] label exposed by the exact PR
head being validated. The workflow accepts only a bounded plain-text label and
passes it to the checked-out Editor; the Editor resolves that label against its
own `AuthoringTool::ALL` catalog and fails startup if no exact match exists. The
workflow does not hard-code product-specific enum variants.

The scenario is available only with explicit `editor` or `both`. It is rejected
with `auto` or `launcher` so a requested tool window cannot be silently omitted
by automatic target classification.

The validation-only Editor process receives the requested label through
`GAMEENGINE_VISUAL_AUTHORING_TOOL` and opens that modeless window before the
screenshot request. Normal Editor builds and launches do not set or depend on
this environment variable. No coordinate click, synthetic pointer input, or
sleep-based automation is used.

This scenario validates the authoring window's deterministic startup state. A
state that additionally requires a specific asset or document to be loaded
still requires a separate explicit document scenario; opening an authoring tool
alone is not evidence for document-dependent controls that are not visible yet.

### Remote AI Studio browser scenario

A PR that needs responsive Remote AI Studio browser evidence MAY add this
secondary marker alongside an explicit `editor` or `both` target:

```text
<!-- gameengine-visual-validation: editor -->
<!-- gameengine-visual-remote-ai-studio: browser -->
```

The secondary marker does not broaden the workflow trust boundary. The normal
same-repository `chatgpt/gameengine-*` pull-request checks must select the
Windows Editor capture job first. The checked-out validation script then starts
the exact-head Editor with its non-default `visual-validation` feature, seeds a
deterministic project-scoped Agent Host fixture after the loopback gateway is
running, and receives the real companion URL from that production gateway.

Microsoft Edge on the Windows runner opens the production HTTP/JavaScript
companion directly. The script captures desktop (`1440x1000`), narrow
(`900x1000`), and mobile (`390x844`) viewport evidence plus a taller image at
each width so controls, progress, permission state, and captured-frame review
below the initial fold can be inspected. The fixture is compiled only for
`visual-validation`; it does not duplicate the companion DOM or bypass Agent
Host, sanitized DTO, HTTP/SSE, permission, or run-state contracts. Mobile and
narrow review must also confirm that controls and long metadata remain inside
the viewport without horizontal clipping or required horizontal scrolling.

The Remote helper launch clears inherited authoring-tool and screenshot
environment variables, then selects the explicit `ADR 0133 Remote AI Studio`
scenario before starting the second Editor process. That scenario is excluded
from the native screenshot lifecycle because browser capture owns its lifetime.
The Editor also treats empty or whitespace-only authoring-tool and native
screenshot values as absent, so ambient CI variables cannot accidentally select
an invalid visual scenario or an invalid capture path.

If deterministic fixture preparation fails, the Editor remains alive and the
validation marker reports the concrete fixture error to the runner instead of
turning a fallible screenshot fixture into an Editor startup failure.

## Workflow and execution boundary

`.github/workflows/gameengine-editor-visual-validation.yml` uses the ordinary
`pull_request` event. Visual capture needs no repository secrets, and using the
same pull-request event family as normal validation makes the run directly
visible to commit-filtered pull-request workflow run lookup instead of requiring
a second run-ID publication channel.

Before a Windows job is selected, the workflow requires:

- the pull request head repository to be `KdGithubIt/GameEngine`;
- the base branch to be `main`;
- the head branch to use the `chatgpt/gameengine-*` namespace;
- one valid visual-validation marker; and
- exact 40-character base and head commit SHAs from the pull request event.

The optional authoring-tool scenario is parsed inside the GitHub-hosted context
job before its value can reach the checked-out Editor process.

The workflow has no issue or pull-request write token and receives no secrets.
The Windows capture job keeps only `contents: read`, checks out the exact PR head
SHA, verifies that checkout, and disables persisted Git credentials. It may use
the same repository-owned self-hosted Windows runner configuration as normal
GameEngine Windows validation; otherwise it uses `windows-latest`.

The same-repository branch restriction is a trust precondition for executing PR
code on the Windows runner. Automation infrastructure remains protected by the
repository rule that normal Dispatcher product patches do not modify
`.github/**` or `.chatgpt-requests/**`; changes to this workflow use a dedicated
infrastructure branch and Draft PR.

## Capture implementation

The Launcher and Editor expose a non-default Cargo feature named
`visual-validation`. Normal application builds do not enable the feature.

Editor capture keeps eframe's normal wgpu renderer. The capture script sets
`GAMEENGINE_SCREENSHOT_TO` only for the validation invocation. The Editor then
requests the next rendered frame through egui's `ViewportCommand::Screenshot`,
receives the renderer-independent `Event::Screenshot`, and writes the returned
image with its existing PNG encoder. The capture path is bounded: if the
screenshot response is not returned, the validation-only process closes and the
script reports a missing artifact instead of waiting indefinitely.

When an authoring-tool scenario is requested, the same script additionally sets
`GAMEENGINE_VISUAL_AUTHORING_TOOL` for the Editor child process only. The Editor
resolves the label from its own catalog and marks the corresponding modeless
window open before the screenshot frame is rendered.

Launcher capture also keeps eframe's normal wgpu renderer. The capture script
sets `GAMEENGINE_LAUNCHER_SCREENSHOT_TO` only for the validation invocation.
The Launcher requests and receives the screenshot through the same
renderer-independent egui command/event contract, then writes PNG bytes through
its validation-only capture module. Its capture path uses the same bounded
failure behavior so a missing screenshot event cannot leave validation waiting
indefinitely.

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

`summary.json` records the resolved target, project source, optional authoring
tool scenario, screenshot byte size, SHA-256 digest, and generation timestamp.
Multi-capture suites also record the per-capture scenario and keep a combined
Cargo/Editor log beside each attempted capture. If a capture fails after earlier
screenshots succeeded, `summary.json` is still written from the `finally` path
with the failed capture name and error message so the uploaded artifact preserves
the exact stopping point and child-process diagnostics. The PNG files are the
visual evidence that ChatGPT or a human reviewer should inspect.

A screenshot being generated successfully proves that the requested desktop
application built, started, rendered a frame, and exported that frame. It does
not by itself prove that the UI looks correct. Visual correctness is established
only after the PNG is actually reviewed.

## Pull-request run and artifact discovery

Because the workflow is a normal `pull_request` workflow, ChatGPT SHOULD start
from the exact PR head SHA and use the connector's commit-filtered pull-request
workflow run lookup. Select the `GameEngine Editor Visual Validation` run for
that head and inspect its workflow run ID and jobs directly. This path works
while the Windows capture is queued or running; a separate PR comment is not
required to learn the run identity.

The artifact lookup key for a run is deterministic:

```text
gameengine-editor-visual-validation-<workflow run ID>-<workflow run attempt>
```

After the capture job reaches a terminal state, ChatGPT SHOULD list artifacts
for that workflow run ID, select the matching Artifact lookup key when present,
download the archive, and inspect every PNG required by the resolved target.
If the run failed, inspect its job and step diagnostics before deciding whether
the failure is product code, validation infrastructure, runner, or another
external dependency.

A successful workflow or a non-empty screenshot MUST NOT be called Visual PASS
until the relevant PNG has actually been reviewed. The workflow result and
`summary.json` are execution evidence; the PNG is the visual evidence.

If a valid same-repository visual-validation request does not appear in the
commit-filtered pull-request workflow run lookup, treat that as an automation
failure and investigate the workflow trigger/configuration. Do not fall back to
claiming that artifacts are unavailable merely because capture has not finished.

## Current scope and extensions

Version 1.5 captures the deterministic initial Launcher or Editor window and can
open one modeless authoring-tool window before an Editor screenshot. Its normal
`pull_request` trigger makes the workflow run directly discoverable from the PR
head SHA without an auxiliary run-ID comment or write-capable reporting job.

It is intended for shell layout, toolbar, startup-visible panels,
authoring-window startup layout, typography, colors, clipping, spacing, and
similar regressions.

UI states that require a specific document, populated tool state, or a sequence
of inputs still need an explicit future visual scenario rather than hidden
sleeps or coordinate-based automation. Such scenarios should keep the same
principles: deterministic setup, an explicit opt-in request, exact-head
execution, and screenshot artifacts that can be reviewed independently of the
normal Rust validation result.

[`AuthoringTool`]: ../crates/editor/src/authoring_tools.rs
