use std::path::PathBuf;
use std::process::Command;

#[test]
fn headless_renderer_graph_excludes_window_backend() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|crates| crates.parent())
        .expect("engine-renderer must live under the GameEngine workspace");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let output = Command::new(cargo)
        .current_dir(workspace_root)
        .args([
            "tree",
            "-p",
            "engine-renderer",
            "--no-default-features",
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--color",
            "never",
        ])
        .output()
        .expect("cargo tree must be available during engine-renderer tests");

    assert!(
        output.status.success(),
        "cargo tree failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let tree = String::from_utf8(output.stdout).expect("cargo tree output must be UTF-8");
    assert!(
        !tree.lines().any(|line| line.starts_with("winit v")),
        "headless engine-renderer must not include winit:\n{tree}"
    );
}
