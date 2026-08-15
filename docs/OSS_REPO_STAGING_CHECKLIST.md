# OSS Repository Staging Checklist

Status: Draft
Purpose: Concrete checklist for preparing a clean public staging repository
before publishing GameEngine under Apache-2.0.

This checklist assumes the public repository will be created from the current
`GameEngine` directory, without importing the existing private git history.

## 0. Decisions Before Copying

- [ ] Public repository name is decided.
- [ ] GitHub owner or organization is decided.
- [x] First public release version is decided: `v0.1.0`.
- [x] First public release is source-first.
- [x] Packaging/export is explicitly unsupported for `v0.1.0`.
- [ ] Official editor sample is decided, recommended: `examples/tiny_goal_project`.
- [ ] Supported standalone example is decided.
- [ ] Experimental examples are labeled as experimental.
- [ ] Public crate naming policy is decided.
- [x] MIT OR Apache-2.0 is confirmed as the target license.
- [x] CLA is not required.
- [x] DCO is required.
- [x] Breaking Rust API changes are allowed before `1.0`.
- [x] Scene/project schemas use best-effort compatibility.
- [x] Publication flow is private staging -> scan/CI/tag -> public.

## 1. Create Clean Staging Directory

Use a separate directory outside the current repository.

Example PowerShell variables:

```powershell
$Source = "C:\RustProject\RustProject\GameEngine"
$Stage = "C:\RustProject\GameEngine-public-stage"
```

Create the staging directory:

```powershell
New-Item -ItemType Directory -Force $Stage
```

Copy only public source files. Prefer allow-list copying over copying the whole
directory and deleting private files afterward.

Recommended allow-list:

```powershell
robocopy "$Source\crates" "$Stage\crates" /E
robocopy "$Source\examples" "$Stage\examples" /E
robocopy "$Source\docs" "$Stage\docs" /E
Copy-Item "$Source\Cargo.toml" "$Stage\Cargo.toml"
Copy-Item "$Source\Cargo.lock" "$Stage\Cargo.lock"
Copy-Item "$Source\rustfmt.toml" "$Stage\rustfmt.toml"
Copy-Item "$Source\AGENTS.md" "$Stage\AGENTS.md"
```

Do not copy:

- `target/`
- `.idea/`
- `docs.zip`
- `build_errors.txt`
- `.codex-task-*`
- local output directories
- private notes
- generated packages

## 2. Add Public Repository Files

Required:

- [ ] `LICENSE`
- [ ] `LICENSE-MIT`
- [ ] `NOTICE`, if attribution notices are required
- [ ] `README.md`
- [ ] `CONTRIBUTING.md`
- [ ] `SECURITY.md`
- [ ] `.gitignore`
- [ ] `.github/workflows/ci.yml`

Recommended:

- [ ] `.github/ISSUE_TEMPLATE/bug_report.md`
- [ ] `.github/ISSUE_TEMPLATE/feature_request.md`
- [ ] `.github/pull_request_template.md`
- [ ] `CHANGELOG.md`, or release notes policy in `README.md`

## 3. Public `.gitignore` Requirements

The public repository should ignore at least:

```gitignore
/target/
/.idea/
/.vscode/
*.log
*.pdb
*.ilk
*.obj
*.tmp
*.zip
outputs/
.codex-task-*
```

Do not ignore source assets needed by official samples.

## 4. Metadata Audit

For every crate manifest:

- [ ] `license = "MIT OR Apache-2.0"` is present.
- [ ] `description` is present.
- [ ] `repository` points to the public repository URL.
- [ ] `edition` is correct.
- [ ] `publish` policy is intentional.
- [ ] crate names are intentional and not placeholders.

Workspace crates to check:

- [ ] `crates/ecs/Cargo.toml`
- [ ] `crates/renderer/Cargo.toml`
- [ ] `crates/engine/Cargo.toml`
- [ ] `crates/authoring/Cargo.toml`
- [ ] `crates/cli/Cargo.toml`
- [ ] `crates/editor/Cargo.toml`
- [ ] `crates/mcp/Cargo.toml`

## 5. License and Rights Audit

Source and docs:

- [ ] All Rust source files are project-owned or have compatible provenance.
- [ ] WGSL shaders are project-owned or have compatible provenance.
- [ ] Rhai scripts are project-owned or have compatible provenance.
- [ ] Markdown docs are project-owned or have compatible provenance.
- [ ] Sample `.obj` files are project-owned or have compatible provenance.
- [ ] Generated or downloaded docs are excluded.

Dependencies:

- [ ] Direct dependency licenses are reviewed.
- [ ] Transitive dependency licenses are reviewed.
- [ ] Any attribution required by dependencies is captured in `NOTICE` or docs.
- [ ] Any incompatible dependency is removed, replaced, or made optional before
  publication.

Suggested commands:

```powershell
cargo metadata --format-version 1 > cargo-metadata.json
cargo tree --workspace > cargo-tree.txt
```

If a license scanning tool is available, run it in the staging repository and
store the result outside the source tree or in a release audit folder that will
not be shipped accidentally.

## 6. Secret and Private Context Scan

Run text searches before first commit:

```powershell
rg -n -i "api[_-]?key|secret|token|password|private|credential|localhost|C:\\Users|C:\\RustProject|TODO|FIXME" .
```

Review findings manually. Some matches are expected, such as documentation
about private repository history or ordinary TODO policy text, but every match
should be intentional.

Checklist:

- [ ] No API keys or tokens.
- [ ] No passwords.
- [ ] No private machine paths in public docs.
- [ ] No private planning notes outside approved OSS planning docs.
- [ ] No generated outputs.
- [ ] No IDE workspace state.

## 7. Documentation Readiness

README must include:

- [ ] What the project is.
- [ ] Current maturity level.
- [ ] Supported platforms.
- [ ] Rust toolchain expectation.
- [ ] Build command.
- [ ] Test command.
- [ ] Editor launch command.
- [ ] Official sample project flow.
- [ ] Standalone example flow.
- [ ] Known limitations.
- [ ] License section.

Known limitations must cover:

- [ ] Packaging/export status.
- [ ] Runtime hierarchy transform status.
- [ ] WASM status.
- [ ] Editor maturity.
- [ ] Supported asset formats.
- [ ] Experimental examples.
- [ ] API stability.

## 8. Supported Sample Verification

Official editor sample:

- [ ] Sample project exists in `examples/`.
- [ ] Sample uses only public assets.
- [ ] Sample does not depend on private paths.
- [ ] Sample starts Play mode without blocking diagnostics.
- [ ] README describes controls.
- [ ] README describes success condition.

Standalone example:

- [ ] Example compiles.
- [ ] Example launches manually on a supported desktop platform.
- [ ] Any required GPU/windowing assumptions are documented.
- [ ] Example name in README matches the actual Cargo example name.

Experimental examples:

- [ ] Experimental examples are not part of the quickstart.
- [ ] Experimental examples are labeled in docs or omitted from public docs.

## 9. Clean Checkout Verification

Run from the staging repository root:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

Expected result:

- [ ] Formatting passes.
- [ ] Clippy passes with warnings denied.
- [ ] Tests pass.
- [ ] Docs build without broken intra-doc links.

Optional manual checks:

```powershell
cargo run -p engine-cli -- --help
cargo run -p engine-editor
cargo run -p engine --example hello_window
```

Only document manual checks that have actually been verified.

## 10. CI Setup

Required workflow checks:

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo doc --workspace --no-deps`

Recommended CI behavior:

- [ ] Runs on pull requests.
- [ ] Runs on pushes to the default branch.
- [ ] Uses a pinned Rust toolchain or clearly documented stable toolchain.
- [ ] Avoids GPU/windowed examples unless a headless strategy is configured.

## 11. Initial Git Commit

Before first commit:

- [ ] `git status --short` shows only intended public files.
- [ ] License files are present.
- [ ] README quickstart works from the staging directory.
- [ ] Secret scan is complete.
- [ ] Quality gates pass or failures are intentionally documented before
  publication.

Initialize:

```powershell
git init
git add .
git status --short
git commit -m "Initial public source release"
```

Do not commit until the file list has been reviewed.

## 12. Pre-Public GitHub Setup

Create the GitHub repository as private first.

Checklist:

- [ ] Push initial branch.
- [ ] Confirm GitHub detects Apache-2.0 license.
- [ ] Confirm README renders correctly.
- [ ] Confirm CI runs and passes.
- [ ] Configure branch protection.
- [ ] Configure issue templates.
- [ ] Configure security policy.
- [ ] Add repository description and topics.
- [ ] Review repository file list in the GitHub UI.

## 13. Public Release Gate

Only make the repository public when all are true:

- [ ] Public boundary is correct.
- [ ] License and rights audit is complete.
- [ ] Secret scan is complete.
- [ ] CI passes.
- [ ] README quickstart is correct.
- [ ] Official sample path is documented.
- [ ] Known limitations are explicit.
- [ ] First release notes are drafted.

## 14. First Tag and Release

Recommended first tag:

```powershell
git tag v0.1.0
git push origin v0.1.0
```

Release notes should include:

- [ ] What is supported.
- [ ] What is experimental.
- [ ] Known limitations.
- [ ] Build/test status.
- [ ] License.
- [ ] Short roadmap.

## 15. Post-Public First Week

Track:

- [ ] First external clone/build report.
- [ ] First issue report.
- [ ] README confusion points.
- [ ] CI failures on external PRs.
- [ ] Missing docs for supported flows.
- [ ] Any accidentally exposed file that must be removed immediately.

Create follow-up issues for:

- [ ] Scene/project validation CLI.
- [ ] Packaging/export milestone.
- [ ] Transform hierarchy, if deferred.
- [ ] Example classification cleanup.
- [ ] Crates.io publication decision.
