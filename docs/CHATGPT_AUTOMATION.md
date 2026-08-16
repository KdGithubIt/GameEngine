# ChatGPT GitHub Automation

Status: Accepted
Version: 1.4.1
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

## Producer operating checklist

The request producer MUST treat patch publication as a protocol, not as an
informal file upload. For every new implementation or correction request:

1. Start from the latest intended baseline and create or select one dedicated
   `chatgpt/gameengine-*` target branch.
2. Read the target branch itself, not a remembered or previously cached copy.
   Capture its current 40-character HEAD SHA and use that exact tree when
   generating the patch.
3. Read `AGENTS.md` and every specification, ADR, or workflow document it makes
   relevant to the requested change before constructing the patch.
4. Build one complete unified Git patch against the exact target tree. Keep
   product changes inside the public allow-list and never include `.github/**`
   or `.chatgpt-requests/**` in a normal dispatcher patch.
5. Preflight the exact reconstructed patch bytes that will be published. Run
   `git apply --check --whitespace=error-all <reconstructed-patch>` against the
   captured target tree, then confirm that the patch contains only intended
   paths and no symlink or submodule changes. Using plain `git apply --check`
   is not equivalent because it can miss whitespace errors the dispatcher will
   reject. Do not manually retype, reformat, or otherwise reconstruct a second
   copy of the patch after this preflight.
6. Split the preflighted patch only for transport size and publish those exact
   bytes as the declared `part-NNNN.patch` files. Before publishing `ready.json`,
   verify the published part byte counts and blob hashes, or the reconstructed
   content hash, against the preflight artifact. Never use `ready.json` as a
   partial-progress marker.
7. Immediately before creating `ready.json`, re-read the remote target branch
   HEAD. If it differs from `expected_head_sha`, abandon the unpublished
   request, regenerate the patch from the new target tree, and use a new request
   ID. Do not update `expected_head_sha` without regenerating the patch.
8. Add `ready.json` in its own final transport commit and change no other file in
   that commit. After this point the request is immutable.
9. Follow the signal, dispatcher, Draft PR, and Windows validation through to a
   terminal result. Read the validation mode and scope before interpreting the
   individual gate results.
10. On failure, identify the failing layer before changing code: transport
    envelope, dispatcher/preflight, target-branch concurrency, Rust/docs
    validation, visual validation, or external runner/service failure.
11. After a confirmed recovery, apply the incident-learning rules below so the
    same root cause does not need to be rediscovered on a later request.

The producer MUST NOT work around a dispatcher failure by pushing the intended
product patch directly to the task branch. Fix the request or, when the trusted
automation itself is defective, use the separately reviewed automation-
infrastructure path described by the repository policy.

## Failure diagnosis before retry

Use the failing layer to choose the recovery instead of making speculative code
changes:

- If the dispatch signal does not accept the ready commit, verify that the push
  was to `chatgpt-dispatch` and that the final commit newly added exactly one
  `.chatgpt-requests/<request-id>/ready.json` file.
- If request-envelope validation fails, compare the request directory and
  `ready.json` with the schema, contiguous part naming, size limits, and exact
  file list in this document.
- If the target HEAD no longer equals `expected_head_sha`, the request is stale.
  Re-read the target branch, regenerate the patch from that exact tree, and use
  a new request ID. Never force-apply or reuse the stale request.
- If `git apply --check` rejects the reconstructed patch, treat the patch/tree
  mismatch as the primary problem. Reconstruct the patch from the current
  target files instead of editing product code merely to make old patch context
  apply.
- If the dispatcher rejects a path, do not weaken the allow-list from the task
  request. `.github/**` and `.chatgpt-requests/**` are trust-boundary paths and
  require the separately reviewed automation-infrastructure workflow.
- If Windows validation fails after a successful patch application, use the
  repair loop below. Read `summary.json`, mode, scope, failing gate logs, and any
  diagnostics artifact before deciding whether another code patch is justified.
- If GitHub, a runner, the network, or a dependency service is the root cause,
  do not create speculative product changes to compensate for that external
  failure.

Confirmed failures and their durable recovery knowledge live in
`docs/CHATGPT_AUTOMATION_INCIDENTS.md`. That log supplements this protocol; it
does not override it.

## Patch parts

Patch transport is byte-preserving concatenation. The dispatcher concatenates
parts in the order declared by `patch_parts`; it does not add separators.
Parts therefore do not need to be independently applicable patches. The
published blobs MUST reconstruct byte-for-byte to the artifact that passed
producer preflight; a patch that was retyped or transformed after preflight has
not been preflighted.

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
`cargo metadata --format-version 1 --locked`, derives workspace membership,
maps changed files to their owning packages, and selects the changed packages
for the normal PR critical path. Reverse dependents are intentionally not added;
full validation on `main` and nightly provides the cross-workspace safety net.

Package names and crate directories are not hard-coded in the workflow. New or
split crates therefore participate as soon as they are workspace members visible
in Cargo metadata. A package-local `Cargo.toml` change remains affected-mode when
current metadata still resolves it safely. A removed package that cannot be
reconstructed from current metadata falls back to `full` rather than guessing.

The planner emits a machine-readable plan containing validation mode, skip
state, changed packages, affected packages, and the package sets selected for
tests, Clippy, and documentation. The Windows executor consumes that plan and
does not repeat classification logic.

Affected validation runs formatting plus package-selected Clippy, tests, and
documentation. Affected Clippy uses package selection without `--all-targets`;
full mode retains `--all-targets` for workspace-wide target coverage.

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

## Incident learning

`docs/CHATGPT_AUTOMATION_INCIDENTS.md` is the durable operational memory for
confirmed ChatGPT/Dispatcher/validation mistakes and failures.

After a failure is understood and a recovery has been validated, ChatGPT MUST:

1. Search the incident log by symptom, failing layer, and root cause.
2. Update an existing entry when the root cause is already represented. Add new
   evidence, a clearer diagnosis, or a stronger prevention rule instead of
   creating a duplicate incident.
3. Add a new incident only when the root cause is materially different from
   existing entries.
4. Record at least the symptom, failing layer, confirmed root cause, successful
   resolution, and prevention/next-action rule.
5. Distinguish observed evidence from inference. Do not record a guessed root
   cause as confirmed merely because one retry happened to succeed.
6. Keep secrets, credentials, private machine paths, and large raw logs out of
   the document. Link to a durable PR, workflow run, commit, or request ID when
   useful instead of copying entire logs.

The incident log is organized by root cause rather than chronology. Repeated
occurrences of the same problem belong in the same entry so that the repository
accumulates reusable recovery knowledge instead of an ever-growing run diary.

If learning from an incident shows that the protocol itself is incomplete or
incorrect, update this canonical document in the same or a follow-up change.
If it shows that trusted automation under `.github/**` is defective, do not try
to repair that trust boundary through a normal dispatcher patch; use a separate
`chatgpt/gameengine-*` automation-infrastructure branch and Draft PR.

## Fallback

No write-capable fallback is installed in the public repository. If the dispatcher is unavailable, stop and repair the trusted dispatcher path rather than bypassing its branch, exact-head, Draft PR, or validation guarantees.
