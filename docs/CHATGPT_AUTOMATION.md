# ChatGPT GitHub Automation

Status: Accepted
Version: 2.1.0
Canonical location: `docs/CHATGPT_AUTOMATION.md`

## Purpose

This document defines the repository-native path used by ChatGPT to apply and
validate GameEngine changes without Codex and without the OpenAI API.

For a ChatGPT session that has the GitHub connector but no repository command
execution environment, the normal path is:

```text
ChatGPT + GitHub connector
  -> exact chatgpt/gameengine-* target branch
  -> chatgpt-producer-stage-<request-id> from the exact target HEAD
  -> connector writes intended final product files
  -> final .chatgpt-producer/<request-id>/ready.json commit
  -> connector opens a transient producer-signal Issue
  -> trusted default-branch producer (issues: opened)
  -> request_protocol.py build on an exact target checkout
  -> chatgpt-dispatch-stage-<request-id>
  -> trusted transport publisher + pre-publish preflight
  -> chatgpt-dispatch
  -> trusted dispatcher
  -> target branch commit/push
  -> Draft pull request
  -> GameEngine Windows Validation
  -> ChatGPT reads validation result/logs
  -> optional corrected request
```

A producer that already has a real Git checkout and command execution MAY run
`request_protocol.py build` directly and publish its exact output to a
`chatgpt-dispatch-stage-*` branch. Both paths converge before the trusted
transport publisher and obey the same Dispatcher request schema and validation
contracts.

The legacy private-repository bridge, local GameEngine GitHub Worker requirement,
auto-merge workflow, and `GameEngine-ChatGPT-Apply` fallback are not part of the
normal public-repository path.

## Trust boundary

Write-capable automation MUST be loaded from trusted `main`. Producer-controlled
branches are data inputs and MUST NOT supply write-capable workflow definitions.

The GitHub connector may write intended product files only to a dedicated
`chatgpt-producer-stage-<request-id>` branch. It MUST NOT directly write
`chatgpt-dispatch`, Dispatcher request patch files, patch hashes, multipart
boundaries, or Dispatcher `ready.json`.

The producer branch ends with one commit that newly adds exactly:

```text
.chatgpt-producer/<request-id>/ready.json
```

The connector then opens a transient GitHub Issue whose body contains only the
producer branch and exact ready commit signal. GitHub's `issues` event resolves
against the default branch, so the write-capable trusted producer is supplied by
`main`, not by the producer branch. The trusted workflow accepts connector
signals only from an Issue whose author association is `OWNER`, `MEMBER`, or
`COLLABORATOR`.

The trusted producer re-fetches the producer branch, target branch, and `main`;
verifies exact branch identity, immutable ready commit, linear history, current
baseline, and public product paths; mechanically derives the product diff from
the exact target tree to the producer product head; stages that state in a
detached exact-target worktree; and invokes
`.github/chatgpt/request_protocol.py build`.

The trusted producer then publishes only the exact builder output to a fresh
`chatgpt-dispatch-stage-<request-id>` branch. A workflow's own `GITHUB_TOKEN`
push does not start ordinary push-triggered workflows, so the trusted producer
explicitly starts the trusted transport publisher from `main` with the exact
stage branch and full ready commit SHA.

The trusted transport publisher performs its own pre-publish validation before
mutating `chatgpt-dispatch`, globally serializes transport publication, and uses
an exact lease. The publisher explicitly starts the trusted Dispatcher from
`main`; the Dispatcher repeats request validation before applying any product
patch.

Normal product requests MUST NOT modify `.github/**` or
`.chatgpt-requests/**`. Connector producer product commits also MUST NOT modify
`.github/**`, `.chatgpt-requests/**`, or `.chatgpt-producer/**`; the only allowed
`.chatgpt-producer/**` change is the final ready marker commit.

## Branches

### `chatgpt-producer-stage-<request-id>`

Connector-only producer branch. It MUST start from the exact target HEAD declared
in the producer envelope. Product commits before ready MUST be linear non-merge
commits and may change only normal product paths accepted by the public path
allow-list.

The final producer commit MUST newly add exactly:

```text
.chatgpt-producer/<request-id>/ready.json
```

After the ready commit is published, the producer branch is immutable. If it
moves before or during trusted production, the request is rejected. Corrections
use a new request ID and a new producer branch.

### `chatgpt-dispatch-stage-<request-id>`

Immutable per-request transport staging branch. It starts from the declared
current `main` baseline and contains one or more patch-part addition commits
followed by one final Dispatcher `ready.json` addition.

Before ready, commits may add only:

```text
.chatgpt-requests/<request-id>/part-NNNN.patch
```

The final commit adds only:

```text
.chatgpt-requests/<request-id>/ready.json
```

After ready, the branch is immutable. Corrections use a new request ID and a new
stage branch.

### `chatgpt-dispatch`

Long-lived shared transport branch containing immutable request records under
`.chatgpt-requests/<request-id>/`. The trusted transport publisher is the normal
single writer. Producers MUST NOT update this ref directly.

### `chatgpt/gameengine-*`

All Dispatcher target work branches MUST start with `chatgpt/gameengine-`.
Every request records the exact current target HEAD in `expected_head_sha`.

## Connector producer envelope

The connector-only path uses a small producer envelope. It describes intent and
baseline identity but contains no patch bytes.

Producer-envelope schema version 1:

```json
{
  "schema_version": 1,
  "request_id": "renderer-aa-20260817-01",
  "target_branch": "chatgpt/gameengine-renderer-aa",
  "expected_head_sha": "0123456789abcdef0123456789abcdef01234567",
  "baseline_main_sha": "89abcdef0123456789abcdef0123456789abcdef",
  "commit_message": "Add shared renderer anti-aliasing",
  "pr_title": "GameEngine: add shared renderer anti-aliasing",
  "pr_body": "## Summary\n\nAdds the shared renderer anti-aliasing path."
}
```

The envelope lives at:

```text
.chatgpt-producer/<request-id>/ready.json
```

`request_id` MUST match the producer branch and directory.
`expected_head_sha` and `baseline_main_sha` MUST be full 40-character SHAs.
The target must still equal `expected_head_sha`, current `main` must still equal
`baseline_main_sha`, and the baseline must be an ancestor of the target.

The producer envelope is not a Dispatcher request. The trusted producer turns
the producer branch's mechanically-derived final product state into the normal
Dispatcher schema-v2 request.

## Connector producer signal

After the producer ready commit, the connector opens a temporary Issue whose
body is exactly this JSON object:

```json
{
  "signal": "gameengine-chatgpt-producer-v1",
  "request_id": "renderer-aa-20260817-01",
  "producer_branch": "chatgpt-producer-stage-renderer-aa-20260817-01",
  "producer_commit": "0123456789abcdef0123456789abcdef01234567"
}
```

No additional keys are permitted. The workflow validates that:

- the event is in `KdGithubIt/GameEngine`;
- the workflow is running from `main`;
- Issue author association is `OWNER`, `MEMBER`, or `COLLABORATOR`;
- signal value is `gameengine-chatgpt-producer-v1`;
- request ID matches the producer branch;
- producer commit is a full SHA.

The Issue is only a signal. It does not authorize product paths, patch bytes, or
trust-boundary changes. Those are independently validated from repository state.
On successful stage publication the trusted producer closes the signal Issue. On
failure it leaves the Issue open and comments that the workflow run must be
inspected before creating a new immutable request.

## Connector-only producer lifecycle

For normal implementation when ChatGPT only has the GitHub connector:

1. Read current `main`, the target branch, root `AGENTS.md`, relevant
   specifications/ADRs, code style, development workflow, and this protocol.
2. Create or select one `chatgpt/gameengine-*` target branch. Normal main-based
   work starts from current `main`.
3. Capture exact target HEAD and current `main` as `expected_head_sha` and
   `baseline_main_sha`.
4. Create a unique `chatgpt-producer-stage-<request-id>` branch from that exact
   target HEAD.
5. Through the connector, write the intended final product files to the producer
   branch. Before complete-file replacement, read the current branch version and
   later review the resulting diff. Do not write trust-boundary paths.
6. Re-read target and `main`. If either differs from the captured SHAs, abandon
   this unpublished producer request and rebuild from current state with a new
   request ID.
7. Add exactly `.chatgpt-producer/<request-id>/ready.json` in a separate final
   commit and change no other file in that commit.
8. Re-read the producer branch and capture its full ready commit SHA. Do not
   modify the producer branch after this point.
9. Open the transient producer-signal Issue with the exact JSON shape above.
10. The trusted producer validates signal authority, immutable producer history,
    target/main state, path safety, and final ready shape.
11. It mechanically derives the product diff, stages it on an exact target
    checkout, and runs `request_protocol.py build`.
12. The builder performs strict patch applicability, whitespace checks,
    path/mode validation, remote target/main rechecks, newline-safe multipart
    splitting, and schema-v2 hash/byte-count generation.
13. Immediately before releasing Dispatcher ready, trusted production rechecks
    producer branch, target branch, and `main`. Any movement aborts the
    unpublished stage request.
14. The trusted producer writes exact builder output to a fresh
    `chatgpt-dispatch-stage-<request-id>` and explicitly starts the trusted
    transport publisher from `main`.
15. Publisher, Dispatcher, Draft PR creation, Windows Validation, and optional
    repair continue through the normal trusted path.

The connector path MUST NOT fall back to hand-authored patch text. A failure is
diagnosed at its responsible layer; corrections use a new immutable producer
request unless trusted automation itself requires a separately reviewed
infrastructure change.

## Mechanical request builder

Every new normal Dispatcher request MUST ultimately be produced with:

```text
.github/chatgpt/request_protocol.py build
```

from an exact checkout of the target HEAD. Hand-authored unified diffs and
manually maintained hunk headers are not a normal producer path.

The builder:

1. requires checkout `HEAD` to equal `expected_head_sha`;
2. requires `baseline_main_sha` to be an ancestor of that target;
3. rejects unstaged tracked changes and untracked files;
4. obtains the patch mechanically from staged Git state and the exact target
   tree;
5. runs `git apply --check --whitespace=error-all` in a detached exact-target
   worktree;
6. applies only to a temporary index, runs `git diff --cached --check`, enforces
   the public path allow-list, and rejects symlink/submodule modes;
7. re-reads remote target and `main` before emitting ready unless running the
   explicit regression-test-only bypass;
8. splits only at newline boundaries;
9. computes exact `patch_sha256` and `patch_bytes`; and
10. emits schema-v2 `ready.json` plus contiguous `part-NNNN.patch` files.

Builder output is immutable. It MUST NOT be retyped, reformatted, line-ending
converted, manually re-split, or synthetically reconstructed. A ChatGPT session
that cannot execute the builder directly MUST use the connector-only trusted
producer path instead of requiring a local Worker process or hand-authoring a
patch.

## Direct builder lifecycle

A producer that already has a trusted real checkout may use this direct path:

1. Read current target and `main`; capture exact full SHAs.
2. Prepare the intended product change in a disposable exact-target checkout and
   stage exactly the intended files.
3. Run the mechanical request builder.
4. Create a unique `chatgpt-dispatch-stage-<request-id>` from current `main`.
5. Publish exact builder-emitted parts without transformation.
6. Re-read target and `main`; if either moved, abandon the unpublished request,
   rebuild from current state, and use a new request ID.
7. Add exact builder-emitted `ready.json` in its own final commit.
8. Continue through publisher, Dispatcher, Draft PR, and validation.

A stale request is never force-applied. SHA fields are never replaced without
regenerating the corresponding product patch.

## Dispatcher `ready.json`

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
  "patch_parts": ["part-0000.patch"],
  "commit_message": "Add shared renderer anti-aliasing",
  "pr_title": "GameEngine: add shared renderer anti-aliasing",
  "pr_body": "## Summary\n\nAdds the shared renderer anti-aliasing path."
}
```

Schema version 1 remains accepted only for already-staged or legacy-compatible
requests during migration. New producer work MUST use schema 2.

All request schemas require matching request ID, safe target branch, full target
SHA, bounded one-line commit message/title, PR body at most 8,000 characters,
exact contiguous patch-part list, and no legacy auto-merge authorization marker.

Schema v2 additionally requires current-main baseline identity and ancestry plus
exact reconstructed patch SHA-256 and byte count.

## Patch parts

Patch transport is byte-preserving concatenation. The Dispatcher concatenates
parts in declared order without adding separators.

Constraints:

- contiguous names `part-0000.patch`, `part-0001.patch`, ...;
- 1 to 64 parts;
- each part 1 to 60,000 bytes;
- reconstructed patch at most 4 MiB;
- request directory contains only declared parts plus `ready.json`;
- files are normal non-executable blobs, not symlinks.

The published artifact MUST be byte-identical to the artifact that passed
builder preflight.

## Transport publisher safety checks

Before advancing `chatgpt-dispatch`, the trusted publisher:

1. accepts a successful stage signal or explicit trusted recovery dispatch with
   the immutable stage branch and full ready commit;
2. requires stage HEAD still to equal that exact commit;
3. requires linear staged history based on `main`;
4. permits pre-ready commits to add only patch parts and the final commit to add
   only `ready.json`;
5. validates names, modes, counts, sizes, schema, and request ID;
6. loads request controls from current `main`;
7. re-reads target and current `main` and validates schema-v2 baseline ancestry;
8. verifies reconstructed hash and byte count;
9. runs strict exact-target applicability, `git diff --cached --check`, public
   path allow-list, and symlink/submodule checks;
10. rejects request-ID reuse when existing transport content differs and treats
    identical existing content idempotently;
11. enters global publisher concurrency before selecting latest transport HEAD;
12. cherry-picks the validated immutable request onto that head;
13. re-reads remote transport immediately before push and uses an exact lease;
14. explicitly starts the trusted Dispatcher from `main` with the published full
    ready commit SHA.

Publisher concurrency uses `cancel-in-progress: false` and `queue: max`.
Canceled/queued publication is replayed using the same immutable stage branch and
full ready commit. Producers do not bypass transport contention by writing the
shared ref.

## Dispatcher safety checks

The trusted Dispatcher:

1. requires the selected full request commit to be reachable from
   `chatgpt-dispatch`;
2. verifies the final commit added only the declared `ready.json`;
3. validates request schema, part names, modes, counts, and sizes;
4. accepts schema 1 only for migration compatibility and schema 2 for current
   normal requests;
5. checks out the declared target and requires current HEAD to equal
   `expected_head_sha`;
6. reconstructs the patch and revalidates v2 hash, byte count, baseline, current
   `main`, and ancestry;
7. runs `git apply --check --whitespace=error-all`;
8. applies to the local index only;
9. rejects `.github/**`, `.chatgpt-requests/**`, and every path outside the
   public GameEngine allow-list, including old rename paths with rename detection
   disabled;
10. rejects old/new modes `120000` and `160000`;
11. runs `git diff --cached --check`;
12. re-reads target HEAD immediately before commit/push; and
13. pushes with an exact lease for `expected_head_sha`.

Dispatcher concurrency also uses `cancel-in-progress: false` and `queue: max`.
No stale request is force-applied.

## Failure diagnosis before retry

Choose recovery by failing layer instead of making speculative changes:

- Producer-signal Issue rejected: verify exact signal JSON, trusted author
  association, matching branch/request ID, and full producer commit SHA.
- Trusted producer rejects branch: verify producer branch is still at the
  signaled ready commit, is based on `expected_head_sha`, has linear product
  history, changes only allowed product paths, and ends with ready-only commit.
- Trusted producer reports stale target/main: abandon the unpublished request and
  rebuild from current state with a new request ID. Do not replace only SHA
  fields.
- Builder rejects product state: correct the exact product state or baseline;
  never hand-edit generated unified diff text.
- Corrupt/misaligned patch: discard the unpublished artifact and regenerate from
  exact target state; never repair hunk headers by hand.
- Publisher rejects staged request: verify immutable stage shape, exact ready
  commit, part sequence, sizes, schema, and exact builder bytes.
- Hash/byte mismatch: published bytes differ from builder output; abandon the
  unpublished request and rebuild with a new ID.
- Transport moved outside publisher serialization: identify the external writer;
  do not force-update or retry with direct producer fast-forward.
- Publisher canceled before publication: replay publisher from `main` with the
  same immutable stage branch and full ready commit.
- Dispatcher canceled after transport publication: replay Dispatcher with the
  same published full request commit; do not republish the request.
- Dispatcher path rejection: do not weaken the product allow-list. Trusted
  automation changes require a separate infrastructure branch and Draft PR.
- Windows Validation failure: read aggregate summary, mode/scope, job logs, and
  diagnostics before deciding whether code changes are justified.
- GitHub, runner, network, or dependency-service failure: do not create product
  changes to compensate for an external failure.

Confirmed reusable automation failures belong in
`docs/CHATGPT_AUTOMATION_INCIDENTS.md`. That log supplements but does not
override this protocol.

## Automation regression suite

`.github/workflows/gameengine-chatgpt-automation-regression.yml` runs when ChatGPT
automation, protocol documentation, or the Editor visual-capture harness changes.
It discovers `test_*_protocol.py`, including:

- `.github/chatgpt/test_request_protocol.py` for existing incident protections
  and schema-v2 request-builder behavior; and
- `.github/chatgpt/test_producer_protocol.py` for connector producer generation,
  trust-boundary path rejection, immutable producer-head checks, and trusted
  default-branch Issue-signal handoff.

A deterministic durable automation fix SHOULD gain a regression when practical.

## Pull request rules

After successful product push, the Dispatcher creates or reuses the one open PR
whose head is the target branch.

- base MUST be `main`;
- PR MUST remain Draft;
- existing non-Draft PRs are converted back to Draft before validation;
- Dispatcher PR body carries `<!-- gameengine-chatgpt-dispatcher -->`;
- legacy auto-merge authorization is forbidden;
- Dispatcher never merges and never enables auto-merge.

## Windows Validation

The Dispatcher explicitly starts `gameengine-windows-validation.yml` from
`main` with target branch, PR number, exact pushed HEAD, and request ID. The
validation workflow resolves the remote branch and requires its current HEAD to
equal the supplied SHA before checkout.

Validation modes are `affected`, `full`, and `docs`.

### `affected`

Safely classifiable crate changes use Cargo metadata to select changed workspace
packages. Formatting plus selected-package Clippy, tests, and documentation are
run. Reverse dependents are not added to the normal PR critical path. Success
means the selected affected scope passed; it is not a full-workspace claim.

### `full`

Full mode is mandatory for nightly validation, workspace manifest/lock/toolchain
changes, validation/build/automation infrastructure, unknown or deleted package
paths, and any change that cannot be classified safely.

Full mode runs:

```text
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

### `docs`

Recognized documentation-only changes may skip Rust compilation on PR,
merge-group, and main fast paths. Nightly still supplies full-workspace coverage.

The machine-readable aggregate summary and workflow conclusion are authoritative
for selected validation mode and scope.

## ChatGPT repair loop

After dispatch:

1. Read validation `summary.json`, selected mode, and scope.
2. If every gate required by that mode succeeds, stop and leave the PR Draft.
3. On failure, inspect relevant job logs and diagnostics.
4. Separate code defects from GitHub/runner/network/dependency failures.
5. Re-read target HEAD, current `main`, and affected files.
6. For a correction, create a new schema-v2 request using either the direct
   builder path or a new immutable connector producer request.
7. Repeat trusted production/publication, Dispatcher, and validation.

Normal automated repair is limited to five rounds. If the same essential
failure repeats twice, reassess the root-cause hypothesis before retrying again.

## Incident learning

After a failure is understood and recovery is validated:

1. Search `docs/CHATGPT_AUTOMATION_INCIDENTS.md` by symptom, layer, and cause.
2. Update an existing root-cause entry when it already covers the failure.
3. Add a new incident only for a materially different confirmed root cause.
4. Record symptom, layer, evidence, root cause, successful resolution,
   prevention/next action, and durable request/PR/run/commit references.
5. Separate confirmed facts from inference.
6. Do not record secrets, credentials, private machine paths, or large raw logs.
7. Add an executable regression when durable prevention is deterministic.
8. Update this canonical protocol when the lesson applies to all future
   requests.

Trusted automation defects under `.github/**` are repaired only through a
separate `chatgpt/gameengine-*` automation-infrastructure branch and Draft PR,
never through the Dispatcher product path.

## Fallback

No write-capable bypass is installed. If connector producer, trusted producer,
publisher, Dispatcher, or validation is unavailable, diagnose and repair the
trusted path instead of bypassing staging, exact-head checks, single-writer
transport, Draft PR, or validation guarantees.
