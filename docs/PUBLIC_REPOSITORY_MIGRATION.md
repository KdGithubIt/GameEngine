# Public Repository Migration

Status: Accepted implementation plan
Target repository: `KdGithubIt/GameEngine`
Source repository: `KdGithubIt/RustProject`
Source boundary: `GameEngine/`
History policy: clean snapshot; do not import the private repository history

## Goal

Move GameEngine development and its Windows GitHub Actions validation to a dedicated public repository without exposing unrelated private repository content or private development history.

The source `RustProject` repository remains private. The public repository uses the current `GameEngine/` workspace as its repository root.

## Public snapshot contract

`scripts/public/Export-PublicSnapshot.ps1` is the source of truth for the initial file boundary. It uses an allow-list rather than copying the whole `GameEngine/` directory and deleting files afterward.

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

The public repository contains only these initial workflows:

1. `gameengine-chatgpt-dispatch-trigger.yml` — read-only signal for immutable ChatGPT request records.
2. `gameengine-chatgpt-dispatcher.yml` — trusted default-branch workflow that applies authorized patches and creates/updates Draft pull requests.
3. `gameengine-windows-validation.yml` — exact-head Windows validation with affected/full/docs planning.

The legacy bridge and auto-merge workflows are not exported. Public dispatcher pull requests remain Draft and require a human merge decision.

Third-party actions in these public workflow templates are pinned to full commit SHAs.

## Public dispatcher trust boundary

The `chatgpt-dispatch` branch remains transport data, not executable automation. The signal workflow has read-only repository access. The write-capable dispatcher is loaded from `main` via `workflow_run` and repeats all request validation.

In the standalone public repository, ChatGPT patch paths are explicitly allow-listed. Product/source/documentation paths are allowed, while `.github/**` and `.chatgpt-requests/**` are forbidden. This prevents a patch request from rewriting the trusted workflow definitions or its own transport records.

The dispatcher requires the exact target branch `expected_head_sha`, validates with `git apply --check`, rejects symlinks/submodules, re-reads the remote branch before push, and pushes with an exact force-with-lease.

## Validation layout compatibility

`scripts/ci/New-ValidationPlan.ps1` discovers the Git repository root independently from the workspace root. The same planner therefore supports both layouts:

```text
RustProject/GameEngine/   # current private monorepo layout
GameEngine/               # standalone public repository root
```

`Test-ValidationPlanner.ps1` exercises both layouts so path-classification changes cannot silently break either migration phase.

## Migration sequence

1. Run `scripts/public/Test-PublicSnapshot.ps1` against the latest private `main` state.
2. Export a fresh snapshot with `Export-PublicSnapshot.ps1`.
3. Create `KdGithubIt/GameEngine` as a new private repository; do not fork or import old history.
4. Commit only the exported snapshot as the new repository's initial history.
5. Run the Windows validation workflow from the private staging repository.
6. Configure default-branch protection/rules for pull-request-based changes and required validation.
7. Confirm repository secrets/variables are empty except settings intentionally created for the new repository. Do not copy old repository secrets wholesale.
8. Change the new repository visibility to public only after the snapshot boundary and validation result are clean.
9. Run the public standard-hosted Windows validation once after publication.
10. Exercise one ChatGPT dispatcher request end-to-end: request transport, Draft PR, exact-head Windows validation, and result inspection.
11. After the standalone path is stable, disable GameEngine-specific validation/automation in the private monorepo. Do not delete the old GameEngine tree as part of the initial cutover.

## Stop conditions

Do not make the staging repository public when any of the following is unresolved:

- an unreviewed model/media/font/archive file is present;
- the security scan reports a high-confidence finding;
- the exported workflow set differs from the three approved workflows;
- standalone Windows validation fails;
- the dispatcher can mutate `.github/` or `.chatgpt-requests/`;
- the source branch moved after the audited snapshot was selected.

## Actions usage objective

The standalone repository is designed to use standard GitHub-hosted `windows-latest` and `ubuntu-latest` runners by default. Self-hosted Windows runners remain an explicit repository-variable opt-in, and fork pull requests are never eligible for self-hosted runner selection.
