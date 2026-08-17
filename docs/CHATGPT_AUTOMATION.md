# ChatGPT GitHub Automation

Status: Accepted
Version: 2.2.0
Canonical location: `docs/CHATGPT_AUTOMATION.md`

## Purpose

This document defines the repository-native path used by ChatGPT to apply and
validate GameEngine changes without Codex and without the OpenAI API.

When ChatGPT has the GitHub connector but no repository shell, the normal
product-change path is:

```text
ChatGPT + GitHub connector
  -> target chatgpt/gameengine-* branch identity
  -> data-only chatgpt-producer-stage-<request-id>
  -> manifest.json + immutable UTF-8/LF source-state blobs
  -> final ready.json commit
  -> repository issue carrying branch + exact ready commit
  -> read-only producer signal loaded from main
  -> trusted producer loaded from main
  -> exact target checkout on GitHub Actions runner
  -> materialize/stage intended product source state
  -> .github/chatgpt/request_protocol.py build
  -> strict exact-target preflight + real remote target/main recheck
  -> schema-v2 request bytes
  -> immutable chatgpt-dispatch-stage-<request-id>
  -> read-only stage signal
  -> trusted transport publisher
  -> chatgpt-dispatch
  -> trusted dispatcher
  -> target branch
  -> Draft pull request
  -> GameEngine Windows Validation
  -> optional Visual Validation when the product change requires it
```

A producer that already owns a real exact checkout and can execute the builder
may continue to use the direct builder entry point. Both producer paths converge
at `chatgpt-dispatch-stage-<request-id>` and use the same publisher, Dispatcher,
request schema, and validation contracts.

The connector path MUST NOT fall back to hand-authored unified diffs, manually
maintained hunk headers, direct product commits to the task branch, or direct
writes to `chatgpt-dispatch`.

## Trust boundary

Write-capable automation MUST be loaded from the trusted default branch.
Producer-controlled branches are data only. They are never trusted executable
control and MUST NOT contain workflow or script changes as part of a product
request.

The normal connector producer signal is a GitHub issue. The issue-triggered
`gameengine-chatgpt-producer-signal.yml` is loaded from `main` and has only
`contents: read` and `issues: read`. A successful signal is consumed through
`workflow_run` by `gameengine-chatgpt-trusted-producer.yml`, which is also loaded
from `main` and owns the write permissions required to publish an immutable
Dispatcher staging branch.

The trusted producer never executes code from the producer branch. It reads and
validates only the producer manifest and source-state blobs, checks that the
producer branch still points at the signaled full ready commit, materializes the
declared final product files in an exact target worktree, stages exactly those
paths, and invokes the trusted `request_protocol.py build` from `main`.

The trusted producer publishes the builder output without rewriting it. It does
not generate or repair unified-diff hunk headers itself. It does not update
`chatgpt-dispatch`. After publication it explicitly invokes the read-only stage
signal because pushes made with the trusted producer's `GITHUB_TOKEN` do not
start ordinary push-triggered workflows.

The trusted transport publisher remains the normal single writer to
`chatgpt-dispatch`. The Dispatcher remains the only automation component that
applies a validated product patch to a `chatgpt/gameengine-*` task branch and
creates or updates the Draft PR.

## Branches

### `chatgpt-producer-stage-<request-id>`

Connector producer input branch. It MUST be created from the declared current
`baseline_main_sha`. Product files are not edited on this branch. Commits before
the final ready commit may only add immutable files below:

```text
.chatgpt-producer/<request-id>/
```

The final commit MUST newly add only:

```text
.chatgpt-producer/<request-id>/ready.json
```

After the ready commit is signaled, the branch is immutable. Any movement causes
trusted production to reject the request. Corrections use a new request ID and a
new branch.

### `chatgpt-dispatch-stage-<request-id>`

Immutable per-request transport staging branch. It is created from the declared
current `main` baseline. Pre-ready commits add only:

```text
.chatgpt-requests/<request-id>/part-NNNN.patch
```

The final commit newly adds only:

```text
.chatgpt-requests/<request-id>/ready.json
```

The trusted producer copies the exact mechanical builder output into this branch.
Direct builder producers may do the same after running the builder themselves.
After ready, the branch is immutable.

### `chatgpt-dispatch`

Long-lived shared transport branch containing immutable request records under
`.chatgpt-requests/<request-id>/`. The trusted transport publisher is the normal
single writer. Producers MUST NOT commit to or update this ref directly.

### `chatgpt/gameengine-*`

All product target branches MUST use this namespace. A request records the exact
full target HEAD in `expected_head_sha`. The Dispatcher refuses a stale target.

Automation/trust-boundary changes under `.github/**` do not use the product
Dispatcher path. They use a dedicated `chatgpt/gameengine-*` infrastructure
branch and Draft PR.

## Connector producer input format

Producer schema version 1 carries changed **source state**, not patch bytes.
The request directory contains:

```text
.chatgpt-producer/<request-id>/manifest.json
.chatgpt-producer/<request-id>/files/NNNN.source   # add/update entries only
.chatgpt-producer/<request-id>/ready.json          # final commit only
```

`files/NNNN.source` is derived from the zero-based manifest entry index. The
producer does not supply arbitrary transport file names.

### `manifest.json`

The manifest has exactly these top-level fields:

```json
{
  "schema_version": 1,
  "request_id": "renderer-aa-20260817-01",
  "target_branch": "chatgpt/gameengine-renderer-aa",
  "expected_head_sha": "0123456789abcdef0123456789abcdef01234567",
  "baseline_main_sha": "89abcdef0123456789abcdef0123456789abcdef",
  "source_format": "utf8-lf",
  "commit_message": "Add shared renderer anti-aliasing",
  "pr_title": "GameEngine: add shared renderer anti-aliasing",
  "pr_body": "## Summary\n\nAdds the shared renderer anti-aliasing path.",
  "files": [
    {
      "path": "crates/example/src/lib.rs",
      "operation": "update",
      "base_mode": "100644",
      "mode": "100644",
      "source_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
      "source_bytes": 1234
    }
  ]
}
```

The manifest file entries MUST be sorted by `path`. Duplicate paths,
Unicode/case-fold collisions, and file/directory prefix collisions are rejected.
The trusted producer also rejects absolute paths, path traversal, backslashes,
control characters, Windows-unsafe names, and paths outside the existing public
GameEngine product allow-list. `.github/**` and `.chatgpt-requests/**` are
therefore never valid product entries.

Supported operations are:

- `add`: `base_mode` is `null`, `mode` is `100644` or `100755`, and source hash
  and byte length describe the final file bytes;
- `update`: `base_mode` binds the exact target file mode, `mode` declares the
  final normal-file mode, and source hash and byte length describe final bytes;
- `delete`: `base_mode` binds the exact target file mode, `mode` is `null`,
  `source_sha256` is `null`, and `source_bytes` is `0`.

The target commit SHA already binds the previous file contents. The producer does
not need to restate an old-content hash.

### Source-state byte contract

Producer schema v1 accepts source blobs only as strict UTF-8 text using LF line
endings. NUL bytes and CR bytes are rejected. This makes connector-created text
state deterministic across GitHub API and Windows/Linux runners.

Binary add/update is intentionally not representable in producer schema v1.
Binary file deletion is permitted when the exact target contains a normal file
with the declared base mode. A future binary source schema requires a separate
protocol revision rather than an implicit encoding exception.

Limits are:

- at most 256 changed file entries;
- at most 1 MiB per source-state blob;
- at most 8 MiB total source-state bytes;
- at most 256 KiB for `manifest.json`;
- normal modes only (`100644` or `100755`).

The downstream protocol-v2 patch remains limited to 4 MiB and 64 transport
parts, so producer input acceptance does not weaken Dispatcher transport limits.

### `ready.json`

The final producer commit adds exactly:

```json
{
  "schema_version": 1,
  "request_id": "renderer-aa-20260817-01",
  "manifest_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "manifest_bytes": 2048
}
```

This binds the exact manifest bytes. Every add/update source blob is separately
bound by the manifest's `source_sha256` and `source_bytes`. The trusted producer
requires the final request directory to contain exactly the manifest, exactly
the source blobs implied by non-delete entries, and `ready.json`. Missing and
extra files are rejected.

All producer data files MUST be normal non-executable blobs. Symlinks and
submodules cannot be introduced through producer input.

## Connector signal format

After the immutable producer ready commit is pushed, ChatGPT creates an issue
with title:

```text
GameEngine ChatGPT Producer <request-id> <full-producer-ready-commit-sha>
```

and an exact JSON body:

```json
{
  "signal": "gameengine-chatgpt-producer-v1",
  "request_id": "renderer-aa-20260817-01",
  "producer_branch": "chatgpt-producer-stage-renderer-aa-20260817-01",
  "producer_commit": "0123456789abcdef0123456789abcdef01234567"
}
```

Only repository owners, members, or collaborators may create an accepted signal.
The read-only signal validates the branch name, full SHA, title/body agreement,
and current remote producer head. Its immutable workflow-run title binds the
issue number, request ID, and producer commit.

The write-capable trusted producer re-reads the issue, requires its title and
body still to match the successful signal, and again checks the remote producer
head before reading any producer data. Issue or branch mutation after signal is
therefore rejected rather than silently changing the authorized input.

`workflow_dispatch` exists only as a trusted recovery entry point and still
accepts only an immutable producer branch plus full ready commit. It does not
execute producer-supplied scripts or workflows.

## Trusted producer responsibilities

For each accepted connector producer request, the trusted producer:

1. runs from the current trusted `main` workflow definition;
2. resolves the successful read-only signal or explicit recovery input;
3. requires producer branch `chatgpt-producer-stage-<request-id>` and a full
   40-character producer ready commit;
4. rechecks the producer remote branch still equals that commit;
5. reads exact `ready.json` and verifies the manifest byte count and SHA-256;
6. strictly validates manifest schema, paths, operations, modes, limits,
   duplicate/case collisions, and deterministic ordering;
7. verifies the request directory has no missing or extra files and validates
   every source byte count, SHA-256, UTF-8 encoding, and LF-only line endings;
8. requires producer commit history after `baseline_main_sha` to be linear,
   addition-only input data, with the final commit changing only ready.json;
9. requires current remote `main` still equal `baseline_main_sha`;
10. requires current remote target still equal `expected_head_sha` and requires
    the main baseline to be an ancestor of that target;
11. creates a detached exact-target worktree and verifies add/update/delete
    semantics and target base modes before touching each declared path;
12. materializes the declared final source bytes and stages exactly the manifest
    path set, with no unstaged or untracked product state;
13. calls `.github/chatgpt/request_protocol.py build` with remote recheck enabled;
14. lets the builder generate the unified diff, strict preflight it, calculate
    patch hash/bytes, split parts, and emit schema-v2 ready.json;
15. rechecks producer branch, target, and `main` after build and immediately
    before stage publication;
16. copies builder bytes without transformation into a fresh
    `chatgpt-dispatch-stage-<request-id>`;
17. creates patch-part commit(s) followed by a separate ready-only final commit;
18. runs trusted `preflight-stage` over that exact local stage ready commit;
19. publishes the new stage branch with an exact non-existence lease and verifies
    its remote head; and
20. explicitly starts the read-only stage signal from `main` with the exact stage
    branch and full ready commit.

The trusted producer MUST NOT commit to the product target branch and MUST NOT
write `chatgpt-dispatch`.

## Mechanical request builder

New normal Dispatcher requests MUST be produced with
`.github/chatgpt/request_protocol.py build` from an exact checkout of the target
HEAD. Hand-authored unified diffs are not a normal producer path.

The builder:

1. requires checkout `HEAD` to equal full `expected_head_sha`;
2. requires full `baseline_main_sha` to be an ancestor of that target;
3. requires staged intended changes and rejects unstaged tracked or untracked
   files;
4. generates the patch mechanically with Git from the staged index;
5. runs `git apply --check --whitespace=error-all` in a detached exact-target
   worktree;
6. applies only to a temporary index and runs `git diff --cached --check`;
7. enforces allowed product paths and rejects symlink/submodule modes;
8. re-reads the real remote target and `main` immediately before output;
9. splits only at newline boundaries;
10. computes exact `patch_sha256` and `patch_bytes`; and
11. emits schema-v2 `ready.json` plus contiguous `part-NNNN.patch` files.

The hidden remote-recheck bypass is regression-test-only. Production trusted
producer invocations never use it.

Builder output bytes are immutable. They MUST NOT be retyped, reformatted,
line-ending converted, manually re-split, or reconstructed by ChatGPT.

## Dispatcher `ready.json` schema v2

New normal requests use:

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

Schema 1 remains accepted only for already-staged migration-compatible requests.
New producer work MUST generate schema 2.

Schema-v2 guarantees include full target and main SHAs, target ancestry from the
main baseline, exact reconstructed patch SHA-256 and byte count, and refusal when
current remote target or current remote `main` no longer matches the declared
state.

## Patch parts

Patch transport is byte-preserving concatenation:

- names are contiguous `part-0000.patch`, `part-0001.patch`, ...;
- 1 to 64 parts;
- each part is 1 to 60,000 bytes;
- reconstructed patch is at most 4 MiB;
- the request directory contains only declared parts plus `ready.json`;
- all request transport blobs are normal non-executable files.

The bytes published to stage MUST be identical to builder output.

## Read-only stage signal

`gameengine-chatgpt-stage-signal.yml` remains read-only. It accepts either:

- the normal push event for externally-published immutable stage branches; or
- trusted `workflow_dispatch` carrying `stage_branch` and full `request_commit`,
  used when the trusted producer's `GITHUB_TOKEN` push cannot trigger another
  push workflow.

Both forms validate the same remote stage head, branch/request-ID relationship,
full ready SHA, ready-only final commit, normal mode, and ready JSON identity.
The workflow-run display title binds the exact stage branch and ready commit for
the downstream publisher.

## Transport publisher safety checks

Before advancing `chatgpt-dispatch`, the trusted publisher:

1. accepts a successful read-only stage signal or explicit recovery dispatch;
2. derives or receives an immutable stage branch and full ready commit;
3. requires the remote stage branch still equal that exact commit;
4. requires linear staged history based on `main`;
5. allows pre-ready commits to add only request patch parts and the final commit
   to add only `ready.json`;
6. validates names, modes, counts, sizes, request ID, and schema;
7. loads request controls from current trusted `main`;
8. runs `request_protocol.py preflight-stage`, including real remote target/main
   rechecks, schema-v2 hash/byte validation, baseline ancestry, strict
   exact-target applicability, `git diff --cached --check`, allowed paths, and
   symlink/submodule rejection;
9. treats an identical previously-published request ID idempotently but rejects
   different content under the same ID;
10. enters global publisher concurrency before selecting the latest transport
    head;
11. re-reads remote `chatgpt-dispatch` immediately before push; and
12. pushes with exact `--force-with-lease` against the observed transport head.

Publisher concurrency is:

```yaml
cancel-in-progress: false
queue: max
```

The publisher then explicitly starts the trusted Dispatcher from `main` with the
published full ready commit SHA.

## Dispatcher safety checks

The trusted Dispatcher:

1. resolves a full published request commit reachable from `chatgpt-dispatch`;
2. verifies the final commit added only the declared `ready.json`;
3. validates request schema and contiguous part names, modes, counts, and sizes;
4. checks out the declared target and requires exact `expected_head_sha`;
5. reconstructs schema-v2 bytes and revalidates hash, byte count, baseline,
   current `main`, and ancestry;
6. runs `git apply --check --whitespace=error-all`;
7. applies to the local index only;
8. rejects `.github/**`, `.chatgpt-requests/**`, and all paths outside the public
   GameEngine allow-list;
9. rejects symlink and submodule modes;
10. runs `git diff --cached --check`;
11. re-reads target HEAD immediately before product commit/push; and
12. pushes with exact lease for `expected_head_sha`.

Dispatcher concurrency also uses `cancel-in-progress: false` and `queue: max`.
No stale request is force-applied.

## Pull request rules

The Dispatcher creates or reuses at most one open PR for the target branch:

- base is `main`;
- PR remains Draft;
- no auto-merge authorization is accepted;
- Dispatcher never merges;
- the PR body carries `<!-- gameengine-chatgpt-dispatcher -->`.

## Windows Validation

The Dispatcher starts `gameengine-windows-validation.yml` from `main` with the
product branch, PR number, exact pushed HEAD, and request ID. The validation
workflow re-resolves and checks that exact branch head before execution.

Modes are `affected`, `full`, and `docs`.

`full` is required for workspace-wide or automation/validation infrastructure
changes and runs:

```text
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

`affected` validates only planner-selected affected package scope. `docs` may
skip Rust compilation for recognized documentation-only changes. The
machine-readable validation summary is authoritative for mode and scope.

Visual Validation is additional evidence for Editor/Launcher changes whose
layout or visible rendering is part of correctness. It is not required for
logic-only, documentation-only, or automation-only changes.

## Failure diagnosis before retry

Choose recovery by failing layer:

- Producer signal failure: correct the issue signal or create a new immutable
  producer request; do not add a write-capable workflow to a producer branch.
- Producer manifest/source rejection: correct malformed, missing, extra,
  duplicate, hashed, encoded, mode, or path data with a new request ID.
- Producer branch mutation after signal: reject it and create a new immutable
  producer branch; never re-point the old request ID.
- Trusted producer stale target/main: rebuild source-state input against current
  target/current `main`; never update only SHA fields.
- Builder whitespace/applicability failure: correct final source state; never
  repair hunk headers manually.
- Stage signal/publisher rejection: inspect immutable stage shape and exact
  builder bytes; do not bypass the read-only signal or publisher.
- Publisher canceled before transport publication: replay using the same
  immutable stage branch and full ready commit.
- Dispatcher canceled after transport publication: replay the Dispatcher using
  the published full request commit; do not republish the request.
- Transport moved outside serialized publisher: identify the external writer;
  producers do not direct-fast-forward the shared transport ref.
- GitHub, runner, network, or dependency-service failure: do not create product
  code changes to hide an infrastructure failure.

Confirmed reusable failures belong in
`docs/CHATGPT_AUTOMATION_INCIDENTS.md`. Do not create an incident from an
unconfirmed hypothesis.

## Automation regression suite

`.github/workflows/gameengine-chatgpt-automation-regression.yml` runs whenever
ChatGPT automation or its canonical documentation changes. It executes:

- `.github/chatgpt/test_request_protocol.py`, covering confirmed Dispatcher and
  publisher incidents plus schema-v2 builder protections; and
- `.github/chatgpt/test_producer_protocol.py`, covering normal source-state
  request generation, add/update/delete, stale target/main, post-signal branch
  mutation, path traversal, trust-boundary paths, duplicate/case collisions,
  malformed manifests, hash mismatches, missing/extra source data, whitespace,
  request-ID binding, ready-only commit expectations, UTF-8/LF source policy,
  real builder remote-recheck races, read-only signal trust, stage-signal
  identity, and publisher single-writer/exact-lease contracts.

When an automation incident produces a deterministic durable fix, add a focused
regression when practical.

## Repair loop

After a product request reaches validation:

1. Read the validation summary, selected mode, and scope.
2. If required gates succeed, leave the PR Draft for human merge decision.
3. On failure, inspect the relevant automation layer, job diagnostics, and
   external-service state.
4. Separate product defects from transport, runner, network, or dependency
   failures.
5. Re-read current target HEAD and current `main` before generating any fix.
6. Use a new request ID and immutable producer/stage branch for corrected product
   state.
7. Repeat trusted production, publication, Dispatcher, and validation.

Normal automated repair is limited to five rounds. If the same essential
failure repeats twice, reassess the root-cause hypothesis before another retry.

## Incident learning

After a reusable automation failure is understood and its resolution is actually
validated:

1. search `docs/CHATGPT_AUTOMATION_INCIDENTS.md` for the same layer/root cause;
2. update an existing incident when it already covers the failure;
3. create a new incident only for a materially different confirmed root cause;
4. record evidence, root cause, verified resolution, prevention, next action,
   and durable request/PR/run/commit references;
5. separate confirmed fact from inference; and
6. update this protocol when the lesson applies to all future requests.

Do not record secrets, credentials, private machine paths, or large raw logs.

## Fallback

No write-capable bypass is installed. If producer signaling, trusted production,
stage signaling, publisher, Dispatcher, or validation is unavailable, diagnose
and repair the trusted path in a separate infrastructure Draft PR. Do not bypass
exact-head checks, builder generation, immutable staging, single-writer
transport, Draft PR, or validation guarantees.
