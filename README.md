# GameEngine

GameEngine is a Rust workspace for a desktop game runtime, editor, authoring tools, rendering, animation, physics, asset import, scripting, and related tooling.

The repository is under active development. Architecture decisions and contributor-facing invariants live in `AGENTS.md`, `docs/`, and `docs/adr/`.

## Toolchain

Use the Rust toolchain pinned by `rust-toolchain.toml`.

## Validation

The local core gate from the repository root is:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Windows developers can run that full-workspace core gate with:

```powershell
.\scripts\validate.ps1
```

Linux and macOS developers can run:

```bash
bash scripts/validate.sh
```

Windows CI uses `affected`, `full`, and `docs` validation modes as documented in `docs/DEVELOPMENT_WORKFLOW.md`. Full validation additionally runs `cargo check --workspace` and `cargo doc --workspace --no-deps`; an affected or documentation-only success must not be reported as a full-workspace five-command result.

## Automation

ChatGPT-authored changes use the repository-native dispatcher documented in `docs/CHATGPT_AUTOMATION.md`. Dispatcher pull requests stay Draft and are never auto-merged.

## Project status

APIs, editor workflows, project schemas, and supported formats may change while the engine is under active development. See the current specifications and accepted ADRs before making architectural changes.
