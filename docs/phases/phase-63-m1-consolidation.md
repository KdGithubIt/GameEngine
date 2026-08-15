# Phase 63: M1 Consolidation

Status: Implemented (2026-07-12)

## Scope Completed

- Added lightweight gameplay-system timing to the `busters_lite` HUD (last and
  maximum update cost in microseconds).
- Kept the vertical slice at six combatants and reused authoring scene loading;
  no duplicate renderer, asset loader, or engine runtime was introduced.
- Wired the editor `Package` button to the Phase 51 `package_project` API.
- The Package action loads project settings, requires a configured start scene,
  locates a prebuilt release player, asks for an output directory, and reports
  success/failure through structured editor diagnostics.
- Consolidated the remaining 17-D, 19-C/D, and 20-B manual checks in
  `docs/M1_ACCEPTANCE_CHECKLIST.md`.
- Reconciled the roadmap and accepted ADR index in the authoring specification.

## Package Workflow

1. Build the generic player once:
   `cargo build -p engine --release --bin player`.
2. Open a project in the editor.
3. Click `Package` and choose an output directory.
4. Run `game.exe` on Windows or `game` on other desktop platforms.

The editor first looks for a player next to the editor executable, then falls
back to the workspace `target/release` directory. Packaging never launches a
subprocess and continues to use ADR 0045's testable copy plan.

## Quality Gates

The required gates remain:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```
