//! Regression coverage for the portability of persisted prefab instance data.
//!
//! ADR 0021 requires canonical project documents to reference assets in a
//! project-relative form. ADR 0134 applies that rule to the
//! `editor.prefab_instance` marker, which previously stored the authoring
//! machine's canonicalized absolute path directly inside `*.scene.json`.

use engine_authoring::{
    replace_file_contents, AuthoringEntity, AuthoringScene, EntityId, PrefabAsset, ProjectConfig,
    ProjectRoot, PROJECT_SCHEMA_VERSION,
};
use engine_cli::run_cli_with_status;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn instantiate_persists_a_project_relative_prefab_source() {
    let project = TempProject::create("prefab_source");
    let scene_relative = "scenes/main.scene.json";
    let prefab_relative = "prefabs/hero.prefab.json";

    let result = run_cli_with_status([
        "prefab".to_owned(),
        "instantiate".to_owned(),
        project.path().display().to_string(),
        scene_relative.to_owned(),
        prefab_relative.to_owned(),
    ])
    .expect("prefab instantiate must run");
    assert_eq!(result.exit_code, 0, "{}", result.output);

    let saved = fs::read_to_string(project.path().join("assets").join(scene_relative))
        .expect("instantiated scene must be readable");
    assert!(
        saved.contains("\"source\": \"assets/prefabs/hero.prefab.json\""),
        "the marker must persist the project-relative prefab path: {saved}"
    );

    // The project directory name is unique per run, so its absence proves no
    // part of the authoring machine's layout reached canonical project data.
    let leaked = project
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("temporary project name must be UTF-8");
    assert!(
        !saved.contains(leaked),
        "the scene must not leak the authoring machine's directory layout: {saved}"
    );
}

#[test]
fn preview_reports_the_same_project_relative_source_without_writing() {
    let project = TempProject::create("prefab_preview");
    let scene_path = project.path().join("assets/scenes/main.scene.json");
    let before = fs::read_to_string(&scene_path).expect("scene fixture must be readable");

    let result = run_cli_with_status([
        "prefab".to_owned(),
        "preview".to_owned(),
        project.path().display().to_string(),
        "scenes/main.scene.json".to_owned(),
        "prefabs/hero.prefab.json".to_owned(),
    ])
    .expect("prefab preview must run");

    assert_eq!(result.exit_code, 0, "{}", result.output);
    assert!(
        result
            .output
            .contains("\"source\": \"assets/prefabs/hero.prefab.json\""),
        "preview must report the same portable source as apply: {}",
        result.output
    );
    assert_eq!(
        fs::read_to_string(&scene_path).expect("scene must remain readable"),
        before,
        "preview must not write the scene"
    );
}

/// Disposable project fixture holding one empty scene and one prefab.
struct TempProject {
    root: PathBuf,
}

impl TempProject {
    fn create(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time must be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "engine_cli_{name}_{}_{}",
            std::process::id(),
            nonce
        ));
        fs::create_dir_all(&root).expect("temporary project directory must be creatable");
        let project = ProjectRoot::create(
            &root,
            ProjectConfig {
                name: "PrefabSourceTest".into(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project fixture must be creatable");

        fs::create_dir_all(project.assets_root().join("prefabs"))
            .expect("prefab directory must be creatable");

        let id = EntityId::generate();
        let entity = AuthoringEntity::new(id.clone(), "hero");
        let prefab = PrefabAsset::from_selection([(&id, &entity)]).expect("prefab fixture");
        replace_file_contents(
            &project.assets_root().join("prefabs/hero.prefab.json"),
            &prefab.to_json().expect("prefab fixture must serialize"),
        )
        .expect("prefab fixture must write");

        replace_file_contents(
            &project.assets_root().join("scenes/main.scene.json"),
            &AuthoringScene::new()
                .to_canonical_json()
                .expect("empty scene must serialize"),
        )
        .expect("scene fixture must write");

        // `ProjectRoot::create` canonicalizes, so later `strip_prefix` checks
        // compare the same form the CLI resolves paths against.
        Self {
            root: project.path().to_path_buf(),
        }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
