//! Internal Cargo host for user-authored sources below `assets/scripts/rust`.

include!("project_modules.rs");

engine::export_game_module!();
