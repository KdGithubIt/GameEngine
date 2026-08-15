# OSS Publication Plan

Status: Draft
Target license: MIT OR Apache-2.0
Target publication model: New public repository without importing the existing
private development history
First public version: v0.1.0
CLA: none
DCO: required
API stability: breaking changes allowed before 1.0
Scene/project schema stability: best-effort compatibility
Publication flow: private staging repository -> scan/CI/tag -> public

## Purpose

This document defines the phases for publishing the current GameEngine workspace
as open source software under MIT OR Apache-2.0. When project policy is
ambiguous, prefer the Apache-2.0 interpretation.

The recommended approach is to create a new clean repository and copy only the
files intended for public release. This reduces the risk of exposing unrelated
learning material, local IDE state, generated artifacts, private notes, or
unknown historical content from the current development repository.

## Publication Boundary

The public repository should use the current `GameEngine` directory as its new
repository root.

Expected public root:

```text
/
  Cargo.toml
  Cargo.lock
  rustfmt.toml
  LICENSE
  NOTICE
  README.md
  CONTRIBUTING.md
  SECURITY.md
  AGENTS.md
  crates/
  examples/
  docs/
  .github/
  .gitignore
```

The current top-level repository contents outside `GameEngine` are not part of
the publication boundary.

## Copy Into the New Repository

- `GameEngine/Cargo.toml`
- `GameEngine/Cargo.lock`
- `GameEngine/rustfmt.toml`
- `GameEngine/crates/`
- `GameEngine/examples/`
- `GameEngine/docs/`, after documentation review
- `GameEngine/AGENTS.md`, after checking that it is appropriate for public
  contributors

## Do Not Copy

- `target/`
- `.idea/`
- `docs.zip`
- `build_errors.txt`
- `.codex-task-*`
- `outputs/`
- top-level `Learning/`
- top-level `FrameWork/`
- local logs, editor state, temporary files, generated packages, and private
  working notes

## Phase OSS-0: New Repository Boundary

Goal: Define exactly what will become the public project.

Tasks:

- Create a new private staging repository.
- Treat the current `GameEngine` directory as the future repository root.
- Do not import the old git history.
- Decide the public project name, crate naming policy, and GitHub organization
  or owner.
- Decide whether all workspace crates are public in the first release:
  `engine`, `engine-renderer`, `engine-ecs`, `engine-authoring`,
  `engine-cli`, `engine-editor`, and `engine-mcp`.

Completion criteria:

- Public repository boundary is documented.
- Non-public directories and generated artifacts are excluded.
- The staging repository contains only intended public source files.

## Phase OSS-1: File Selection and Repository Hygiene

Goal: Ensure the staging repository contains no private or generated content.

Tasks:

- Copy only the approved files into the new repository.
- Add a public `.gitignore` that excludes Rust build outputs, IDE metadata,
  logs, generated archives, and temporary outputs.
- Remove stale build diagnostics and local task files.
- Review examples and docs for personal paths, machine-specific assumptions,
  private notes, and outdated instructions.
- Run a secret scan before the first public commit.

Completion criteria:

- Fresh clone contains no local build artifacts or IDE files.
- Secret scan has no findings.
- Public repository can be archived without exposing unrelated material.

## Phase OSS-2: Rights and License Audit

Goal: Confirm the project can be distributed under Apache-2.0.

Tasks:

- Confirm ownership of all source files, documentation, shaders, examples, and
  sample project assets.
- Identify copied third-party snippets, generated content, bundled assets, or
  externally sourced material.
- Audit direct and transitive Rust dependencies for license compatibility.
- Decide whether any files require additional attribution in `NOTICE`.
- Remove or replace any file with unclear rights.

Completion criteria:

- All included files are owned by the project or have compatible terms.
- Apache-2.0 compatibility risks are resolved.
- Required notices and attributions are known before licensing files are added.

## Phase OSS-3: License Application

Goal: Make the license clear to users, contributors, package registries, and
automated scanners.

Tasks:

- Add the official Apache License 2.0 text.
- Add the MIT License text.
- Add `NOTICE` if attribution notices are required.
- Add `license = "MIT OR Apache-2.0"` to every public crate manifest.
- Add `repository`, `description`, and other publication metadata to crate
  manifests.
- Add a license section to `README.md`.
- Decide whether source files need SPDX headers. If adopted, use
  `SPDX-License-Identifier: MIT OR Apache-2.0`.

Completion criteria:

- License scanners identify the repository and all crates as MIT OR Apache-2.0.
- The README and crate manifests agree on the license.
- Required notices are present.

## Phase OSS-4: Public Documentation

Goal: Let a new contributor clone, build, test, and run the project without
private context.

Tasks:

- Add or rewrite `README.md` for public users.
- Document prerequisites, supported platforms, Rust toolchain expectations,
  build commands, test commands, and example commands.
- Document editor and CLI entry points.
- Summarize architecture and crate responsibilities.
- Document current limitations and roadmap status.
- Add `CONTRIBUTING.md`.
- Add `SECURITY.md`.
- Add GitHub issue and pull request templates.
- Review existing documentation for mojibake, encoding problems, stale phase
  status, and private development notes.

Completion criteria:

- A new contributor can follow the README from clone to working build.
- Existing docs are readable and appropriate for public release.
- Contribution and security reporting paths are clear.

## Phase OSS-5: Quality Gates and CI

Goal: Make public builds repeatable.

Required local gates:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

Tasks:

- Add GitHub Actions for formatting, clippy, tests, and docs.
- Decide the minimum supported Rust version or pin a toolchain file.
- Run examples that are expected to work in the first public release.
- Decide whether graphics/editor tests need platform-specific handling.
- Document any intentionally unsupported targets.

Completion criteria:

- CI passes from a clean checkout.
- Local documented commands match CI.
- Any platform-specific gaps are documented.

## Phase OSS-6: Release and Distribution Policy

Goal: Define what "released" means for the first public version.

Tasks:

- Decide whether the first release is source-only or also includes binaries.
- Decide whether crates will be published to crates.io immediately or later.
- Define versioning policy, tag format, and release note format.
- Decide whether examples and sample projects are versioned with the engine.
- Decide how experimental APIs are labeled.

Completion criteria:

- First public release scope is clear.
- Release checklist exists.
- Users can distinguish stable, experimental, and internal APIs.

## Phase OSS-7: Community and Governance

Goal: Prepare the project to receive external issues and contributions.

Tasks:

- Define maintainer responsibilities.
- Require DCO sign-off and do not require a CLA.
- Define branch protection and review requirements.
- Define issue triage labels.
- Define security disclosure process.
- Add a code of conduct if the project will actively accept community
  contribution.

Completion criteria:

- Maintainers know how to handle issues, pull requests, and security reports.
- Contribution requirements are visible before contributors submit work.

## Phase OSS-8: Initial Publication

Goal: Publish the repository with a clean first release.

Tasks:

- Create the public repository or make the staging repository public.
- Push the initial clean commit.
- Verify GitHub renders README, license, and docs correctly.
- Create the first tag and release notes.
- Open initial roadmap and known-issue tickets.
- Announce only after clone, build, test, and example execution have been
  verified from the public repository.

Completion criteria:

- Public repository is usable from a clean clone.
- First release is tagged.
- Known limitations are documented.

## Recommended Order

```text
OSS-0 -> OSS-1 -> OSS-2 -> OSS-3 -> OSS-4 -> OSS-5 -> OSS-6 -> OSS-7 -> OSS-8
```

The highest-risk blockers are repository boundary mistakes, unclear asset or
code ownership, accidental inclusion of generated/private files, and unreadable
or stale documentation.
