# GameEngine Agent Instructions

Before changing this project, read and follow:

- `docs/AI_FRIENDLY_AUTHORING_SPEC.md`
- `docs/RUST_CODE_STYLE.md`
- `docs/DEVELOPMENT_WORKFLOW.md`
- Accepted records under `docs/adr/`

These documents are the canonical product, architecture, code style, development
workflow, and documentation specifications. Do not duplicate or redefine their
contracts in this file.

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

`cargo check --workspace` is intentionally not a separate final gate. Clippy
already performs workspace compilation with broader target coverage, and the
test gate performs code generation and executes the tests. A targeted
`cargo check -p <package>` remains available when it is the fastest useful
feedback during implementation.

Normal pull requests and merge-queue validation use affected-workspace planning.
CI obtains the workspace package list from `cargo metadata`, maps changed files
to their owning workspace packages, and validates the packages changed by that
change. Package names and crate directories MUST NOT be duplicated in the
workflow classifier.

Affected Clippy, tests, and documentation all use the planner-selected changed
package set. Reverse dependents are intentionally not added to the normal PR
critical path; full validation on `main` and nightly is the safety net for
cross-workspace coverage. A package-local `Cargo.toml` change may remain
affected-mode when current metadata can classify it safely. The workspace
manifest, lock file, pinned toolchain, CI/build configuration, deleted or
otherwise unclassifiable package paths, and any other uncertain change force
full validation.

Pushes to `main` and nightly validation always run full workspace Clippy,
tests, and documentation. Documentation-only changes recognized by the planner
skip Rust compilation on PR and merge-group fast paths, but still receive full
validation after landing on `main`.

Run documentation validation locally when changing public documentation,
rustdoc examples, or public APIs where a documentation failure is plausible,
and when diagnosing a documentation CI failure.

The local core-validation entry points are `scripts/validate.ps1` on Windows
and `scripts/validate.sh` on Linux/macOS. These scripts intentionally remain
full local gates; the affected-path optimization is a CI scheduling policy.
