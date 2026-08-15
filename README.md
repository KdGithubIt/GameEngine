# GameEngine

GameEngine is a Rust workspace for a desktop game runtime, editor, authoring tools, rendering, animation, physics, asset import, scripting, and related tooling.

The repository is under active development. Architecture decisions and contributor-facing invariants live in `AGENTS.md`, `docs/`, and `docs/adr/`.

## Toolchain

Use the Rust toolchain pinned by `rust-toolchain.toml`.

## Validation

From the repository root:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

Windows developers can run the repository validation script with:

```powershell
.\scripts\validate.ps1
```

Linux and macOS developers can run:

```bash
bash scripts/validate.sh
```

GitHub Actions uses the same affected/full validation model documented in `docs/DEVELOPMENT_WORKFLOW.md`.

## Automation

ChatGPT-authored changes use the repository-native dispatcher documented in `docs/CHATGPT_AUTOMATION.md`. Dispatcher pull requests stay Draft and are never auto-merged.

## Project status

APIs, editor workflows, project schemas, and supported formats may change while the engine is under active development. See the current specifications and accepted ADRs before making architectural changes.
