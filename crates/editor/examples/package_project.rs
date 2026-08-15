//! Packages an editor project with explicitly built Player and GameModule binaries.
//!
//! This small acceptance utility uses the same packaging functions as the
//! editor button. It exists so release verification can be repeated without
//! automating native file-picker dialogs.

use engine_editor::build::{package_project_with_game_module, BuildConfig};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let project_path = PathBuf::from(arguments.next().ok_or(
        "usage: package_project <project-root> <output-dir> <player-binary> <game-module>",
    )?);
    let output_dir = PathBuf::from(arguments.next().ok_or("missing output directory")?);
    let player_binary = PathBuf::from(arguments.next().ok_or("missing player binary")?);
    let game_module = PathBuf::from(arguments.next().ok_or("missing game module")?);
    if arguments.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let project = engine_authoring::ProjectRoot::open(&project_path)?;
    let settings = engine_authoring::ProjectSettings::load(project.path())?;
    let manifest_json = std::fs::read_to_string(project.path().join("asset_manifest.json"))?;
    let manifest = engine::AssetManifest::from_json(&manifest_json)?;
    let config = BuildConfig {
        project_root: project.path().to_path_buf(),
        output_dir,
        start_scene: settings.start_scene,
    };
    let plan =
        package_project_with_game_module(&config, &manifest, &player_binary, Some(&game_module))?;
    println!(
        "Packaged {} files into {}",
        plan.copies.len(),
        config.output_dir.display()
    );
    Ok(())
}
