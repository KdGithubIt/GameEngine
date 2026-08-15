# ChatGPT GitHub Automation

Status: Accepted
Version: 1.3.0
Canonical location: `docs/CHATGPT_AUTOMATION.md`

## Purpose

This document defines the repository-native path used by ChatGPT to apply and
validate GameEngine changes without Codex and without the OpenAI API.

The normal path is:

```text
ChatGPT
  -> chatgpt-dispatch transport branch
  -> read-only GitHub Actions dispatch signal
  -> trusted default-branch dispatcher
  -> chatgpt/gameengine-* task branch
  -> Draft pull request
  -> GameEngine Windows Validation
  -> ChatGPT reads the run result/log artifact
  -> optional corrected request
```

The public repository uses the dispatcher path directly. The legacy private-repository bridge, auto-merge workflow, and `GameEngine-ChatGPT-Apply` fallback are intentionally not installed here.

## Trust boundary

`chatgpt-dispatch` is a transport branch. Request files on that branch are data,
not executable automation.

A small push-triggered signal workflow notices a newly-created `ready.json`.
That signal has only `contents: read` and cannot mutate repository or Actions
state. Its completion triggers `gameengine-chatgpt-dispatcher.yml` through
`workflow_run`. GitHub loads the `workflow_run` workflow from the default branch,
so the write-capable dispatcher is not supplied by the transport branch.

The trusted dispatcher repeats the ready-commit validation; it does not trust
the signal workflow's checks as authorization.

The producer of requests MUST modify only `.chatgpt-requests/` on the transport
branch. Workflow files, product code, and repository configuration MUST NOT be
changed as part of a request transport commit.

## Branches

### `chatgpt-dispatch`

Long-lived transport branch used only for immutable ChatGPT request records.
Each request is stored under:

```text
.chatgpt-requests/<request-id>/
```

Request records are retained. The dispatcher does not delete or rewrite them,
which avoids a cleanup push retriggering the ready-marker workflow.

### `chatgpt/gameengine-*`

All target work branches MUST start with `chatgpt/gameengine-`. A request for
any other branch is rejected.

ChatGPT creates or reads the target branch before constructing the request and
records its exact 40-character HEAD SHA in `expected_head_sha`.

## Request lifecycle

A request is published in this order:

1. Read the current target branch and capture its exact HEAD SHA.
2. Create a unique request ID.
3. Split the complete unified Git patch into one or more transport parts.
4. Commit all `part-NNNN.patch` files to
   `.chatgpt-requests/<request-id>/` on `chatgpt-dispatch`.
5. Re-read the target branch. If its HEAD changed, abandon this request and
   build a new request from the new HEAD.
6. Create `ready.json` in a separate final commit. That commit MUST add exactly
   one file: the request's `ready.json`.
7. The read-only signal completes; the default-branch `workflow_run` dispatcher starts.

A `ready.json` modification, deletion, or a commit that changes any additional
file is rejected. Once ready, a request is immutable. Corrections use a new
request ID.

## Patch parts

Patch transport is byte-preserving concatenation. The dispatcher concatenates
parts in the order declared by `patch_parts`; it does not add separators.
Parts therefore do not need to be independently applicable patches.

Constraints:

- names are contiguous `part-0000.patch`, `part-0001.patch`, ...;
- 1 to 64 parts;
- each part is 1 to 60,000 bytes;
- reconstructed patch is at most 4 MiB;
- request directory contains only listed parts plus `ready.json`;
- files must be normal non-executable Git blobs, not symlinks.

## `ready.json` format

Schema version 1 uses this shape:

```json
{
  "schema_version": 1,
  "request_id": "renderer-aa-20260814-01",
  "target_branch": "chatgpt/gameengine-renderer-aa",
  "expected_head_sha": "0123456789abcdef0123456789abcdef01234567",
  "patch_parts": [
    "part-0000.patch",
    "part-0001.patch"
  ],
  "commit_message": "Add shared renderer anti-aliasing",
  "pr_title": "GameEngine: add shared renderer anti-aliasing",
  "pr_body": "## Summary\n\nAdds the shared renderer anti-aliasing path."
}
```

Required validation:

- `request_id` matches the request directory;
- `target_branch` is a safe `chatgpt/gameengine-*` ref;
- `expected_head_sha` is a full SHA;
- `commit_message` is a non-empty single line of at most 120 characters;
- `pr_title` is a non-empty single line of at most 200 characters;
- `pr_body` is at most 8,000 characters;
- `patch_parts` exactly matches the files in the request directory;
- the legacy `<!-- gameengine-chatgpt-automation -->` auto-merge marker is
  forbidden in dispatcher PR bodies.

## Dispatcher safety checks

Before publishing a change, the trusted dispatcher performs all of the
following:

1. Verifies the triggering signal succeeded, came from a push on this repository
   and `chatgpt-dispatch`, and the request commit is reachable from that branch.
2. Verifies the final commit newly added only the declared `ready.json`.
3. Verifies request schema, part names, file modes, counts, and size limits.
4. Checks out the declared target branch.
5. Requires its current HEAD to equal `expected_head_sha`.
6. Reconstructs the patch and runs `git apply --check --whitespace=error-all`.
7. Applies the patch to the local index only.
8. Rejects `.github/**` and `.chatgpt-requests/**`, and rejects every other
   changed path outside the explicit public GameEngine allow-list, including old
   paths of renames by inspecting the diff with rename detection disabled.
9. Rejects old or new Git modes `120000` (symlink) and `160000` (submodule).
10. Runs `git diff --cached --check`.
11. Re-reads the remote target HEAD immediately before commit/push.
12. Pushes with an exact `--force-with-lease` for `expected_head_sha`.

The workflow is globally serialized with `cancel-in-progress: false`. The
serialization prevents dispatcher requests from racing one another; the HEAD
checks and lease still reject changes made by humans, fallback automation, or
other external writers.

No stale request is force-applied. A stale request fails and must be rebuilt
from the latest branch state.

## Pull request rules

After a successful push the dispatcher creates or reuses the one open PR whose
head is the target branch.

- base branch MUST be `main`;
- PR MUST remain Draft;
- existing non-Draft PRs are converted back to Draft before validation;
- dispatcher PR body carries `<!-- gameengine-chatgpt-dispatcher -->`;
- dispatcher PRs never carry the legacy auto-merge authorization marker;
- dispatcher never calls merge or enables auto-merge.

`gameengine-windows-validation.yml` also contains no merge step. The public
repository does not install the legacy auto-merge workflow.

## Windows validation

The dispatcher explicitly starts `gameengine-windows-validation.yml` using
`workflow_dispatch` **from `main`**, so an old or modified task branch cannot
select an older validation workflow definition. The dispatcher supplies:

- target branch;
- PR number;
- exact pushed HEAD SHA;
- request ID.

The trusted validation workflow resolves the target branch through the GitHub
API, requires its current HEAD to equal the supplied SHA, and then checks out
that exact commit. Pull-request, push, and merge-group runs derive the same
head/base context from their events.

Validation has three modes: `affected`, `full`, and `docs`.

### Affected mode

Normal pull requests, merge groups, and pushes to `main` use `affected` mode
when changed paths can be classified safely. The planner runs
`cargo metadata --format-version 1 --locked`, derives workspace membership and
the workspace dependency graph, maps changed files to their owning packages,
and selects the changed packages plus their transitive reverse-dependency
closure.

Package names, crate directories, and dependency relationships are not
hard-coded in the workflow. New or split crates therefore participate as soon
as they are workspace members visible in Cargo metadata. A package-local
`Cargo.toml` change remains affected-mode when current metadata still resolves
it safely. A removed package that cannot be reconstructed from current metadata
falls back to `full` rather than guessing.

The planner emits a machine-readable plan containing validation mode, skip
state, changed packages, affected packages, and the package sets selected for
tests, Clippy, and documentation. The Windows executor consumes that plan and
does not repeat classification logic.

Affected validation runs formatting plus package-selected Clippy, tests, and
documentation. Clippy uses `--all-targets` for the selected packages so target
coverage is not reduced merely because the run is affected-mode.

### Full mode

Full mode is mandatory when:

- running the nightly schedule;
- the workspace manifest, lock file, or pinned toolchain changes;
- validation/build infrastructure changes;
- any changed path cannot be classified safely;
- Cargo metadata cannot be evaluated safely.

Full mode runs broad workspace validation:

```text
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

The explicit workspace check is part of full validation only. The normal
affected path remains package-selected so ordinary pull requests do not pay for
a redundant full-workspace compilation.

### Documentation-only mode

A change containing only explicitly recognized GameEngine documentation paths
uses `docs` mode on pull-request, merge-group, and `main` fast paths. Rust
compilation is skipped. The nightly schedule still validates the full workspace.

### Classification safety and runner reuse

The classifier knows workspace packages only through Cargo metadata. Unknown or
deleted package paths, workspace-wide Cargo inputs, Cargo/build configuration,
and validation scripts force `full` mode. The workflow never guesses an
affected package for an unrecognized path. The nightly full suite is retained
specifically to detect an affected-planner mistake that escaped a fast path.

CI uses sccache for cacheable Rust compiler invocations, keeps
`CARGO_INCREMENTAL=0`, and disables debug information for the CI `dev` and
`test` profiles. GitHub-hosted Windows runners use a fresh Cargo home and
`cargo fetch --locked` instead of restoring the former registry/git archive on
every Windows job.

Persistent self-hosted Windows runners are an explicit repository-variable
opt-in. `GAMEENGINE_WINDOWS_RUNNER_LABELS` is a JSON array of runner labels; when
it is unset the workflow continues to use `windows-latest`. Fork pull requests
always use `windows-latest`, even when the variable is configured. On a trusted
self-hosted runner, `GAMEENGINE_CI_CACHE_ROOT` may select a local SSD root for a
persistent Cargo home, target directory, and sccache directory. Target and
sccache paths include the pinned Rust channel and host triple so incompatible
toolchains are not intentionally mixed; Cargo itself continues to fingerprint
profiles and features.

Per-gate diagnostic artifacts are uploaded only when the Windows executor
fails. Successful compiler output remains in the Actions log instead of being
duplicated into artifacts. Every run still uploads the aggregate artifact:

```text
gameengine-windows-validation-<run-id>-<attempt>
```

Its `summary.json` uses schema version 4 and records the planner scope, job
outcomes, and aggregate result.

The aggregate report job creates or updates one concise PR comment containing
the selected validation mode and scope. The workflow run conclusion is the
authoritative result for the checks required by that mode.

The nightly schedule is the mandatory full-workspace safety net. Therefore an
`affected` PR, merge-group, or `main` result MUST NOT be described as proof
that every workspace package passed; it proves that the metadata-derived
affected scope passed.

## ChatGPT repair loop

After dispatch, ChatGPT should inspect the Windows validation run and its
machine-readable aggregate result.

1. Read `summary.json` and identify the selected validation mode and affected
   package scope before interpreting a success or failure.
2. If every check required by that mode succeeds, stop and leave the PR Draft.
3. If formatting, workspace check, Clippy, tests, or documentation fail, inspect
   the executor log and the failure-only diagnostics artifact when present.
4. Separate code failures from runner, GitHub, network, or dependency-service
   failures before editing code.
5. Re-read the current target branch HEAD and affected files.
6. Create a new request ID with a patch against that exact HEAD.
7. Repeat dispatch and validation.

Normal automated repair is limited to five correction rounds. Stop earlier if
the same essential failure repeats twice or if the failure is external to the
code. Do not make speculative code changes to compensate for runner/network
failures.

An `affected` result must never be rewritten or summarized as a full-workspace
result. If the changed paths should have forced `full` but classification did
not, fix the planner/validation contract instead of treating the narrower run
as sufficient.

## Fallback

No write-capable fallback is installed in the public repository. If the dispatcher is unavailable, stop and repair the trusted dispatcher path rather than bypassing its branch, exact-head, Draft PR, or validation guarantees.
