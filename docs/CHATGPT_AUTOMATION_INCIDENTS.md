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
INC-001  Markdown trailing whitespace rejected by patch preflight  Dispatcher  Resolved
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
