# Public Repository Migration

Status: Public cutover completed 2026-08-15; legacy private automation retirement prepared in `KdGithubIt/RustProject` PR #72
Target repository: `KdGithubIt/GameEngine`
Source repository: `KdGithubIt/RustProject`
Audited source snapshot: `68e136f9a06b52f6e024b4f895f7db5c8510ad00`
Source boundary: `GameEngine/`
History policy: clean snapshot; the private repository history was not imported

## Result

GameEngine development and Windows GitHub Actions validation now run from the dedicated public `KdGithubIt/GameEngine` repository. The public repository uses the former `GameEngine/` workspace as its repository root, while `KdGithubIt/RustProject` remains the private migration source.

The audited standalone snapshot was merged through public bootstrap PR #1. Its full Windows validation passed formatting, workspace Check, workspace Clippy with all targets and warnings denied, workspace tests, and workspace documentation. The repository-native ChatGPT Dispatcher was then exercised end to end with temporary PR #2; its `affected` validation selected `engine-ecs` and succeeded. PR #2 was closed without merge and its probe branch was reset to `main`.

The remaining private-side cutover cleanup is represented by `KdGithubIt/RustProject` PR #72, which removes the five legacy GameEngine-specific workflows while leaving the unrelated private `deploy.yml` workflow and old `GameEngine/` source tree intact. That retirement becomes effective only after PR #72 is merged.

## Public snapshot contract

`scripts/public/Export-PublicSnapshot.ps1` records the initial publication boundary. It uses an allow-list rather than copying the whole former `GameEngine/` directory and deleting files afterward.

The snapshot includes:

- workspace manifests and pinned toolchain files;
- `crates/`;
- `examples/`;
- `scripts/`;
- Markdown files under `docs/`;
- `AGENTS.md`, `CLAUDE.md`, and `README.md`;
- exactly three public workflows from `.github/workflows/`.

The snapshot intentionally excludes private/generated development material such as `.codex_tmp/`, IDE metadata, diagnostic logs, generated archives, the legacy `GameEngine-ChatGPT-Apply/` fallback, and binary Word/PowerPoint manuals.

Model/media/font/archive files are rejected unless their repository-relative path is explicitly reviewed in `scripts/public/public-asset-allowlist.txt`. The exporter also runs high-confidence credential and private-user-path checks without printing matched secret values.

## Public workflow set

The public repository contains these GameEngine workflows:

1. `gameengine-chatgpt-dispatch-trigger.yml` — read-only signal for immutable ChatGPT request records.
2. `gameengine-chatgpt-dispatcher.yml` — trusted default-branch workflow that applies authorized patches and creates/updates Draft pull requests.
3. `gameengine-windows-validation.yml` — exact-head Windows validation with affected/full/docs planning.

The legacy bridge and auto-merge workflows were not migrated. Public dispatcher pull requests remain Draft and require a human merge decision.

Third-party actions in these public workflows are pinned to full commit SHAs.

## Public dispatcher trust boundary

The `chatgpt-dispatch` branch is transport data, not executable automation. The signal workflow has read-only repository access. The write-capable dispatcher is loaded from `main` via `workflow_run` and repeats all request validation.

In the standalone public repository, ChatGPT patch paths are explicitly allow-listed. Product/source/documentation paths are allowed, while `.github/**` and `.chatgpt-requests/**` are forbidden. This prevents a patch request from rewriting the trusted workflow definitions or its own transport records.

The dispatcher requires the exact target branch `expected_head_sha`, validates with `git apply --check`, rejects symlinks/submodules, re-reads the remote branch before push, and pushes with an exact force-with-lease.

## Validation layout compatibility

`scripts/ci/New-ValidationPlan.ps1` discovers the Git repository root independently from the workspace root. The planner retains compatibility with both layouts during cutover support:

```text
RustProject/GameEngine/   # legacy private migration-source layout
GameEngine/               # standalone repository root when cloned into GameEngine/
```

`Test-ValidationPlanner.ps1` exercises both layouts so path-classification changes cannot silently regress the retained migration compatibility.

## Completed cutover sequence

1. Selected and fixed the private source snapshot at `68e136f9a06b52f6e024b4f895f7db5c8510ad00`.
2. Created the clean `KdGithubIt/GameEngine` repository without importing private Git history.
3. Exported the allow-listed `GameEngine/` boundary and ran publication safety checks before publishing the snapshot branch.
4. Installed only the approved public Dispatcher signal, Dispatcher, and Windows Validation workflows.
5. Ran full public Windows validation and confirmed all five workspace commands succeeded.
6. Merged bootstrap PR #1 to `main`.
7. Enabled GitHub Actions to create Dispatcher Draft pull requests.
8. Exercised the production Dispatcher path end to end with PR #2 and confirmed `affected` validation success for `engine-ecs`.
9. Closed PR #2 without merge and reset its probe branch to `main`.
10. Prepared `KdGithubIt/RustProject` PR #72 to retire the legacy private GameEngine workflows.

## Post-cutover requirements

- Merge `KdGithubIt/RustProject` PR #72 before treating private GameEngine Actions retirement as effective.
- Keep the old private `GameEngine/` tree as migration history until a separate explicit cleanup decision is made.
- Remove any temporary migration-only credential from the public repository if it is still configured; normal Dispatcher and validation workflows do not require it.
- Continue to keep Dispatcher pull requests Draft and require a human merge decision.
- Treat `affected`, `full`, and `docs` results according to their actual validation scope; never report an affected success as full-workspace success.

## Historical pre-public stop conditions

The snapshot was not eligible for publication until all of the following were satisfied:

- reviewed model/media/font/archive paths only;
- no high-confidence credential or private-user-path findings;
- exactly the approved public workflow set;
- successful standalone Windows validation;
- Dispatcher trust boundaries preventing `.github/` or `.chatgpt-requests/` mutation;
- an unchanged audited private source SHA.

## Actions usage objective

The standalone public repository uses standard GitHub-hosted `windows-latest` and `ubuntu-latest` runners by default. Self-hosted Windows runners remain an explicit repository-variable opt-in, and fork pull requests are never eligible for self-hosted runner selection. Retiring the legacy private GameEngine workflows prevents duplicate private-repository validation and nightly Actions usage after cutover.
