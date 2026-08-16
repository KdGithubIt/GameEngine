# ChatGPT GitHub Automation

Status: Accepted
Version: 2.1.0
Canonical location: `docs/CHATGPT_AUTOMATION.md`

## Purpose

This document defines the repository-native path used by ChatGPT to apply and
validate GameEngine changes without Codex and without the OpenAI API.

For ChatGPT sessions that have the GitHub connector but no repository command
execution environment, the normal path is:

```text
ChatGPT + GitHub connector
  -> exact chatgpt/gameengine-* target branch
  -> chatgpt-producer-stage-<request-id> from that exact target HEAD
  -> connector writes intended product files as normal Git commits
  -> final .chatgpt-producer/<request-id>/ready.json commit
  -> read-only producer stage signal
  -> trusted default-branch producer
  -> request_protocol.py build on an exact target checkout
  -> chatgpt-dispatch-stage-<request-id>
  -> trusted transport publisher + pre-publish preflight
  -> chatgpt-dispatch
  -> trusted dispatcher
  -> target branch commit/push
  -> Draft pull request
  -> GameEngine Windows Validation
  -> ChatGPT reads the validation result
  -> optional corrected request
```

A producer that already has a real Git checkout and command execution MAY use
`request_protocol.py build` directly and publish its exact output to a
`chatgpt-dispatch-stage-*` branch. Both paths converge before the trusted
transport publisher and obey the same request schema and validation contracts.

The legacy private-repository bridge, auto-merge workflow, and
`GameEngine-ChatGPT-Apply` fallback are intentionally not installed here.

## Trust boundary

Write-capable automation MUST be loaded from the trusted default branch.
Producer-controlled branches are data inputs, not trusted executable control.

A connector-only producer may write product files to a dedicated
`chatgpt-producer-stage-<request-id>` branch, but it MUST NOT write the shared
`chatgpt-dispatch` ref. The final producer commit adds only
`.chatgpt-producer/<request-id>/ready.json`. A push-triggered producer signal has
only `contents: read`. Its successful `workflow_run` causes GitHub to load the
write-capable trusted producer from `main`.

The trusted producer re-fetches the exact producer branch, target branch, and
`main`; verifies immutable branch identity and linear history; rejects product
changes outside the public allow-list or inside trust-boundary paths; derives the
product diff mechanically from the exact target tree to the producer product
head; stages that state in a detached exact-target worktree; and invokes
`.github/chatgpt/request_protocol.py build`. The connector never supplies a
hand-authored unified diff, hunk coordinate, patch hash, multipart boundary, or
Dispatcher `ready.json`.

The trusted producer publishes only the exact builder output to a fresh
`chatgpt-dispatch-stage-<request-id>` branch. Because a push made with a
workflow's own `GITHUB_TOKEN` does not start ordinary push-triggered workflows,
the trusted producer explicitly starts the transport publisher from `main` with
that immutable stage branch and full ready commit SHA.

Direct request producers follow the older staging entry point: they run the same
mechanical builder in a real exact-target checkout and publish its immutable
output to `chatgpt-dispatch-stage-<request-id>`. They MUST NOT advance
`chatgpt-dispatch` directly.

The trusted transport publisher validates the complete staged request and runs
trusted pre-publish preflight before any transport mutation. It then serializes
publication onto the latest `chatgpt-dispatch` head and pushes with an exact
lease. The dispatcher is started explicitly from `main` with the published full
ready commit SHA and repeats the request checks before applying product changes.

Workflow files, automation controls, and transport data are separate from normal
product patches. Normal Dispatcher product patches MUST NOT modify `.github/**`
or `.chatgpt-requests/**`. Connector producer branches also MUST NOT modify
`.github/**` or `.chatgpt-requests/**`; `.chatgpt-producer/**` is reserved for
its final producer ready marker.

## Branches

### `chatgpt-producer-stage-<request-id>`

Connector-only producer branch. It MUST be created from the exact current target
HEAD declared by the producer envelope. Product commits before ready MUST be
linear non-merge commits and may change only normal product paths accepted by
`request_protocol.py`. The final commit MUST newly add exactly:

```text
.chatgpt-producer/<request-id>/ready.json
```

After the ready commit is pushed, the branch is immutable. If it moves before or
during trusted production, the request is rejected. Corrections use a new
request ID and a new producer branch.

### `chatgpt-dispatch-stage-<request-id>`

Immutable per-request transport staging branch. It is created from the declared
current `main` baseline and contains one or more patch-part addition commits
followed by one final `ready.json` addition. Before ready, commits may add only:

```text
.chatgpt-requests/<request-id>/part-NNNN.patch
```

The final commit adds only:

```text
.chatgpt-requests/<request-id>/ready.json
```

After ready the branch is immutable. Corrections use a new request ID and stage
branch.

### `chatgpt-dispatch`

Long-lived shared transport branch containing immutable request records under
`.chatgpt-requests/<request-id>/`. The trusted transport publisher is the normal
single writer. Producers MUST NOT create commits on this branch or update its
ref directly.

### `chatgpt/gameengine-*`

All target work branches MUST start with `chatgpt/gameengine-`. A request for any
other target branch is rejected. Every request records the exact full target HEAD
SHA in `expected_head_sha`.

## Connector producer envelope

The connector-only path uses a small producer envelope that describes intent but
contains no patch bytes. Schema version 1 is the current producer-envelope
schema:

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

The envelope path is
`.chatgpt-producer/<request-id>/ready.json`. Its request ID MUST match the branch
name. `expected_head_sha` and `baseline_main_sha` MUST be full 40-character SHAs.
The target must still equal `expected_head_sha`, current `main` must still equal
`baseline_main_sha`, and that baseline must be an ancestor of the target.

The producer envelope is not a Dispatcher request schema. The trusted producer
turns the producer branch's mechanically-derived final product state into the
normal Dispatcher schema-v2 request.

## Connector-only producer lifecycle

For a normal implementation when ChatGPT only has the GitHub connector:

1. Read current `main`, the target branch, `AGENTS.md`, relevant specifications,
   ADRs, code style, development workflow, and this protocol.
2. Create or select one `chatgpt/gameengine-*` target branch. For normal
   main-based work, create it from current `main`.
3. Capture exact target HEAD and current `main` as `expected_head_sha` and
   `baseline_main_sha`.
4. Create a unique `chatgpt-producer-stage-<request-id>` branch from that exact
   target HEAD.
5. Through the GitHub connector, write the intended final product files to that
   producer branch. Complete-file updates are permitted only after reading the
   current branch version and reviewing the resulting diff. Do not write
   `.github/**`, `.chatgpt-requests/**`, or `.chatgpt-producer/**` as product
   edits.
6. Re-read target and `main`. If either differs from the captured SHAs, abandon
   the unpublished producer request and rebuild from current state with a new ID.
7. Add exactly `.chatgpt-producer/<request-id>/ready.json` in a separate final
   commit. Do not change any other file in that commit.
8. The read-only producer signal validates the marker shape. The trusted
   default-branch producer then revalidates the complete producer history and
   branch immutability.
9. The trusted producer mechanically derives the exact product diff, stages it
   on an exact target checkout, and runs `request_protocol.py build`. That
   builder performs strict patch preflight, path/mode validation, remote
   target/main rechecks, newline-safe multipart splitting, and schema-v2 hash and
   byte-count generation.
10. Immediately before releasing the Dispatcher ready marker, trusted production
    rechecks producer branch, target branch, and `main`. Any movement aborts the
    unpublished stage request.
11. The trusted producer writes the exact builder output to a fresh
    `chatgpt-dispatch-stage-<request-id>` and explicitly starts the trusted
    transport publisher from `main`.
12. Publisher, Dispatcher, Draft PR creation, Windows Validation, and optional
    repair continue through the normal trusted path.

The connector path MUST NOT fall back to hand-authored patches when trusted
production fails. Diagnose the failing layer and either create a corrected new
producer request or repair trusted automation in a separate infrastructure PR.

## Mechanical request builder

New normal Dispatcher requests MUST be produced with
`.github/chatgpt/request_protocol.py build` from an exact checkout of the target
HEAD. Hand-authored unified diffs and manually maintained hunk headers are not a
normal producer path.

The producer prepares the intended product state in a disposable checkout and
stages exactly the intended files in Git. The builder:

1. requires checkout `HEAD` to equal the supplied full `expected_head_sha`;
2. requires `baseline_main_sha` to be an ancestor of that target;
3. rejects unstaged tracked changes and untracked files;
4. obtains the patch mechanically from the staged index and exact target tree;
5. runs `git apply --check --whitespace=error-all` in a detached target worktree;
6. applies only to a temporary index, runs `git diff --cached --check`, enforces
   the public path allow-list, and rejects symlink/submodule modes;
7. re-reads remote target and `main` before emitting ready unless using the
   regression-test-only bypass;
8. splits only at newline boundaries;
9. computes exact `patch_sha256` and `patch_bytes`; and
10. emits schema-v2 `ready.json` plus contiguous `part-NNNN.patch` files.

Those emitted bytes are immutable. They MUST NOT be retyped, reformatted,
line-ending converted, manually re-split, or synthetically reconstructed. A
ChatGPT session that cannot execute the builder directly MUST use the
connector-only trusted producer path above rather than hand-authoring a patch or
requiring a local Worker process.

## Direct builder request lifecycle

A producer with a trusted real checkout may use this direct path:

1. Read current target and `main`; capture exact full SHAs.
2. Prepare the intended product change in a disposable checkout at the exact
   target SHA and stage exactly those files.
3. Run the mechanical request builder.
4. Create a unique `chatgpt-dispatch-stage-<request-id>` from current `main`.
5. Publish exact builder-emitted parts without transformation.
6. Re-read target and `main`; if either moved, abandon the unpublished request,
   rebuild, and use a new ID.
7. Add exact builder-emitted `ready.json` in its own final commit.
8. Continue through trusted publisher, Dispatcher, Draft PR, and validation.

A stale request is never force-applied. SHA fields are never updated without
regenerating the product patch.

## Dispatcher `ready.json` format

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

For every request:

- request ID matches its directory;
- target is a safe `chatgpt/gameengine-*` ref;
- expected target SHA is full-length;
- commit message is a non-empty single line of at most 120 characters;
- PR title is a non-empty single line of at most 200 characters;
- PR body is at most 8,000 characters;
- `patch_parts` exactly matches the request directory;
- the legacy auto-merge authorization marker is forbidden.

Schema v2 additionally requires the full current-main baseline, exact SHA-256 and
byte length of the reconstructed patch, target ancestry from that baseline, and
revalidation that current `main` still equals the declared baseline.

## Patch parts

Patch transport is byte-preserving concatenation. The Dispatcher concatenates
parts in declared order without adding separators. Constraints:

- contiguous names `part-0000.patch`, `part-0001.patch`, ...;
- 1 to 64 parts;
- each part 1 to 60,000 bytes;
- reconstructed patch at most 4 MiB;
- request directory contains only declared parts plus `ready.json`;
- files are normal non-executable blobs, not symlinks.

The artifact published after preflight MUST be byte-identical to the preflighted
artifact.

## Transport publisher safety checks

Before advancing `chatgpt-dispatch`, the trusted publisher:

1. accepts a successful read-only stage signal or explicit recovery dispatch
   carrying the immutable stage branch and full ready commit;
2. requires stage branch HEAD still to equal that exact ready commit;
3. requires linear staged history based on `main`;
4. allows pre-ready commits to add only request patch parts and the final commit
   to add only `ready.json`;
5. validates names, modes, counts, sizes, request ID, and schema;
6. loads current-main trusted request controls;
7. re-reads target and current `main` and validates schema-v2 baseline ancestry;
8. verifies reconstructed hash and byte count;
9. runs strict exact-target patch applicability, `git diff --cached --check`,
   public path allow-list, and symlink/submodule rejection;
10. rejects request-ID reuse when existing content differs and treats identical
    existing content idempotently;
11. enters the global publisher concurrency group before selecting the latest
    transport head;
12. cherry-picks the validated immutable request onto that head;
13. re-reads remote transport immediately before push and uses an exact lease;
14. explicitly starts the trusted Dispatcher from `main` with the published full
    ready commit SHA.

Publisher concurrency uses `cancel-in-progress: false` and `queue: max`. A
canceled/queued publisher is replayed using the same immutable stage branch and
full ready commit. Producers do not bypass contention by writing the shared ref.

## Dispatcher safety checks

The trusted Dispatcher:

1. resolves an explicit trusted request commit or compatible read-only transport
   signal and requires that full commit to be reachable from `chatgpt-dispatch`;
2. verifies the final commit added only the declared `ready.json`;
3. validates request schema, part names, modes, counts, and sizes;
4. accepts schema 1 only for migration compatibility and schema 2 for normal
   current requests;
5. checks out the declared target and requires current HEAD to equal
   `expected_head_sha`;
6. reconstructs the patch and revalidates v2 hash, byte count, baseline, current
   `main`, and ancestry;
7. runs `git apply --check --whitespace=error-all`;
8. applies to the local index only;
9. rejects `.github/**`, `.chatgpt-requests/**`, and all paths outside the public
   GameEngine allow-list, including old rename paths with rename detection off;
10. rejects modes `120000` and `160000`;
11. runs `git diff --cached --check`;
12. re-reads target HEAD immediately before commit/push; and
13. pushes with an exact lease for `expected_head_sha`.

Dispatcher concurrency also uses `cancel-in-progress: false` and `queue: max`.
No stale request is force-applied.

## Failure diagnosis before retry

Choose recovery by failing layer:

- Connector producer signal failure: verify the branch name and that the final
  commit newly adds only `.chatgpt-producer/<request-id>/ready.json`.
- Trusted producer rejection: verify producer branch immutability, exact target
  and `main` SHAs, linear history, allowed product paths, and producer envelope.
  Do not edit the producer branch after ready; create a new request ID.
- Trusted producer builder failure: correct the product state or stale baseline;
  never hand-edit a generated diff.
- Corrupt/misaligned patch: discard the unpublished artifact and regenerate from
  the exact target; never repair hunk headers by hand.
- Stage signal/publisher rejection: verify immutable stage shape, exact stage
  ready commit, request envelope, part sequence, sizes, and exact builder bytes.
- Hash/byte mismatch: published bytes differ from builder output; abandon the
  unpublished request and rebuild with a new ID.
- Current `main` advanced: rebuild the normal main-based target/request from
  current `main`; never replace only `baseline_main_sha`.
- Target HEAD moved: regenerate from the current target with a new ID; never
  force-apply a stale request.
- Transport moved outside publisher serialization: identify the external writer;
  do not force-update or use a producer-side direct fast-forward retry.
- Publisher canceled before publication: replay publisher from `main` using the
  same immutable stage branch and full ready commit.
- Dispatcher canceled after transport publication: replay Dispatcher using the
  same published full request commit; do not republish the request.
- Dispatcher path rejection: do not weaken the allow-list from a product request.
  Trusted automation changes require a separate infrastructure branch and PR.
- Windows Validation failure: read aggregate summary, selected mode/scope, logs,
  diagnostics, and external-service state before deciding whether code changes
  are justified.
- GitHub, runner, network, or dependency-service failure: do not create
  speculative product changes to compensate.

Confirmed reusable failures belong in
`docs/CHATGPT_AUTOMATION_INCIDENTS.md`. That log supplements but does not
override this protocol.

## Automation regression suite

`.github/workflows/gameengine-chatgpt-automation-regression.yml` runs when ChatGPT
automation, protocol documentation, or the Editor visual-capture harness changes.
It discovers `test_*_protocol.py`, including:

- `.github/chatgpt/test_request_protocol.py` for INC-001 through INC-007 and
  schema-v2 request-builder protections; and
- `.github/chatgpt/test_producer_protocol.py` for connector producer generation,
  trust-boundary rejection, immutable producer-head checks, and read-only signal
  / trusted publisher handoff contracts.

When an automation incident produces a deterministic durable fix, add a
regression when practical.

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
`main` and supplies target branch, PR number, exact pushed HEAD, and request ID.
The workflow resolves the remote target and requires its current HEAD to equal
the supplied SHA before checkout.

Validation modes are `affected`, `full`, and `docs`.

### `affected`

Normal safely-classifiable crate changes use Cargo metadata to select changed
workspace packages. Formatting plus selected-package Clippy, tests, and docs are
run. Reverse dependents are not added to the normal PR critical path. Success
means only the selected affected scope passed; it is not a full-workspace claim.

### `full`

Full mode is mandatory for nightly validation, workspace manifest/lock/toolchain
changes, validation/build/automation infrastructure, unknown or deleted package
paths, and any change that cannot be classified safely. It runs:

```text
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

### `docs`

Recognized documentation-only changes may skip Rust compilation on PR,
merge-group, and main fast paths. Nightly still provides full-workspace coverage.

The machine-readable aggregate summary and workflow conclusion are authoritative
for selected mode and scope.

## ChatGPT repair loop

After dispatch:

1. Read validation `summary.json`, selected mode, and scope.
2. If all gates required by that mode succeed, stop and leave the PR Draft.
3. On failure, inspect relevant job logs and diagnostics.
4. Separate code defects from GitHub/runner/network/dependency failures.
5. Re-read target HEAD, current `main`, and affected files.
6. For a correction, create a new schema-v2 request using either the exact
   direct builder path or a new immutable connector producer branch.
7. Repeat trusted production/publication, Dispatcher, and validation.

Normal automated repair is limited to five rounds. If the same essential
failure repeats twice, reassess the root-cause hypothesis before another retry.

## Incident learning

After a failure is understood and recovery is validated:

1. Search `docs/CHATGPT_AUTOMATION_INCIDENTS.md` by symptom, layer, and cause.
2. Update an existing root-cause entry when it already covers the failure.
3. Add a new incident only for a materially different confirmed root cause.
4. Record symptom, layer, evidence, root cause, successful resolution,
   prevention/next action, and durable request/PR/run/commit references.
5. Separate confirmed facts from inference.
6. Do not record secrets, credentials, private machine paths, or large raw logs.
7. Add an executable regression when the durable prevention is deterministic.
8. Update this canonical protocol when the lesson applies to all future requests.

Trusted automation defects under `.github/**` are repaired only through a
separate `chatgpt/gameengine-*` automation-infrastructure branch and Draft PR,
never through the Dispatcher product path.

## Fallback

No write-capable bypass is installed. If connector producer, trusted producer,
publisher, Dispatcher, or validation is unavailable, diagnose and repair the
trusted path rather than bypassing staging, exact-head checks, single-writer
transport, Draft PR, or validation guarantees.
