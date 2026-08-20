# Internal Rust Build Host

User-authored Rust code lives below `../assets/scripts/rust/` and is shown in the Asset Browser. Folders below that root are free-form and become Rust module paths. This crate contains only Cargo integration and the generated `src/project_modules.rs` index; do not move user scripts into `game/src/` and do not add `mod.rs` files below the Rust script root. Open the project in Engine Editor once on each machine so it can refresh the index and write the standard `.cargo/config.toml` SDK path.

## Validate

From this `game/` directory run:

```text
cargo check --all-targets
```
