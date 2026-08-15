# GameEngine Development and Validation Workflow

This document defines the standard branch, validation, and CI workflow for changes in this repository.

## Toolchain

`rust-toolchain.toml` is the source of truth for the Rust version and required components. Local development and CI must use that pinned toolchain. Do not update Rust only to match a runner default or the newest stable release; toolchain updates are separate, intentional changes.

## Standard workflow

1. Create a dedicated work branch from the latest baseline branch.
2. Implement the scoped change.
3. During iteration, run the smallest validation that can answer the current question. Prefer affected-package tests, focused test filters, and targeted Clippy. A targeted `cargo check -p <package>` is optional when compile-only feedback is useful.
4. Do not repeatedly run the full workspace gate after every edit.
5. Run `cargo fmt --all`.
6. Before handoff, run the core local gate once:
   - `cargo fmt --all --check`
   - `cargo clippy --workspace --all-targets -- -D warnings`
   - `cargo test --workspace`
7. Review `git diff` and `git diff --stat` for unintended changes.
8. Remove temporary validation workflows, scripts, patches, and diagnostic logs.
9. Commit, push, and create a pull request.
10. Confirm the permanent GameEngine CI result.

The local core gate intentionally lets Clippy provide compile feedback instead of paying for a second full-workspace compilation during local handoff. The permanent Windows `full` validation path additionally runs `cargo check --workspace` explicitly and validates workspace documentation. Keep targeted `cargo check` available during implementation when it is the fastest useful feedback.

Windows developers can run the full local core gate from the repository root with:

```powershell
.\scripts\validate.ps1
```

Linux and macOS developers can run:

```bash
bash scripts/validate.sh
```

Both scripts stop immediately and return a non-zero exit code when a command fails. The local scripts intentionally remain full-workspace validation; affected validation is a CI scheduling optimization.

## Diagnose environment before changing code

When validation fails, first separate an implementation failure from a CI or environment failure.

- Confirm the pinned Rust version is active before treating a new Clippy warning as a code regression.
- Check Linux native dependencies before changing Rust code for errors from crates such as `alsa-sys`, `libudev-sys`, windowing backends, or input backends.
- Inspect the failing command's exit code and normal Actions log. Do not use `continue-on-error` to hide a failure.
- Separate pre-existing or platform-specific test failures from failures introduced by the current change.
- Fix only problems related to the current task. Do not perform broad Clippy cleanups or unrelated rewrites.
- Remove every temporary validation-only change after diagnosis is complete.

## Editing complete files on a work branch

Updating an existing text file as a complete file is allowed on a dedicated work branch when all of the following are true:

1. Read the current contents of the target file from the work branch before editing it.
2. Confirm that the same target file on the same work branch has not changed since it was read.
3. Review the pull request diff after the update.
4. Confirm there is no unintended large deletion, line-ending conversion, encoding conversion, or repository-wide formatting change.
5. Never overwrite `main` directly.

A later update to `main` does not by itself invalidate a complete-file edit already prepared on the work branch. The relevant concurrency check is whether the target file changed on that work branch after it was read. Resolve conflicts when the branch is merged or updated.

Prefer normal edits or content-based replacement over brittle line-number patches. Complete-file replacement is a practical fallback, not permission to skip diff review.

## CI contract

The permanent Windows validation workflow has three validation modes: `affected`, `full`, and `docs`.

### Affected PR validation

Normal pull requests and merge-group validation use changed-path classification. Known changes under one or more existing workspace crate directories select those changed packages only. Reverse dependents are deliberately excluded from the PR critical path; full validation on `main` and nightly recovers cross-workspace coverage.

The executor runs three Windows matrix jobs in parallel:

- formatting plus affected Clippy;
- affected package tests;
- affected package documentation.

For affected packages, Clippy uses package selection without `--all-targets`:

```text
cargo clippy -p <affected-package> ... -- -D warnings
```

Tests use normal selected-package tests with `cargo test -p <package>`, and documentation uses `cargo doc -p <package> --no-deps`. When several packages change, each gate receives the same planner-selected package set.

`crates/rig/**` maps naturally through Cargo metadata to the low-dependency `engine-rig` package. Changes confined to rig primitives therefore validate without selecting renderer, windowing, audio, importers, or the Rapier-backed high-level engine package merely because those packages depend on rig.

### Full validation

Full validation is selected when any of the following is true:

- the run is for a push to `main`;
- the run is the nightly scheduled validation;
- `Cargo.toml`, `Cargo.lock`, or `rust-toolchain.toml` changes;
- validation/build infrastructure changes, including the permanent validation workflow or validation scripts;
- the changed path cannot be classified safely.

A package-local `Cargo.toml` change may remain affected-mode when current Cargo metadata can classify it safely.

Full validation uses the same three parallel Windows gates and runs:

```text
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

The explicit workspace check runs only on `full` validation. Ordinary affected PRs keep the package-selected fast path instead of paying for a second full-workspace compilation.

### Documentation-only validation

A PR that changes only explicitly recognized GameEngine documentation paths is classified as `docs`. Rust compilation is skipped for that PR. Full validation still runs after the documentation change lands on `main` and during the nightly run.

### Exact-head and fallback safety

Dispatcher-triggered validation still resolves the requested branch through the GitHub API, verifies its exact 40-character HEAD SHA, and validates that exact commit. If the dispatcher run cannot identify a matching PR, affected classification is not guessed; the run compares the requested head with the current `main` baseline and still falls back to full validation whenever the changed paths are workspace-wide or cannot be classified safely.

Workspace package ownership comes from `cargo metadata`; package names and crate directories are not duplicated in the workflow classifier. Deleted or otherwise unclassifiable package paths force full validation.

### Caching and CI profiles

CI disables Cargo incremental compilation so cacheable compiler work can be reused through sccache, and it disables debug information in the CI `dev` and `test` profiles without disabling debug assertions or overflow checks. Hosted jobs do not restore the complete `target` directory. Self-hosted runners may use the configured persistent Cargo, target, and sccache paths.

The workflow does not run a separate `cargo fetch` step. Each gate lets Cargo fetch only what that command needs, avoiding a serialized dependency-fetch phase before compilation starts.

### Results and diagnostics

The workflow reports the selected mode, affected packages, and reason in the Actions summary and in the single validation PR comment.

A failing matrix gate uploads gate-specific diagnostics named like:

```text
gameengine-windows-validation-<gate>-diagnostics-<run-id>-<attempt>
```

Every run also uploads the machine-readable aggregate artifact:

```text
gameengine-windows-validation-<run-id>-<attempt>
```

Its `summary.json` uses schema version 4 and records the validation mode, changed and affected packages, selected gate package sets, aggregate executor outcome, and overall result.

The workflow run conclusion is authoritative. A PR fast-path run succeeds only when every matrix gate required by its selected mode succeeds.

## Performance target

For an ordinary PR that changes a known crate without Cargo/build configuration changes, the target wall-clock time is **2 to 5 minutes on a warm cache**. This is a target, not a correctness rule. Full `main`/nightly validation may take longer because broad workspace coverage is intentionally moved off the normal PR critical path.

The repository-native ChatGPT dispatcher protocol is defined in `CHATGPT_AUTOMATION.md`.
