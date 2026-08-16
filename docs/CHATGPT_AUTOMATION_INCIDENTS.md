# ChatGPT Automation Incident Log

Status: Living operational record
Canonical location: `docs/CHATGPT_AUTOMATION_INCIDENTS.md`

## Purpose

This document records confirmed failures in the repository-native ChatGPT
Dispatcher and validation workflow together with the recovery knowledge needed
to avoid rediscovering the same cause.

The protocol itself is defined by `docs/CHATGPT_AUTOMATION.md`. This file does
not redefine request schemas, branch rules, trust boundaries, or validation
contracts. If an incident reveals that the protocol is incomplete or wrong,
update the canonical protocol instead of silently creating a conflicting rule
here.

## Recording rules

Record an incident only after there is enough evidence to identify the root
cause and the recovery has been confirmed. A failed run by itself is not an
incident entry.

- Organize entries by root cause, not by date or workflow run.
- Search existing entries before adding a new one.
- Update an existing entry for repeated occurrences of the same root cause.
- Separate observed evidence from inference.
- Record the successful recovery, not merely an attempted workaround.
- Add a prevention or ChatGPT next-action rule that can be followed on the next
  occurrence.
- Do not copy secrets, credentials, private machine paths, or large raw logs.
- Prefer durable references such as request IDs, PRs, workflow runs, or commits
  when evidence needs to be retained.
- Do not record ordinary product-code defects here unless the failure is about
  how ChatGPT transported, applied, validated, or interpreted the change.

If the same essential failure occurs twice while ChatGPT is attempting an
automatic repair, stop speculative correction and reassess the root cause as
required by `docs/CHATGPT_AUTOMATION.md`.

## Incident index

```text
INC-001  Markdown trailing whitespace rejected by patch preflight  Dispatcher         Resolved
INC-002  Unified-diff old coordinates drifted from target tree     Dispatcher         Resolved
INC-003  Published patch bytes differed from preflight artifact    Transport          Resolved
INC-004  Main advanced after task baseline and polluted validation  Validation         Resolved
INC-005  Patch headers included checkout-directory prefix           Dispatcher         Resolved
INC-006  eframe screenshot helper incompatible with default wgpu    Visual validation  Resolved
INC-007  Missing-request probe misread unresolved revision text     Transport          Resolved
```

## INC-001: Markdown trailing whitespace rejected by patch preflight

Status: Resolved
Layer: Dispatcher
First confirmed: 2026-08-16
Last confirmed: 2026-08-16

### Symptom

Request `automation-docs-20260816-01` passed the ready-marker signal and request
envelope validation, then failed in `Reconstruct and validate patch` with
`git apply --check rejected the patch.`

### Evidence

- Dispatcher run `31920548068` failed at the patch validation step.
- The dispatcher annotation reported `git apply --check rejected the patch.`
- Re-running the same reconstructed patch with
  `git apply --check --whitespace=error-all` reported one trailing-whitespace
  error on the new incident document's `Status:` line.
- Plain `git apply --check` had accepted the same patch, so that weaker
  preflight did not reproduce the dispatcher contract.

### Root cause

The patch producer used two trailing spaces to create a Markdown hard line
break. The dispatcher intentionally rejects all added whitespace errors with
`--whitespace=error-all`, while the producer's earlier local syntax check used
plain `git apply --check`. The producer therefore validated a weaker contract
than the dispatcher enforces.

### Resolution

Remove the trailing spaces and generate a new immutable request. Validate the
reconstructed correction patch with the dispatcher's exact
`git apply --check --whitespace=error-all` option before publishing `ready.json`.

### Prevention / ChatGPT next action

For every dispatcher request, use the exact whitespace-strict `git apply`
preflight before publishing the request. Do not use Markdown trailing spaces in
new or changed lines; use a blank line when a visual line break is needed.

### References

- Request: `automation-docs-20260816-01`
- PR: N/A; patch application failed before PR creation
- Workflow run: `31920548068`
- Commit: N/A; target branch was not changed

## INC-002: Unified-diff old coordinates drifted from target tree

Status: Resolved
Layer: Dispatcher
First confirmed: 2026-08-16
Last confirmed: 2026-08-16

### Symptom

Repeated correction requests for PR #23 passed request-envelope validation but
failed in `Reconstruct and validate patch` because `git apply --check` rejected
`crates/render-runtime/src/render_backend.rs`. The target branch did not move
between those attempts.

### Evidence

- Request `20260816-phase3-full-ibl-bindgroups-r5` failed in Dispatcher run
  `31921512779` at `render_backend.rs:2626`.
- The target branch remained at
  `acc2b01b0157c9b381c8b89a8f5e28d7c7f42df1`, ruling out a stale target HEAD.
- Re-reading the exact target blob showed the Light BGL block began at old-file
  line 2624, while the hand-built hunk declared line 2626. The producer had
  incorrectly carried earlier added-line offsets into the old-file coordinates.
- A following repair exposed the same class of error in the adjacent Light BG
  hunk (`2666` versus the actual old-file line `2667`).
- Request `20260816-phase3-full-ibl-bindgroups-r7` combined the dependent edit
  into target-aligned context and Dispatcher run `31922381541` passed
  `Reconstruct and validate patch`, committed, and pushed successfully.
- The same target-alignment class recurred in request
  `adr-audit-index-20260816-02`: Dispatcher run `31922197791` rejected ADR/doc
  hunks that had been generated from stitched snippets rather than the exact
  target files. The final exact-target regeneration was validated by the
  successful ADR-audit request recorded in INC-003.
- The same target-alignment class recurred in Phase 8 request
  `20260816-phase8-temporal-history-ce777a7-r1`: Dispatcher run `31931155985`
  rejected the final `docs/adr/README.md` hunk while the target branch remained
  at `ce777a7ecf572001e727e10f470c7a1bebae6714`. The local preflight fixture had
  an extra blank line after the target file's real EOF, so it validated context
  that did not exist in the exact target blob.
- Replacement request `20260816-phase8-temporal-history-ce777a7-r2` regenerated
  that hunk against the exact target EOF. Dispatcher run `31931372697` passed
  reconstruction/application and produced commit
  `b3421b3c454ca7ebde31a2a9585e441d91ea7802` in Draft PR #49.

### Root cause

The producer manually maintained unified-diff hunk coordinates across several
edits in the same file. It adjusted later old-file coordinates as though prior
added lines had already changed the source tree. Unified diff old coordinates
must always refer to the unmodified target file. Adjacent dependent hunks also
made the hand-maintained offsets fragile. The Phase 8 recurrence also showed
that a synthetic or stitched preflight fixture can create the same mismatch
when its EOF or surrounding context differs from the exact target blob.

### Resolution

Re-read the exact target blob, rebuild the patch against that tree, and keep
old-file hunk coordinates anchored to the unmodified source. For adjacent edits
that depend on the same local context, combine them into one target-aligned hunk
or generate the diff from old/new files instead of carrying offsets by hand.
The corrected r7 request passed the dispatcher's strict reconstruction/apply
preflight and produced commit
`1e82c3beb53205404a5110828c5e8943f62d6aa7`.

### Prevention / ChatGPT next action

When `git apply --check` rejects a non-stale request, re-read the exact target
file and regenerate the unified diff from that source. Never update old-file
hunk coordinates by accumulating prior additions. Prefer generated diffs; when
manual construction is unavoidable, merge adjacent dependent edits and verify
the complete reconstructed request with
`git apply --check --whitespace=error-all` before publishing `ready.json`.
Do not substitute a synthetic partial file for the exact target during
preflight; EOF presence and blank-line context are part of patch applicability.

### References

- Request: `20260816-phase3-full-ibl-bindgroups-r7`
- PR: `#23`
- Workflow run: `31922381541`
- Commit: `1e82c3beb53205404a5110828c5e8943f62d6aa7`
- Repeated failed request: `20260816-phase8-temporal-history-ce777a7-r1`
- Recovery request: `20260816-phase8-temporal-history-ce777a7-r2`
- Repeated failed Dispatcher run: `31931155985`
- Recovery Dispatcher run: `31931372697`
- Recovery PR: `#49`
- Recovery commit: `b3421b3c454ca7ebde31a2a9585e441d91ea7802`

## INC-003: Published patch bytes differed from preflight artifact

Status: Resolved
Layer: Transport
First confirmed: 2026-08-16
Last confirmed: 2026-08-16

### Symptom

ADR-audit request `adr-audit-index-20260816-03` passed the request envelope and
target-HEAD checks but failed `Reconstruct and validate patch` even though the
corrected local patch had already passed the dispatcher's strict `git apply`
preflight.

### Evidence

- Dispatcher run `31922629969` failed only the ADR 0113 hunk after the earlier
  exact-target applicability problems had been corrected.
- The intended preflighted `part-0001.patch` was 2,445 bytes with Git blob SHA
  `b2cb31fff1dcec4cbae188f3dd74381aab82ab15`.
- The published request's `part-0001.patch` was 2,444 bytes with Git blob SHA
  `a68d723a0d3e51a7de2515966f53a851f9591548`; the other four published part
  blobs matched their preflight artifacts.
- Comparing the two artifacts found one missing leading `-` on a deleted
  Markdown bullet. The target branch itself had not moved.
- Request `adr-audit-index-20260816-04` published all five preflighted blobs with
  matching byte counts and blob SHAs. Dispatcher run `31922738621` then passed,
  produced commit `c689fff815f48ff2f56836fcfb096d443dee67ca` and Draft PR #30, and Windows
  Validation run `31922746993` completed successfully in `docs` mode.
- The same byte-divergence class recurred before `ready.json` in request
  `20260816-phase5-stable-csm-e99d227a-r1`: a fixed-byte split ended inside a
  whitespace-bearing line fragment, and the published part blob SHA differed
  from the preflight artifact. The request was abandoned; newline-boundary
  splitting restored exact blob equality before the next ready marker.

### Root cause

The producer manually transcribed one patch part after local preflight. That
transcription changed one byte, so the artifact validated locally was not the
artifact later reconstructed by the Dispatcher. A correct preflight cannot
protect a different set of transport bytes.

### Resolution

Treat the preflighted patch as an immutable artifact. Split and publish those
exact bytes, then verify the published part byte counts and Git blob SHAs before
creating `ready.json`. The `-04` request used that process and completed patch
application, Draft PR publication, and docs validation successfully.

### Prevention / ChatGPT next action

Never retype or manually reconstruct a patch after strict preflight. Publish the
preflight artifact mechanically, verify the remote part blobs are byte-identical
before `ready.json`, and only then re-read the target HEAD and release the ready
marker. If any part hash or byte count differs, abandon that unpublished request
and create a new request from the verified artifact. When the publication API
accepts text rather than raw bytes, split parts on newline boundaries so a
whitespace-bearing mid-line fragment never becomes terminal transport text.

### References

- Failed request: `adr-audit-index-20260816-03`
- Successful request: `adr-audit-index-20260816-04`
- PR: `#30`
- Failed Dispatcher run: `31922629969`
- Successful Dispatcher run: `31922738621`
- Validation run: `31922746993`
- Commit: `c689fff815f48ff2f56836fcfb096d443dee67ca`
- Repeated unpublished request: `20260816-phase5-stable-csm-e99d227a-r1`

## INC-004: Main advanced after task baseline and polluted validation scope

Status: Resolved
Layer: Validation
First confirmed: 2026-08-16
Last confirmed: 2026-08-16

### Symptom

Request `adr0127-native-2d-20260816-1156-01` applied successfully and created
Draft PR #33, but Windows Validation run `31923487466` planned validation for
unrelated renderer and automation files even though the request itself added
only ADR 0127.

### Evidence

- The original task branch was created from main commit
  `295b86e41cf7db2dc673739de348783358a6a7ee`, and the Dispatcher produced task
  commit `52214ccf845650da4d4f4adfdc28636e142a4955`.
- Before validation planning, `main` advanced to
  `e9c1a060b63b8b2ff93e1f45115047f69d0a1a15` through unrelated renderer work.
- Validation run `31923487466` checked out the exact requested head, then used
  the newer main commit as `BASE_SHA`. Its changed-path step listed the unrelated
  renderer/automation changes between those divergent commits as well as ADR
  0127, so the task no longer received documentation-only scope.
- Recovery request `adr0127-native-2d-20260816-1208-02` started a fresh task
  branch from the then-current main commit, regenerated the request, and produced
  commit `6f190a90cacb5aee2d48c6a13dbd39ba9e698946` in Draft PR #34.
- Validation run `31923640542` then reported `docs` mode, base
  `e9c1a060b63b8b2ff93e1f45115047f69d0a1a15`, exact head
  `6f190a90cacb5aee2d48c6a13dbd39ba9e698946`, and overall `success`. The
  superseded PR #33 was closed without merge.

### Root cause

The Dispatcher correctly protects the target branch with `expected_head_sha`,
but that guard only detects movement of the target branch. PR validation plans
changed paths against the PR's current main baseline. When main advances after a
task branch was created, the old task head and current main are divergent. A
direct base-to-head changed-path comparison therefore includes intervening main
changes that are absent from the old task branch, polluting validation scope.

### Resolution

Create a new task branch from current `main`, regenerate the patch/request from
that exact baseline, and validate the new Draft PR. The replacement request and
PR #34 completed documentation-only validation successfully. Do not treat the
old broader validation as evidence for the task's intended scope.

### Prevention / ChatGPT next action

For normal main-based work, immediately before publishing `ready.json`, re-read
both the target branch and `main`. If main advanced beyond the baseline the task
branch contains, create a new task branch and regenerate the request from current
main unless the older baseline is intentional. After validation, always read the
machine-readable mode/scope before interpreting gate results.

### References

- Failed-scope request: `adr0127-native-2d-20260816-1156-01`
- Successful request: `adr0127-native-2d-20260816-1208-02`
- Superseded PR: `#33`
- Successful PR: `#34`
- Polluted validation run: `31923487466`
- Successful validation run: `31923640542`
- Successful commit: `6f190a90cacb5aee2d48c6a13dbd39ba9e698946`

## INC-005: Patch headers included checkout-directory prefix

Status: Resolved
Layer: Dispatcher
First confirmed: 2026-08-16
Last confirmed: 2026-08-16

### Symptom

Request `20260816-phase5-stable-csm-e99d227a-r2` passed the ready-marker
signal and request-envelope checks, then failed in `Reconstruct and validate
patch` before any product commit was created.

### Evidence

- Dispatcher run `31926298039` rejected all four changed files with
  `No such file or directory`, including
  `GameEngine/crates/render-runtime/src/shadow.rs`.
- The target branch remained at
  `e99d227ab40a7d839afa718e40ab372ec1899f32`, ruling out stale target state.
- The failed patch headers used paths such as
  `a/GameEngine/crates/render-runtime/src/shadow.rs`, carrying an old
  checkout-directory name into the repository-relative Git patch path.
- Replacement request `20260816-phase5-stable-csm-e99d227a-r3` kept the
  product edits unchanged but regenerated headers as `a/crates/...` and
  `a/docs/...`. Dispatcher run `31926476659` then passed reconstruction and
  application, produced commit `7ce7997042802368886d8fe2db26ce7be98ed1af`
  and Draft PR #39, and Windows Validation run `31926484133` completed
  successfully in affected mode for `engine-render-runtime`.

### Root cause

The patch producer treated the local checkout directory name `GameEngine/` as
part of the repository path. Dispatcher `git apply` runs at the repository
root, so Git patch headers must be repository-root relative. The otherwise
current patch therefore referenced paths that do not exist in the target tree.

### Resolution

Regenerate the same immutable product patch from the repository root, removing
the checkout-directory prefix from every old/new path. Re-verify the published
part blob hashes, use a new request ID, and release a new ready marker. The r3
request passed Dispatcher application and Windows Validation without changing
the Phase 5 product design.

### Prevention / ChatGPT next action

Generate and preflight patches from the repository root. Before `ready.json`,
inspect every changed path and reject checkout-local prefixes such as
`GameEngine/`. If `git apply` reports missing files while the target HEAD is
current, validate patch path roots before editing code or adjusting hunk
context.

### References

- Failed request: `20260816-phase5-stable-csm-e99d227a-r2`
- Successful request: `20260816-phase5-stable-csm-e99d227a-r3`
- PR: `#39`
- Failed Dispatcher run: `31926298039`
- Successful Dispatcher run: `31926476659`
- Validation run: `31926484133`
- Commit: `7ce7997042802368886d8fe2db26ce7be98ed1af`

## INC-006: eframe screenshot helper incompatible with default wgpu

Status: Resolved
Layer: Visual validation
First confirmed: 2026-08-16
Last confirmed: 2026-08-16

### Symptom

ADR 0112 Draft PR #28 requested Editor visual validation, but the capture job
built the Editor and then panicked before any screenshot was produced.

### Evidence

- Visual Validation run `31930038159` reached the Editor capture invocation and
  panicked in eframe 0.34.3's native wgpu integration with
  `EFRAME_SCREENSHOT_TO not yet implemented for wgpu backend`.
- No PNG was generated, so the failing workflow could not establish visual
  correctness for PR #28.
- A first infrastructure-only Glow attempt in Draft PR #45 was not a valid
  resolution: Visual Validation run `31930753037` was rejected by Cargo because
  enabling Glow changed dependency resolution while the capture command uses
  `--locked`.
- Commit `ac8e755e1031b180f924d8a61a0e054f38350972` kept the default wgpu
  renderer and moved Editor capture to egui's normal screenshot command/event
  path. Visual Validation run `31930981030` then completed successfully and
  uploaded artifact `9259315213` containing a non-empty `editor.png`.
- Final commit `ff2254a12648db7d91d0a27526d529f5fe94fcd7` also bounded a missing
  screenshot response so the validation-only process cannot wait indefinitely.
  Visual Validation run `31931147173` completed successfully and artifact
  `9259358605` contained `editor.png` with SHA-256
  `574200f7ef80e07331f454b38a6f032d4668790001533b7dd06ca56950e5b030`.
- ChatGPT retrieved and visually inspected the successful screenshots. The
  Editor startup layout showed no clipping, overlap, broken spacing, missing
  panels, or obvious color/background regression.

### Root cause

The visual harness treated eframe's `__screenshot` /
`EFRAME_SCREENSHOT_TO` helper as renderer-independent. eframe 0.34 uses wgpu as
the default native renderer, while the helper path used by the harness is not
implemented by eframe 0.34.3's native wgpu integration. The validation contract
therefore depended on an example/helper capture mechanism that did not support
the renderer used by the actual Editor.

### Resolution

Keep the normal Editor renderer and Cargo dependency graph unchanged. The
Editor `visual-validation` feature is now application-owned: the script passes
`GAMEENGINE_SCREENSHOT_TO`, the Editor requests a frame with egui
`ViewportCommand::Screenshot`, receives `Event::Screenshot`, writes the image
with its existing PNG encoder, and closes. A bounded capture timeout turns a
missing response into an explicit failed artifact instead of a hanging job.

This resolution was validated by successful Visual Validation runs
`31930981030` and `31931147173` and by direct review of their PNG artifacts.

### Prevention / ChatGPT next action

Do not use `EFRAME_SCREENSHOT_TO` for Editor capture while the native Editor uses
wgpu. Prefer egui's renderer-independent screenshot command/event path and keep
capture-only behavior dependency-neutral so `cargo --locked` remains valid.
Always inspect the produced PNG before reporting Visual Validation PASS. The
Launcher still uses a separate capture path and must not be reported fixed or
visually validated by this Editor-only incident.

### References

- Request: N/A; trusted visual-validation infrastructure change
- Blocked product PR: `#28`
- Infrastructure PR: `#45`
- Failed Visual Validation run: `31930038159`
- Unsuccessful Glow-attempt run: `31930753037`
- Successful Visual Validation runs: `31930981030`, `31931147173`
- Successful artifacts: `9259315213`, `9259358605`
- Commit: `ff2254a12648db7d91d0a27526d529f5fe94fcd7`

## INC-007: Missing-request probe misread unresolved revision text

Status: Resolved
Layer: Transport publisher
First confirmed: 2026-08-16
Last confirmed: 2026-08-16

### Symptom

A fresh staged request could pass the stage signal and then fail in the trusted
Transport Publisher with `request ID already exists on chatgpt-dispatch with
different content`, even though `.chatgpt-requests/<request-id>` did not exist
on the current `chatgpt-dispatch` tree.

### Evidence

- ADR 0131 request `adr0131-36ffbb09-0822-r7` reached the trusted publisher but
  publisher run `31936294990` rejected it as a conflicting existing request.
  Reading the exact transport tree showed that request directory was absent.
- ADR 0112 independently reproduced the same false-positive with fresh request
  `adr0112-source-order-20260816-17` in publisher run `31937239061`; the exact
  `chatgpt-dispatch` tree at `f8cef7b8667b0e3f17d628b9efa7eea9a00f9ebc`
  also lacked the request directory.
- The publisher's optional lookup used
  `git rev-parse "$transport_head:$request_dir" 2>/dev/null || true` without
  `--verify`. For a missing tree, Git could emit the unresolved revision
  expression to stdout before returning non-zero, leaving command substitution
  non-empty after `|| true`.
- Infrastructure PR #76 changed that lookup to `git rev-parse --verify` and was
  merged as `6e5162f70b2c7f15666f8659ebd3978eaf3f00ef`.
- After the merge, the previously rejected immutable Phase 9 stage replayed
  unchanged: publisher run `31938218225` succeeded, published transport commit
  `7b9f3b79fdbcc980c5d79b96a4100ae7e97d271c`, and Dispatcher run
  `31938224725` successfully applied the product patch.
- Fresh post-fix publisher runs `31938262049`, `31938631848`, and `31939039568`
  also succeeded for the ADR 0131 implementation and corrections.

### Root cause

The trusted publisher treated non-empty stdout from a non-verifying
`git rev-parse` call as proof that the request tree already existed. Missing Git
objects can leave an unresolved revision expression on stdout, so the optional
existence probe confused a failed resolution with a valid object ID and routed
fresh requests into the request-ID conflict path.

### Resolution

Use `git rev-parse --verify "$transport_head:$request_dir"` for the optional
request-tree lookup. A real existing tree resolves to its object ID; a missing
tree now leaves `existing_tree` empty and follows the normal publication path.
PR #76 merged this one-line trusted-workflow fix, and both unchanged-stage replay
and fresh production requests subsequently published successfully.

### Prevention / ChatGPT next action

For optional Git object existence probes in trusted transport code, require
`git rev-parse --verify` or an equivalent object-existence check before treating
stdout as an object ID. When the publisher reports a request-ID conflict but the
transport tree appears not to contain that request, inspect the existence probe
before republishing, changing the product patch, or bypassing the trusted
publisher. Preserve the immutable stage and replay it only through the formal
workflow once the transport defect is corrected.

### References

- Failed ADR 0131 request: `adr0131-36ffbb09-0822-r7`
- Failed publisher run: `31936294990`
- Independent failed publisher run: `31937239061`
- Infrastructure PR: `#76`
- Infrastructure merge commit: `6e5162f70b2c7f15666f8659ebd3978eaf3f00ef`
- Confirmed unchanged-stage recovery publisher run: `31938218225`
- Confirmed unchanged-stage recovery Dispatcher run: `31938224725`
- Confirmed recovery transport commit: `7b9f3b79fdbcc980c5d79b96a4100ae7e97d271c`
- Fresh successful ADR 0131 publisher runs: `31938262049`, `31938631848`, `31939039568`

## Entry template

Copy this section for a materially new root cause. Incident IDs are monotonic
and never reused.

```markdown
## INC-NNN: Short root-cause title

Status: Resolved | Open | External blocker
Layer: Transport | Dispatcher | Concurrency | Validation | Visual validation | External
First confirmed: YYYY-MM-DD
Last confirmed: YYYY-MM-DD

### Symptom

Describe the externally visible failure: the rejected request, failing step,
validation gate, or misleading result that started the investigation.

### Evidence

List the small set of observations that establish the diagnosis. Include a
request ID, PR, workflow run, commit, or concise error text when useful.

### Root cause

State why the failure happened at the responsibility layer that owns the
problem. Do not stop at the final error message when an earlier protocol or
state error caused it.

### Resolution

Describe the recovery that was actually validated. If the correct action was
to abandon a stale request or wait for an external service, say so rather than
describing a code change that was not required.

### Prevention / ChatGPT next action

Write the rule ChatGPT should apply on the next occurrence. Prefer a concrete
decision such as "re-read the target branch and regenerate the request" over a
generic reminder to be careful.

### References

- Request: `<request-id>`
- PR: `#<number>`
- Workflow run: `<run-id or durable reference>`
- Commit: `<sha>`
```

## When to update the protocol instead

An incident entry explains a confirmed failure. Change
`docs/CHATGPT_AUTOMATION.md` as well when the durable lesson changes how every
future request should be produced, validated, retried, or reported. Change the
trusted `.github/**` automation only through its separately reviewed
automation-infrastructure branch and Draft PR; a normal dispatcher request is
not allowed to mutate its own trust boundary.