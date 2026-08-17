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
  -> chatgpt-producer-stage-<request-id> from current main
  -> connector writes small structured edit-NNNN.json payloads
  -> final .chatgpt-producer/<request-id>/ready.json commit
  -> connector opens a transient producer-signal Issue
  -> trusted default-branch producer (issues: opened)
  -> apply edit plan to an exact target checkout
  -> request_protocol.py build
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
transport publisher and obey the same Dispatcher request and validation
contracts.

The legacy private-repository bridge, local GameEngine GitHub Worker requirement,
auto-merge workflow, and `GameEngine-ChatGPT-Apply` fallback are not part of the
normal public-repository path.

## Trust boundary

Write-capable automation MUST be loaded from trusted `main`. Producer-controlled
branches are data inputs and MUST NOT supply write-capable workflow definitions.

The GitHub connector writes only structured edit-plan data to a dedicated
`chatgpt-producer-stage-<request-id>` branch. It MUST NOT directly modify the
target product branch, create unified patch text, calculate hunk coordinates,
write Dispatcher request parts, or advance `chatgpt-dispatch`.

The producer branch may add only:

```text
.chatgpt-producer/<request-id>/edit-NNNN.json
```

followed by one final commit that newly adds exactly:

```text
.chatgpt-producer/<request-id>/ready.json
```

The connector then opens a transient GitHub Issue whose title and body identify
the immutable producer branch and full ready commit. The `issues` workflow is
loaded from the default branch. The write-capable trusted producer therefore
comes from `main`, not from the producer-controlled branch. The job is gated to
Issue titles beginning with `GameEngine ChatGPT Producer: ` and trusted author
associations `OWNER`, `MEMBER`, or `COLLABORATOR`.

The trusted producer re-fetches the producer branch, target branch, and `main`;
validates exact branch identity, immutable ready commit, linear request history,
current baseline, edit-plan file list, path safety, and edit operation schemas;
then applies the operations to a detached checkout whose HEAD is exactly
`expected_head_sha`.

Only after those operations are staged does trusted automation invoke
`.github/chatgpt/request_protocol.py build`. That existing builder creates the
unified diff mechanically from Git, performs strict patch/path/mode preflight,
rechecks remote target and `main`, splits immutable transport parts on newline
boundaries, and emits schema-v2 hash/byte-count metadata.

The trusted producer publishes only the exact builder output to a fresh
`chatgpt-dispatch-stage-<request-id>` branch. A workflow's own `GITHUB_TOKEN`
push does not start ordinary push-triggered workflows, so it explicitly starts
the trusted transport publisher from `main` with the exact stage branch and full
ready commit SHA.

The transport publisher repeats trusted pre-publish validation before mutating
`chatgpt-dispatch`, globally serializes publication, and uses an exact lease. It
then explicitly starts the trusted Dispatcher from `main`. The Dispatcher again
validates request bytes, baseline, paths, and target HEAD before applying the
product patch.

Normal product requests MUST NOT modify `.github/**` or
`.chatgpt-requests/**`. Connector edit operations are validated by the same
public product-path allow-list and additionally reject absolute paths, backslash
paths, dot components, and `..` traversal.

## Branches

### `chatgpt-producer-stage-<request-id>`

Connector-only immutable edit-plan branch. It MUST be created from the exact
`baseline_main_sha` declared in the producer envelope. Before ready, every commit
must be linear and may only add contiguous edit payload files under the one
request directory. The final commit MUST add only that request's `ready.json`.

After the ready commit is published, the branch is immutable. If it moves before
or during trusted production, processing fails. Corrections use a new request ID
and a new producer branch.

### `chatgpt-dispatch-stage-<request-id>`

Immutable per-request transport staging branch. It starts from the declared
current `main` baseline and contains one or more builder-generated patch-part
addition commits followed by one final Dispatcher `ready.json` addition.

Before ready, commits may add only:

```text
.chatgpt-requests/<request-id>/part-NNNN.patch
```

The final commit adds only:

```text
.chatgpt-requests/<request-id>/ready.json
```

After ready, the branch is immutable. Corrections use a new request ID and stage
branch.

### `chatgpt-dispatch`

Long-lived shared transport branch containing immutable request records under
`.chatgpt-requests/<request-id>/`. The trusted transport publisher is the normal
single writer. Producers MUST NOT update this ref directly.

### `chatgpt/gameengine-*`

All Dispatcher target work branches MUST start with `chatgpt/gameengine-`.
Every request records the exact current target HEAD in `expected_head_sha`.

## Connector edit plan

Each edit payload file contains exactly one JSON object. Payload names are
contiguous `edit-0000.json`, `edit-0001.json`, ... and are applied in that order.
One payload is at most 256,000 bytes; total edit payload is at most 4 MiB.

### Exact text replacement

```json
{
  "operation": "replace_text",
  "path": "crates/example/src/lib.rs",
  "old": "exact existing text",
  "new": "replacement text"
}
```

`old` MUST be non-empty and MUST occur exactly once in the current operation
state of the exact target checkout. Zero or multiple matches reject the request.
This is the normal way to update part of a large existing source file through a
text-only connector without replacing the complete file through Contents API.

### Create UTF-8 text file

```json
{
  "operation": "create_text",
  "path": "docs/example.md",
  "content": "new file contents\n"
}
```

The path MUST not already exist.

### Delete file

```json
{
  "operation": "delete_file",
  "path": "docs/obsolete.md",
  "expected_blob_sha": "0123456789abcdef0123456789abcdef01234567"
}
```

The file MUST exist and its current Git blob SHA at that point in the ordered
edit plan MUST equal `expected_blob_sha`.

The connector producer path is intentionally UTF-8 text oriented. A task that
requires unsupported binary mutation does not silently weaken this contract.

## Connector producer envelope

Producer-envelope schema version 1 contains no patch bytes:

```json
{
  "schema_version": 1,
  "request_id": "renderer-aa-20260817-01",
  "target_branch": "chatgpt/gameengine-renderer-aa",
  "expected_head_sha": "0123456789abcdef0123456789abcdef01234567",
  "baseline_main_sha": "89abcdef0123456789abcdef0123456789abcdef",
  "edit_parts": ["edit-0000.json"],
  "commit_message": "Add shared renderer anti-aliasing",
  "pr_title": "GameEngine: add shared renderer anti-aliasing",
  "pr_body": "## Summary\n\nAdds the shared renderer anti-aliasing path."
}
```

The envelope lives at:

```text
.chatgpt-producer/<request-id>/ready.json
```

`request_id` MUST match branch and directory. `edit_parts` MUST exactly list the
contiguous edit files. `expected_head_sha` and `baseline_main_sha` are full
40-character SHAs. Current target must equal `expected_head_sha`, current `main`
must equal `baseline_main_sha`, and the baseline must be an ancestor of target.

The producer envelope is not a Dispatcher request. The trusted producer applies
its edit plan and then uses the normal mechanical builder to create the
Dispatcher schema-v2 request.

## Connector producer signal

After producer ready, the connector opens an Issue with the exact title:

```text
GameEngine ChatGPT Producer: <request-id>
```

and body:

```json
{
  "signal": "gameengine-chatgpt-producer-v1",
  "request_id": "renderer-aa-20260817-01",
  "producer_branch": "chatgpt-producer-stage-renderer-aa-20260817-01",
  "producer_commit": "0123456789abcdef0123456789abcdef01234567"
}
```

No additional body keys are permitted. The workflow validates repository,
default-branch execution, trusted author association, exact title, signal value,
request/branch identity, and full producer commit SHA.

The Issue is only a signal; it does not authorize paths or product content. On
successful stage publication the trusted producer closes the signal Issue. On
processing failure it leaves the Issue open with a diagnostic pointer so ChatGPT
can inspect the workflow before creating a new immutable request.

## Connector-only lifecycle

For a normal implementation when ChatGPT has only the GitHub connector:

1. Read current `main`, target branch, root `AGENTS.md`, relevant specs/ADRs,
   code style, development workflow, and this protocol.
2. Create or select one `chatgpt/gameengine-*` target branch. Normal main-based
   work starts from current `main`.
3. Capture exact target HEAD and current `main` as `expected_head_sha` and
   `baseline_main_sha`.
4. Create a unique `chatgpt-producer-stage-<request-id>` from that exact current
   `main` baseline.
5. Read exact target file content needed to form precise edit anchors. Create
   only small structured edit payloads on the producer branch; do not rewrite a
   huge existing source through Contents API just to transport a change.
6. Add all contiguous edit payload files before ready.
7. Re-read target and `main`. If either differs from captured SHAs, abandon this
   unpublished producer request and rebuild from current state with a new ID.
8. Add exactly `.chatgpt-producer/<request-id>/ready.json` in a separate final
   commit and no other file in that commit.
9. Re-read producer branch and capture full ready commit SHA. Never modify the
   branch after this point.
10. Open the transient producer-signal Issue with exact title/body above.
11. Trusted producer validates signal authority, producer immutability/history,
    target/main state, edit payloads, and product paths.
12. It applies ordered edits to an exact target checkout and stages resulting
    product state.
13. `request_protocol.py build` mechanically creates/preflights schema-v2 request
    bytes and rechecks remote target/main.
14. Immediately before releasing Dispatcher ready, trusted production again
    checks producer branch, target branch, and `main`. Any movement aborts the
    unpublished stage request.
15. Trusted producer publishes exact builder output to a fresh
    `chatgpt-dispatch-stage-<request-id>` and explicitly starts trusted publisher
    from `main`.
16. Publisher, Dispatcher, Draft PR, Windows Validation, and optional repair
    continue through the existing trusted path.

The connector path MUST NOT fall back to hand-authored unified diff text.
Failures are diagnosed at their responsible layer. Corrections use a new
immutable producer request unless trusted automation itself requires a separate
reviewed infrastructure PR.

## Mechanical request builder

Every new normal Dispatcher request ultimately uses:

```text
.github/chatgpt/request_protocol.py build
```

from an exact checkout of target HEAD. Hand-authored unified diffs and manually
maintained hunk headers are not a normal producer path.

The builder:

1. requires checkout `HEAD` to equal `expected_head_sha`;
2. requires `baseline_main_sha` to be an ancestor of target;
3. rejects unstaged tracked changes and untracked files;
4. obtains patch mechanically from staged Git state and exact target tree;
5. runs `git apply --check --whitespace=error-all` in a detached exact-target
   worktree;
6. applies only to a temporary index, runs `git diff --cached --check`, enforces
   public path allow-list, and rejects symlink/submodule modes;
7. re-reads remote target and `main` before emitting ready unless running the
   explicit regression-test-only bypass;
8. splits only at newline boundaries;
9. computes exact `patch_sha256` and `patch_bytes`; and
10. emits schema-v2 `ready.json` plus contiguous `part-NNNN.patch` files.

Builder output is immutable. It MUST NOT be retyped, reformatted, line-ending
converted, manually re-split, or synthetically reconstructed. A ChatGPT session
that cannot execute the builder directly MUST use connector-only trusted
production rather than requiring a local Worker process or hand-authoring a
patch.

## Direct builder lifecycle

A producer with a trusted real checkout may instead:

1. Read current target and `main`; capture exact full SHAs.
2. Prepare intended change in a disposable exact-target checkout and stage only
   intended files.
3. Run the mechanical request builder.
4. Create unique `chatgpt-dispatch-stage-<request-id>` from current `main`.
5. Publish exact builder-emitted parts without transformation.
6. Re-read target and `main`; if either moved, abandon unpublished request,
   rebuild from current state, and use a new ID.
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

All schemas require matching request ID, safe target, full target SHA, bounded
one-line commit message/title, PR body at most 8,000 characters, exact patch-part
list, and no legacy auto-merge authorization marker. Schema v2 additionally
requires current-main baseline identity/ancestry and exact reconstructed patch
SHA-256/byte count.

## Patch parts

Patch transport is byte-preserving concatenation. Dispatcher concatenates parts
in declared order without separators.

Constraints:

- contiguous `part-0000.patch`, `part-0001.patch`, ...;
- 1 to 64 parts;
- each 1 to 60,000 bytes;
- reconstructed patch at most 4 MiB;
- request directory contains only declared parts plus `ready.json`;
- normal non-executable blobs, not symlinks.

Published bytes MUST be identical to builder-preflighted bytes.

## Transport publisher safety checks

Before advancing `chatgpt-dispatch`, trusted publisher:

1. accepts successful immutable stage signal or explicit trusted recovery
   dispatch with stage branch and full ready commit;
2. requires stage HEAD still equal exact ready commit;
3. requires linear history based on `main`;
4. allows pre-ready commits to add only patch parts and final commit only ready;
5. validates names, modes, counts, sizes, schema, request ID;
6. loads controls from current `main`;
7. re-reads target/current `main` and validates v2 baseline ancestry;
8. verifies reconstructed hash and byte count;
9. runs strict exact-target applicability, `git diff --cached --check`, path
   allow-list, symlink/submodule checks;
10. rejects differing request-ID reuse and treats identical content idempotently;
11. enters global publisher concurrency before selecting latest transport HEAD;
12. cherry-picks validated immutable request;
13. re-reads remote transport before push and uses exact lease; and
14. explicitly starts trusted Dispatcher from `main` with published full ready
    commit SHA.

Publisher concurrency uses `cancel-in-progress: false` and `queue: max`.
Canceled/queued publication is replayed with same immutable stage branch and full
ready commit. Producers never bypass transport serialization.

## Dispatcher safety checks

Trusted Dispatcher:

1. requires selected full request commit reachable from `chatgpt-dispatch`;
2. verifies final commit added only declared ready;
3. validates schema, part names, modes, counts, sizes;
4. accepts schema 1 only for migration compatibility and schema 2 for normal
   current requests;
5. requires current target HEAD equal `expected_head_sha`;
6. reconstructs patch and revalidates v2 hash, bytes, baseline, current `main`,
   ancestry;
7. runs `git apply --check --whitespace=error-all`;
8. applies to local index only;
9. rejects `.github/**`, `.chatgpt-requests/**`, and every path outside public
   allow-list, including old rename paths with rename detection disabled;
10. rejects modes `120000` and `160000`;
11. runs `git diff --cached --check`;
12. re-reads target HEAD immediately before commit/push; and
13. pushes with exact lease for `expected_head_sha`.

Dispatcher concurrency also uses `cancel-in-progress: false` and `queue: max`.
No stale request is force-applied.

## Failure diagnosis before retry

Choose recovery by failing layer:

- Producer-signal Issue rejected: verify exact title/JSON, trusted author
  association, matching branch/request ID, full producer commit.
- Producer edit payload rejected: fix operation schema, exact text anchor, safe
  path, expected delete blob, or contiguous payload list in a new request.
- Producer branch moved after ready: never rewrite it; use a new request ID.
- Stale target/main: rebuild from current state; never replace only SHA fields.
- Builder rejected staged product state: fix actual edit intent; never hand-edit
  generated diff.
- Corrupt/misaligned patch: discard unpublished artifact and regenerate from
  exact target; never repair hunk headers manually.
- Publisher rejected stage: verify immutable stage shape, ready commit, part
  sequence/sizes/schema, and exact builder bytes.
- Hash/byte mismatch: abandon unpublished request and rebuild with new ID.
- Transport moved outside serialization: identify external writer; never
  force-update from producer.
- Publisher canceled before publication: replay from `main` with same immutable
  stage branch/full ready commit.
- Dispatcher canceled after transport publication: replay same published full
  request commit; do not republish.
- Dispatcher failed after successfully pushing the exact product commit but before
  Draft PR or Windows Validation reconciliation: do not replay normal apply and do
  not republish. After the trusted recovery workflow is available on `main`, invoke
  `gameengine-chatgpt-dispatcher-recovery.yml` with the same published full request
  commit. A producer with workflow-dispatch capability may use its
  `request_commit` input. A connector-only producer instead opens a transient Issue
  titled `GameEngine ChatGPT Dispatcher Recovery: <full-request-commit>` whose body
  is exactly `{"signal":"gameengine-chatgpt-dispatcher-recovery-v1","request_commit":"<full-request-commit>"}`.
  The Issue trigger accepts only `OWNER`, `MEMBER`, or `COLLABORATOR` authors and
  closes the signal only after successful reconciliation. Recovery is schema-v2
  only, has no product `contents: write` permission, proves the target is the
  one-parent child of `expected_head_sha`, requires the exact reconstructed request
  patch to equal that commit's diff bytes, preserves baseline ancestry even if
  `main` advanced afterward, then only reconciles the Draft PR and exact-head
  Windows Validation.
- Dispatcher path rejection: do not weaken product allow-list. Trusted automation
  changes require separate infrastructure branch/Draft PR.
- Windows Validation failure: read summary mode/scope, job logs, diagnostics, and
  external-service state before deciding code changes.
- GitHub/runner/network/dependency external failure: do not create speculative
  product changes to compensate.

Confirmed reusable automation failures belong in
`docs/CHATGPT_AUTOMATION_INCIDENTS.md`; it supplements but does not override this
protocol.

## Automation regression suite

`.github/workflows/gameengine-chatgpt-automation-regression.yml` discovers
`test_*_protocol.py` whenever automation/protocol paths change. Coverage includes
existing INC-001 through INC-007 protections plus connector edit-plan generation,
create/delete behavior, trust-boundary/traversal rejection, exact-match guards,
immutable producer-head checks, and default-branch Issue-signal handoff.

A deterministic durable automation fix SHOULD gain a regression when practical.

## Pull request rules

After successful product push, Dispatcher creates/reuses one open PR whose head
is target branch.

- base MUST be `main`;
- PR MUST remain Draft;
- existing non-Draft PRs are converted back to Draft;
- PR body carries `<!-- gameengine-chatgpt-dispatcher -->`;
- legacy auto-merge authorization forbidden;
- Dispatcher never merges or enables auto-merge.

## Windows Validation

Dispatcher explicitly starts `gameengine-windows-validation.yml` from `main`
with target branch, PR number, exact pushed HEAD, and request ID. Validation
resolves remote target and requires exact supplied SHA before checkout.

Modes are `affected`, `full`, and `docs`.

### `affected`

Safely classifiable crate changes use Cargo metadata to select changed workspace
packages. Formatting plus selected-package Clippy, tests, and documentation run.
Success means selected affected scope passed, not whole workspace.

### `full`

Mandatory for nightly, workspace manifest/lock/toolchain, validation/build/
automation infrastructure, unknown/deleted package paths, and unclassifiable
changes. Runs:

```text
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

### `docs`

Recognized docs-only PR/merge/main fast paths may skip Rust compilation. Nightly
still supplies full-workspace coverage.

Machine-readable aggregate summary and workflow conclusion are authoritative for
mode and scope.

## ChatGPT repair loop

After dispatch:

1. Read validation `summary.json`, mode, scope.
2. If all required gates succeed, stop and leave PR Draft.
3. On failure inspect relevant logs/diagnostics.
4. Separate code defects from external failures.
5. Re-read target HEAD, current `main`, affected files.
6. Create correction with direct builder or a new immutable connector edit plan.
7. Repeat trusted production/publication, Dispatcher, validation.

Normal automated repair is limited to five rounds. If same essential failure
repeats twice, reassess root-cause hypothesis before retrying.

## Incident learning

After confirmed failure and validated recovery:

1. Search `docs/CHATGPT_AUTOMATION_INCIDENTS.md` by symptom/layer/cause.
2. Update existing root-cause entry when applicable.
3. Add new incident only for materially different confirmed cause.
4. Record symptom, layer, evidence, cause, successful resolution, prevention,
   and durable request/PR/run/commit references.
5. Separate fact from inference.
6. Never record secrets, credentials, private machine paths, large raw logs.
7. Add deterministic executable regression when practical.
8. Update this protocol when lesson applies to all future requests.

Trusted automation defects under `.github/**` are repaired only through a
separate `chatgpt/gameengine-*` automation-infrastructure branch and Draft PR,
never through Dispatcher product path.

## Fallback

No write-capable bypass is installed. If connector producer, trusted producer,
publisher, Dispatcher, or validation is unavailable, diagnose and repair trusted
path instead of bypassing staging, exact-head checks, single-writer transport,
Draft PR, or validation guarantees.
