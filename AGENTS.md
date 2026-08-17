# GameEngine Agent Instructions

Before changing this project, read and follow:

- `docs/AI_FRIENDLY_AUTHORING_SPEC.md`
- `docs/RUST_CODE_STYLE.md`
- `docs/DEVELOPMENT_WORKFLOW.md`
- `docs/CHATGPT_AUTOMATION.md` when using the repository-native ChatGPT Dispatcher
- Accepted records under `docs/adr/`

These documents are the canonical product, architecture, code style, development
workflow, and documentation specifications. Do not duplicate or redefine their
contracts in this file.

When a ChatGPT Dispatcher request fails, read
`docs/CHATGPT_AUTOMATION_INCIDENTS.md` before retrying. After the root cause is
confirmed and the recovery path succeeds, update the matching incident entry or
add a new root-cause entry. Record the symptom, root cause, resolution, and
prevention rule; do not turn the incident log into a chronological dump of CI
runs or unconfirmed guesses.

When an implementation decision is not covered by the specification:

1. Prefer a small, reversible change.
2. Preserve the separation between authoring data and the runtime ECS.
3. Add the decision to the specification or an ADR before relying on it across
   crate boundaries.
4. Do not silently change serialized formats, stable identifiers, or command
   semantics.

For game, demo, playable sample, level, or prototype requests, follow
`docs/AI_FRIENDLY_AUTHORING_SPEC.md` Section 5.3. A Rust example or
runtime-only setup alone is not a completed deliverable unless the user
explicitly asks for a code-only experiment.

During implementation, use targeted validation for the affected package,
module, or test set. Do not repeatedly run the full workspace validation suite
after every edit. Expand validation only when the changed API or behavior
crosses crate boundaries.

Before considering Rust work ready for handoff, run the core local gate once:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The local core gate intentionally lets Clippy provide compile feedback instead
of running a second full compilation. The permanent Windows full-validation
path additionally runs `cargo check --workspace` explicitly before Clippy, and
runs workspace documentation after tests. A targeted `cargo check -p <package>`
remains available when it is the fastest useful feedback during implementation.

When a Launcher or Editor change needs human-visible confirmation, follow
`docs/EDITOR_VISUAL_VALIDATION.md`. Visual validation is explicitly opt-in and
supplements rather than replaces the normal Rust validation result; do not
request it for changes that do not need screenshot review.

Normal pull requests, Dispatcher-triggered validation, merge-group validation,
and ordinary pushes to `main` use impact-based affected-workspace planning. CI
obtains the workspace package list and dependency graph from `cargo metadata`,
maps changed files to their owning workspace packages, and validates the changed
packages plus their transitive reverse dependents. Package names, crate
directories, and dependency relationships MUST NOT be duplicated in the
workflow classifier.

For PR-like validation, changed paths are computed from the merge base of the
current base revision and the exact validated head. Unrelated commits that land
on `main` after a task branch diverges MUST NOT be treated as task changes merely
because the current main tip moved.

Affected Clippy, tests, and documentation use the planner-selected affected
package set. A package-local `Cargo.toml` change may remain affected-mode when
current metadata can classify it safely. The workspace manifest, lock file,
pinned toolchain, CI/build/validation infrastructure, deleted or otherwise
unclassifiable package paths, and any other uncertain change force full
validation.

Nightly validation always runs full workspace Check, Clippy, tests, and
documentation. Ordinary pushes to `main` use affected planning unless their
changed paths require full validation. Documentation-only changes recognized by
the planner skip Rust compilation; nightly full validation remains the periodic
workspace-wide safety net.

Planner regression tests are validation-infrastructure tests. Run them when the
validation workflows or `scripts/ci/**` change, not once for every unrelated
product validation run.

Run documentation validation locally when changing public documentation,
rustdoc examples, or public APIs where a documentation failure is plausible,
and when diagnosing a documentation CI failure.

The local core-validation entry points are `scripts/validate.ps1` on Windows
and `scripts/validate.sh` on Linux/macOS. These scripts intentionally run the
full-workspace core local gate (formatting, Clippy, and tests); the affected-path
optimization is a CI scheduling policy. Workspace Check and documentation remain
part of Windows `full` validation rather than the local core scripts.
