# GameEngine ChatGPT Direct Worker

Status: Operational helper
Canonical automation protocol: `docs/CHATGPT_AUTOMATION.md`

## Purpose

`scripts/public/gameengine_chatgpt_worker.py` is a convenience wrapper for the
existing **Direct builder lifecycle** in `docs/CHATGPT_AUTOMATION.md`.
It does not introduce a second Bridge, a new trusted writer, or a replacement
Dispatcher protocol.

Use it only when the producer has a real `KdGithubIt/GameEngine` checkout plus
`git`, `gh`, and Python. Connector-only ChatGPT sessions continue to use the
trusted producer path defined by the canonical protocol.

The worker is intended for large changes where transporting dozens of structured
connector edits would cost more time than materializing one already-generated
product patch in a disposable checkout.

## Trust model

The input patch is **authoring input only**. It is never copied directly into
`.chatgpt-requests/**`.

The worker:

1. resolves the exact remote target HEAD and current `main`;
2. creates a disposable worktree at the exact target HEAD;
3. applies the input patch to that worktree with `git apply --index
   --whitespace=error-all`;
4. invokes `.github/chatgpt/request_protocol.py build`;
5. uses only the builder-emitted schema-v2 parts and `ready.json`;
6. publishes them to a fresh `chatgpt-dispatch-stage-<request-id>` branch with
   patch parts first and `ready.json` in a separate final commit;
7. relies on the existing read-only stage signal, trusted transport publisher,
   and trusted Dispatcher;
8. waits for Windows Validation;
9. if the resulting PR requests Visual Validation, waits for that workflow too,
   but reports that human/ChatGPT screenshot review is still required.

The worker never pushes `main`, never directly advances `chatgpt-dispatch`, and
never uses the legacy private-repository Bridge events.

## Usage

```text
python scripts/public/gameengine_chatgpt_worker.py \
  --workspace . \
  --patch-file <product.patch> \
  --target-branch chatgpt/gameengine-<task> \
  --request-id <unique-request-id> \
  --commit-message "<one-line commit message>" \
  --pr-title "<one-line PR title>" \
  --pr-body-file <pr-body.md>
```

The target branch must already exist and contain the current `main` baseline as
required by the canonical Direct builder lifecycle. The request ID is immutable:
if its stage branch already exists, choose a new request ID.

The patch may be large enough to require multiple Dispatcher parts. The worker
does not impose a legacy 50 KB patch limit. Part sizing, total size, byte hashes,
newline-boundary splitting, path checks, and strict applicability are owned by
`request_protocol.py` and the canonical protocol.

## Completion semantics

A zero exit status means the stage signal, trusted publisher, Dispatcher, and
Windows Validation workflows all completed successfully. It also confirms that
the target branch advanced and an open PR matching that exact target HEAD exists.

When the PR contains a GameEngine visual-validation marker, a zero exit status
also means the Visual Validation workflow completed successfully. It does **not**
mean the screenshot itself passed human review. The JSON result reports
`workflow-success-human-review-required` so the caller must still retrieve and
inspect the visual artifact before declaring Visual Validation PASS.

On failure the worker exits non-zero and names the responsible stage. Do not
bypass a failed publisher/Dispatcher by directly writing the protected transport
or product branches. Diagnose against `docs/CHATGPT_AUTOMATION_INCIDENTS.md` and
follow the recovery path in the canonical protocol.
