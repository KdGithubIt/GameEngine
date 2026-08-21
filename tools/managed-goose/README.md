# GameEngine Managed Goose patched build

This directory defines the reproducible source and patch provenance for the
temporary GameEngine-managed Goose ACP binary used while upstream mid-turn
compaction fixes remain unmerged.

## Baseline

- Upstream: `aaif-goose/goose`
- Official baseline: `v1.45.0`
- Exact upstream SHA: `4dc0420f5704a92806c6628c8f0a3497d7a88759`
- GameEngine previous managed pin: `v1.44.0`
  (`876555f85b1bd0e15ed75eed7c5ac1163c1f097a`)
- Upstream PR evaluated: `#11075`, observed open + Draft
- PR base/head snapshot:
  `d6ee97b4ced58f6c59befd537adb356cc27c75cd` /
  `0786603fe8187ac52ed826bb07a48956976865f5`

`v1.45.0` is used instead of upstream `main` so the managed binary stays on the
newest official stable release while carrying only the fixes that are still
missing there. The PR's state-machine hunk is intentionally not carried because
that architecture does not exist in the stable baseline; the equivalent legacy
Agent-loop hunk is carried instead.

## Patch policy

`series.json` is canonical. Every patch has:

- a stable ordered path;
- a SHA-256 checked before application;
- an exact upstream origin commit or GameEngine provenance reason; and
- an allow-list of the upstream files it may modify.

The series includes:

1. a GameEngine distribution identity so `goose --version` is visibly different
   from official Goose;
2. structured context-overflow 400 classification;
3. bounded retry when a compaction summary is truncated at the output limit;
4. propagation of the streaming output-limit flag through `collect_stream`; and
5. proactive mid-turn compaction after a tool result, plus executable regression
   coverage for the same-turn tool loop.

The upstream provider-limit precedence and desktop token-bar commits are not
included. Managed Local already owns the authoritative context/output contract
through the llama-server launch, custom Goose provider definition,
`GOOSE_CONTEXT_LIMIT`, and `GOOSE_MAX_TOKENS`. Importing another precedence
policy would duplicate responsibility, and the upstream desktop model picker is
not part of the GameEngine ACP execution path.

## Build and release

`.github/workflows/gameengine-managed-goose-build.yml` checks out the exact
upstream SHA, verifies the series, applies each patch with whitespace checking,
runs Rust validation and the focused regression suite, builds the official
Windows CLI target, probes ACP availability, and creates a deterministic ZIP.

The uploaded artifact contains:

- the immutable ZIP;
- `provenance.json`;
- `SHA256SUMS.txt`; and
- the exact `series.json`.

A manual workflow-dispatch path may publish the same files as a GitHub Release.
Only that job receives `contents: write`. It refuses to overwrite an existing
release tag, so a published distribution identity is immutable.

The pre-merge pull-request workflow can exercise checkout, patching, tests,
Windows build, packaging, and artifact upload. Publishing a production Release
from this new workflow cannot be end-to-end proven until the workflow exists on
the default branch; that limitation must be reported as `production E2E未検証`
until a post-merge run succeeds.
