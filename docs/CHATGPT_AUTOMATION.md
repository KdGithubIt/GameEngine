# ChatGPT GitHub Automation

Status: Accepted
Version: 2.0.0
Canonical location: `docs/CHATGPT_AUTOMATION.md`

## Purpose

This document defines the repository-native path used by ChatGPT to apply and
validate GameEngine changes without Codex and without the OpenAI API.

The normal path is:

```text
ChatGPT / GameEngine GitHub Worker
  -> exact target checkout with intended product changes staged
  -> trusted request builder generates and preflights the Git patch
  -> chatgpt-dispatch-stage-<request-id> staging branch
  -> read-only GitHub Actions stage signal
  -> trusted default-branch transport publisher + pre-publish preflight
  -> chatgpt-dispatch transport branch
  -> trusted default-branch dispatcher
  -> chatgpt/gameengine-* task branch
  -> Draft pull request
  -> GameEngine Windows Validation
  -> ChatGPT reads the run result/log artifact
  -> optional corrected request
```

The public repository uses the dispatcher path directly. The legacy
private-repository bridge, auto-merge workflow, and `GameEngine-ChatGPT-Apply`
fallback are intentionally not installed here.

## Trust boundary

Request producers do not write the shared `chatgpt-dispatch` ref directly.
Each request is first written to a dedicated
`chatgpt-dispatch-stage-<request-id>` branch as data. The staging branch may
contain only immutable `.chatgpt-requests/<request-id>/part-NNNN.patch` additions
followed by one final `ready.json` addition.

A small push-triggered staging signal notices a newly-created `ready.json` on a
staging branch. That signal has only `contents: read` and cannot mutate
repository or Actions state. Its completion triggers the write-capable transport
publisher through `workflow_run`. GitHub loads the `workflow_run` workflow from
the default branch, so the write-capable publisher is not supplied by the
producer-controlled staging branch.

The trusted transport publisher validates the complete staged commit range and,
before any transport write, loads the request protocol implementation from
current `main` and preflights the staged request against the exact declared
target branch. The pre-publish preflight reconstructs the published patch bytes,
validates the request schema, checks schema-v2 hashes and the current-main
baseline, and runs the same strict Git applicability/path/mode checks used by the
dispatcher. Only a request that passes that trusted preflight may be serialized
onto `chatgpt-dispatch`.

After preflight, the publisher globally serializes publication. It cherry-picks
the staged request commits onto the latest `chatgpt-dispatch` head and pushes
with an exact lease. This makes the publisher the normal single writer for the
shared transport ref. A human, fallback tool, or old producer that writes
`chatgpt-dispatch` directly is an external writer; the publisher must reject the
stale lease instead of force-updating over that change.

Both the transport publisher and dispatcher opt into the GitHub Actions
`concurrency` queue with `queue: max`. This preserves up to 100 pending runs in
each global serialization group instead of the default behavior that replaces an
older pending run when a newer one arrives. If a publisher run is canceled or
cannot enter the bounded queue, the immutable staging branch remains the source
of truth and can be replayed from `main` with the explicit `workflow_dispatch`
inputs. A dispatcher run can likewise be replayed from `main` using the exact
published request commit.

GitHub does not start ordinary push-triggered workflows for pushes made with a
workflow's own `GITHUB_TOKEN`. Therefore the publisher explicitly starts the
trusted dispatcher with `workflow_dispatch` after the transport push. The
dispatcher is loaded from `main`, requires the supplied full request commit SHA
to be reachable from `chatgpt-dispatch`, and repeats the ready-commit and request
envelope validation before applying any product patch.

The existing read-only `chatgpt-dispatch` push signal remains a compatibility
path for an externally-created valid transport record. It is not the normal
producer publication path. Request producers MUST use staging branches and MUST
NOT directly advance `chatgpt-dispatch`.

Workflow files, product code, and repository configuration MUST NOT be changed
as part of request staging or transport publication. Normal request data is
limited to `.chatgpt-requests/<request-id>/`.

## Branches

### `chatgpt-dispatch-stage-<request-id>`

Per-request staging branch used to remove producer contention from the shared
transport ref. The branch name MUST be exactly
`chatgpt-dispatch-stage-<request-id>`, where `<request-id>` satisfies the same
request ID rules as `ready.json`.

A producer creates a fresh staging branch from current `main`, publishes one or
more patch-part addition commits, and finally adds `ready.json` in its own commit.
All staged commits after the `main` base MUST be linear non-merge commits. Before
`ready.json`, each staged commit may only add normal
`.chatgpt-requests/<request-id>/part-NNNN.patch` files. The final commit MUST add
exactly `.chatgpt-requests/<request-id>/ready.json` and no other file.

After the ready commit is pushed, the staging branch is immutable. If it moves
before the serialized publisher runs, publication is rejected. Corrections use
a new request ID and therefore a new staging branch.

### `chatgpt-dispatch`

Long-lived shared transport branch used only for immutable ChatGPT request
records. Each request is stored under:

```text
.chatgpt-requests/<request-id>/
```

The trusted transport publisher is the normal single writer. Producers MUST NOT
create commits on this branch or update its ref directly. Request records are
retained. The publisher does not delete or rewrite them.

### `chatgpt/gameengine-*`

All target work branches MUST start with `chatgpt/gameengine-`. A request for
any other branch is rejected.

ChatGPT creates or reads the target branch before constructing the request and
records its exact 40-character HEAD SHA in `expected_head_sha`.

## Mechanical request builder

New normal product requests MUST be produced with
`.github/chatgpt/request_protocol.py build` from an exact checkout of the target
HEAD. Hand-authored unified diffs and manually maintained hunk headers are not a
normal producer path.

The producer prepares the intended product state in a disposable checkout and
stages exactly the intended files in Git. The builder then:

1. requires checkout `HEAD` to equal the supplied full `expected_head_sha`;
2. requires the supplied `baseline_main_sha` to be an ancestor of that target;
3. rejects unstaged tracked changes and untracked files so the staged index is
   the complete change source;
4. obtains the patch mechanically with Git from the staged index and exact
   target tree, including additions and deletions;
5. preflights that exact patch in a detached worktree with
   `git apply --check --whitespace=error-all`;
6. applies it only to the temporary index, runs `git diff --cached --check`, and
   enforces the public path allow-list plus symlink/submodule restrictions;
7. re-reads the remote target branch and remote `main` before emitting
   `ready.json` unless running the explicit regression-test-only bypass;
8. splits the immutable preflight artifact only at newline boundaries and only
   for transport size;
9. computes `patch_sha256` and `patch_bytes` from the complete artifact; and
10. emits schema-v2 `ready.json` plus contiguous `part-NNNN.patch` files.

The producer MUST publish those emitted patch part bytes without retyping,
reformatting, line-ending conversion, or synthetic reconstruction. If the
producer cannot run the builder against a real exact target checkout, it MUST
obtain an execution environment such as the GameEngine GitHub Worker rather
than falling back to hand-authored unified diff text.

## Request lifecycle

A new request is published in this order:

1. Read the current target branch and current `main`; capture both exact full
   SHAs.
2. Prepare the intended product change in a disposable checkout whose `HEAD` is
   exactly the captured target SHA, and stage exactly the intended files.
3. Run the mechanical request builder. It generates the unified Git patch,
   strict-preflights the exact artifact, rechecks remote target/main state,
   splits the artifact, and emits schema-v2 request files.
4. Create a unique request ID and a fresh
   `chatgpt-dispatch-stage-<request-id>` branch from current `main`.
5. Add the exact builder-emitted `part-NNNN.patch` blobs under
   `.chatgpt-requests/<request-id>/` on the staging branch. The producer may use
   one or more commits, but those commits may only add patch-part files for this
   request.
6. Re-read the target branch and `main`. If either differs from the builder's
   `expected_head_sha` / `baseline_main_sha`, abandon this unpublished request,
   rebuild from current state, and use a new request ID.
7. Add the exact builder-emitted `ready.json` in a separate final staging commit.
   That commit MUST add exactly one file: the request's `ready.json`.
8. The read-only stage signal completes. The trusted default-branch publisher
   validates immutable stage history and executes the trusted pre-publish
   preflight against the exact target before any transport mutation.
9. Only after successful preflight, the publisher waits in the global publisher
   concurrency queue and cherry-picks the staged request commits onto the latest
   `chatgpt-dispatch` head.
10. Immediately before updating `chatgpt-dispatch`, the publisher re-reads its
    remote head and pushes with an exact lease. An unexpected external writer
    makes the publication fail rather than overwrite the new transport history.
11. After a successful or idempotently detected publication, the publisher
    starts `gameengine-chatgpt-dispatcher.yml` from `main` with the exact
    published ready commit SHA.
12. The dispatcher revalidates that commit and continues to the target task
    branch, Draft PR, and Windows validation.

A `ready.json` modification, deletion, staging-branch rewrite after ready, or a
staged commit that changes any non-request path is rejected. Once ready, a
request is immutable. Corrections use a new request ID.

## Producer operating checklist

The request producer MUST treat patch publication as a protocol, not as an
informal file upload. For every new implementation or correction request:

1. Start from the latest intended baseline and create or select one dedicated
   `chatgpt/gameengine-*` target branch.
2. Read the target branch itself and current `main`, not remembered or cached
   values. Capture both full 40-character SHAs.
3. Read `AGENTS.md` and every specification, ADR, or workflow document it makes
   relevant to the requested change before preparing the product change.
4. Modify a real checkout of the exact target HEAD and stage exactly the intended
   product files. Do not manually write unified-diff hunk headers or calculate
   hunk coordinates. Do not construct a synthetic or stitched partial source
   fixture as the diff source.
5. Run `.github/chatgpt/request_protocol.py build` to mechanically generate the
   complete Git patch, strict-preflight it against the exact target tree, verify
   allowed paths/modes, re-read remote target/main, split on newline boundaries,
   and create schema-v2 metadata. The builder's output is the authoritative
   preflight artifact.
6. Keep product changes inside the public allow-list and never include
   `.github/**` or `.chatgpt-requests/**` in a normal dispatcher patch. Because
   Git generates the diff from the repository root, patch paths MUST remain
   repository-root relative and MUST NOT acquire checkout-directory prefixes
   such as `GameEngine/`.
7. Create a fresh staging branch named
   `chatgpt-dispatch-stage-<request-id>` from current `main` and publish the
   exact builder output as contiguous `part-NNNN.patch` additions. Verify the
   staged part byte counts/blob hashes or reconstructed hash against the builder
   artifact. Never use `ready.json` as a partial-progress marker.
8. Immediately before adding `ready.json`, re-read remote target and `main`.
   Schema v2 declares both `expected_head_sha` and `baseline_main_sha`; any
   difference requires abandoning the unpublished request and rebuilding from
   current state with a new request ID. Do not update either SHA without
   regenerating the patch.
9. Add the exact builder-emitted `ready.json` in its own final staging commit and
   change no other file in that commit. After this point the staging branch and
   request are immutable. Do not advance `chatgpt-dispatch` yourself.
10. Follow the stage signal, trusted pre-publish publisher, dispatcher, Draft PR,
    and Windows validation through to a terminal result. Read validation mode
    and scope before interpreting individual gate results.
11. On failure, identify the failing layer before changing code: request builder,
    staging signal, trusted pre-publish preflight, transport publication, request
    envelope, dispatcher/apply, target-branch concurrency, Rust/docs validation,
    visual validation, or external runner/service failure.
12. After a confirmed recovery, apply the incident-learning rules below so the
    same root cause does not need to be rediscovered on a later request.

The producer MUST NOT work around a builder, staging, publisher, or dispatcher
failure by pushing the intended product patch directly to the task branch or by
directly advancing `chatgpt-dispatch`. Fix the request or, when trusted
automation itself is defective, use the separately reviewed
automation-infrastructure path described by the repository policy.

## Failure diagnosis before retry

Use the failing layer to choose the recovery instead of making speculative code
changes:

- If the request builder rejects the staged change, correct the exact staged
  product state or target/baseline mismatch. Do not hand-edit the generated
  unified diff to bypass the builder.
- If a generated patch is reported as corrupt or has hunk-count/coordinate
  problems, discard that unpublished artifact, return to the exact target
  checkout, and regenerate mechanically. Never repair hunk headers by hand.
- If the staging signal does not accept the ready commit, verify that the push
  was to `chatgpt-dispatch-stage-<request-id>` and that the final commit newly
  added exactly one matching `.chatgpt-requests/<request-id>/ready.json` file.
- If the transport publisher rejects the stage branch, verify that the branch
  still points at the signaled ready commit, that it was based on `main`, that
  all staged commits are linear, and that every pre-ready change only adds patch
  parts for the same request ID.
- If trusted pre-publish preflight rejects `patch_sha256` or `patch_bytes`, the
  published bytes differ from the builder artifact. Do not repair the staged
  request; abandon it and create a new request from byte-identical builder output.
- If trusted pre-publish preflight reports that `main` advanced, rebuild the
  target branch/request from current `main`; do not merely change
  `baseline_main_sha`.
- If the publisher reports an existing request ID with different content, do
  not overwrite or reuse that ID. Create a new request ID and staging branch.
- If the publisher reports that `chatgpt-dispatch` moved outside the serialized
  publisher, classify the other writer before retrying. Do not force-update the
  transport ref. Old producer logic that writes the shared ref directly must be
  migrated to staging instead of compensated for with a retry loop.
- If a publisher run is canceled before publication, including because the
  bounded Actions concurrency queue is full, do not mutate or recreate the
  staged request. Re-run `gameengine-chatgpt-transport-publisher.yml` from
  `main` with the same immutable `stage_branch` and full `request_commit`.
- If a dispatcher run is canceled after the request was published to
  `chatgpt-dispatch`, re-run `gameengine-chatgpt-dispatcher.yml` from `main`
  with the same published full `request_commit` rather than republishing or
  changing the request record.
- If request-envelope validation fails, compare the request directory and
  `ready.json` with the schema, contiguous part naming, size limits, and exact
  file list in this document.
- If the target HEAD no longer equals `expected_head_sha`, the request is stale.
  Re-read the target branch, regenerate the patch from that exact tree, and use
  a new request ID. Never force-apply or reuse the stale request.
- If `git apply --check` rejects the reconstructed patch, treat the patch/tree
  mismatch as the primary problem. Return to the exact target files and run the
  mechanical builder instead of editing patch text or product code merely to
  make old patch context apply.
- If `git apply --check` reports `No such file or directory` for otherwise
  current target files, treat checkout-path contamination as a producer defect.
  Regenerate from repository root with the builder rather than editing headers.
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

The mechanical builder splits only on newline boundaries. A text-valued
publication API therefore never turns a whitespace-bearing mid-line fragment
into terminal transport text.

Constraints:

- names are contiguous `part-0000.patch`, `part-0001.patch`, ...;
- 1 to 64 parts;
- each part is 1 to 60,000 bytes;
- reconstructed patch is at most 4 MiB;
- request directory contains only listed parts plus `ready.json`;
- files must be normal non-executable Git blobs, not symlinks.

## `ready.json` format

Schema version 2 is required for new normal requests:

```json
{
  "schema_version": 2,
  "request_id": "renderer-aa-20260817-01",
  "target_branch": "chatgpt/gameengine-renderer-aa",
  "expected_head_sha": "0123456789abcdef0123456789abcdef01234567",
  "baseline_main_sha": "89abcdef0123456789abcdef0123456789abcdef",
  "patch_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "patch_bytes": 12345,
  "patch_parts": [
    "part-0000.patch",
    "part-0001.patch"
  ],
  "commit_message": "Add shared renderer anti-aliasing",
  "pr_title": "GameEngine: add shared renderer anti-aliasing",
  "pr_body": "## Summary\n\nAdds the shared renderer anti-aliasing path."
}
```

Schema version 1 remains accepted only for already-staged or legacy-compatible
requests during migration. New producer work MUST use the builder and schema 2.

Required validation for all schemas:

- `request_id` matches the request directory;
- `target_branch` is a safe `chatgpt/gameengine-*` ref;
- `expected_head_sha` is a full SHA;
- `commit_message` is a non-empty single line of at most 120 characters;
- `pr_title` is a non-empty single line of at most 200 characters;
- `pr_body` is at most 8,000 characters;
- `patch_parts` exactly matches the files in the request directory;
- the legacy `<!-- gameengine-chatgpt-automation -->` auto-merge marker is
  forbidden in dispatcher PR bodies.

Schema version 2 additionally requires:

- `baseline_main_sha` is the full current `main` SHA captured for the request;
- the target HEAD contains that baseline;
- current remote `main` still equals `baseline_main_sha` at builder release and
  trusted pre-publish preflight;
- `patch_sha256` is SHA-256 of the complete concatenated patch bytes;
- `patch_bytes` is the exact byte length of that complete patch;
- both values match the bytes reconstructed by the publisher and dispatcher.

## Transport publisher safety checks

Before advancing the shared transport branch, the trusted publisher performs all
of the following:

1. Verifies the staging signal succeeded, came from a push on this repository,
   and the branch is exactly `chatgpt-dispatch-stage-<request-id>`. An explicit
   recovery dispatch instead supplies the same immutable stage branch and full
   ready commit directly from `main`.
2. Re-fetches the staging ref and requires it still to equal the signaled or
   explicitly supplied full ready commit SHA.
3. Finds the staging branch's `main` base and rejects merge commits or any
   staged change outside `.chatgpt-requests/<request-id>/`.
4. Requires every pre-ready change to add only immutable `part-NNNN.patch`
   files and the final commit to add only `ready.json`.
5. Verifies contiguous part naming, normal file modes, per-part size limits,
   total patch size, and matching `request_id`.
6. Loads the request protocol implementation from current `main` and reconstructs
   the staged patch before any transport write.
7. Re-reads the exact target branch and, for schema 2, current `main`; requires
   target HEAD to equal `expected_head_sha`, current `main` to equal
   `baseline_main_sha`, and the target to contain that baseline.
8. For schema 2, requires reconstructed `patch_sha256` and `patch_bytes` to match
   the manifest.
9. Runs `git apply --check --whitespace=error-all` against a detached worktree of
   the exact target, then applies to that temporary index only, runs
   `git diff --cached --check`, validates the public path allow-list, and rejects
   symlink/submodule modes.
10. Rejects request ID reuse when the existing transport request tree differs.
    An identical existing tree is treated idempotently.
11. Reads the latest `chatgpt-dispatch` head only after entering the global
    publisher concurrency group.
12. Cherry-picks the validated staging commits onto that exact head.
13. Re-reads the remote transport head immediately before push and uses an exact
    `--force-with-lease` for the captured head.
14. Starts the trusted dispatcher from `main` with the exact published ready
    commit SHA instead of relying on a push event generated by `GITHUB_TOKEN`.

The transport publisher is globally serialized with `cancel-in-progress: false`
and `queue: max`. GitHub may keep up to 100 publisher runs pending in that group;
without `queue: max`, a newer pending run would replace the older pending run.
The immutable staging branch plus explicit recovery dispatch prevents a canceled
publisher run from requiring a mutable transport retry. This is the concurrency
boundary that prevents ChatGPT request producers from racing one another on the
shared transport ref.

## Dispatcher safety checks

Before publishing a product change, the trusted dispatcher performs all of the
following:

1. Accepts either the legacy read-only `chatgpt-dispatch` signal or an explicit
   trusted `workflow_dispatch` request commit, and requires the selected full
   commit SHA to be reachable from `chatgpt-dispatch`.
2. Verifies the final commit newly added only the declared `ready.json`.
3. Verifies request schema, part names, file modes, counts, and size limits.
4. Accepts schema 1 for migration compatibility and schema 2 for normal current
   requests.
5. Checks out the declared target branch and requires its current HEAD to equal
   `expected_head_sha`.
6. Reconstructs the complete patch and, for schema 2, revalidates
   `patch_sha256`, `patch_bytes`, `baseline_main_sha`, current `main`, and target
   ancestry.
7. Runs `git apply --check --whitespace=error-all`.
8. Applies the patch to the local index only.
9. Rejects `.github/**` and `.chatgpt-requests/**`, and rejects every other
   changed path outside the explicit public GameEngine allow-list, including old
   paths of renames by inspecting the diff with rename detection disabled.
10. Rejects old or new Git modes `120000` (symlink) and `160000` (submodule).
11. Runs `git diff --cached --check`.
12. Re-reads the remote target HEAD immediately before commit/push.
13. Pushes with an exact `--force-with-lease` for `expected_head_sha`.

The dispatcher itself remains globally serialized with `cancel-in-progress:
false` and `queue: max`, retaining up to 100 pending dispatcher runs instead of
replacing older pending requests. The target HEAD checks and lease still reject
changes made by humans, fallback automation, or other external writers.

No stale request is force-applied. A stale request fails and must be rebuilt
from the latest branch state.

## Automation regression suite

`.github/workflows/gameengine-chatgpt-automation-regression.yml` runs the
repository-owned regression suite whenever ChatGPT automation, its protocol
documentation, or the Editor visual-capture harness changes. The suite uses
`.github/chatgpt/test_request_protocol.py` to preserve confirmed incident
lessons as executable checks.

The suite currently covers at least:

- INC-001: strict rejection of added trailing whitespace;
- INC-002: rejection of corrupt or target-misaligned unified-diff hunks;
- INC-003: schema-v2 patch hash mismatch rejection;
- INC-004: stale/current-main baseline mismatch rejection;
- INC-005: checkout-directory-prefixed patch rejection;
- INC-006: Editor capture remains on the application-owned screenshot path, not
  the unsupported eframe helper; and
- INC-007: the transport publisher retains verifying `git rev-parse --verify`
  semantics for optional request-tree existence probes.

When a transport/apply/validation incident produces a durable automation fix,
add a regression when the failure can be reproduced deterministically. The
incident log remains the explanation of the root cause; the regression suite is
the executable prevention layer.

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
5. Re-read the current target branch HEAD, current `main`, and affected files.
6. Prepare the correction in an exact checkout, stage the intended files, and
   create a new schema-v2 request with the mechanical builder.
7. Repeat staging, trusted pre-publish preflight, dispatch, and validation.

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
7. When the durable resolution can be reproduced deterministically in automation,
   add or update a regression so the same defect is rejected before another
   production request depends on memory alone.

The incident log is organized by root cause rather than chronology. Repeated
occurrences of the same problem belong in the same entry so that the repository
accumulates reusable recovery knowledge instead of an ever-growing run diary.

If learning from an incident shows that the protocol itself is incomplete or
incorrect, update this canonical document in the same or a follow-up change.
If it shows that trusted automation under `.github/**` is defective, do not try
to repair that trust boundary through a normal dispatcher patch; use a separate
`chatgpt/gameengine-*` automation-infrastructure branch and Draft PR.

## Fallback

No write-capable fallback is installed in the public repository. If the
publisher or dispatcher is unavailable, stop and repair the trusted path rather
than bypassing its staging, single-writer transport, exact-head, Draft PR, or
validation guarantees.
