use super::{input_error_json, to_json, CliError, CliRunResult};
use engine_authoring::{
    load_scene_from_json, replace_file_contents, AuthoringCommand, AuthoringEntity,
    AuthoringPermission, AuthoringPermissions, AuthoringSession, ComponentSchema,
    ComponentSchemaRegistry, EntityId, ProjectId, ProjectRoot, SceneAuthoringError,
    SceneAuthoringService, SceneLoadError, SceneSaveError, StableId,
};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const SCENE_CAPABILITIES: [&str; 8] = [
    "project.describe",
    "scene.inspect",
    "scene.validate",
    "scene.preview",
    "scene.apply",
    "entity.find",
    "entity.inspect",
    "component.schemas",
];

#[derive(Debug, Serialize)]
struct ProjectDescribeOutput {
    project_id: ProjectId,
    name: String,
    engine_version: String,
    capabilities: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct EntityFindOutput {
    entities: Vec<AuthoringEntity>,
}

#[derive(Debug, Serialize)]
struct EntityInspectOutput {
    entity: Option<AuthoringEntity>,
}

#[derive(Debug, Serialize)]
struct ComponentSchemasOutput {
    schemas: Vec<ComponentSchema>,
}

#[derive(Debug, Serialize)]
struct AuthoringErrorOutput {
    success: bool,
    error: AuthoringErrorBody,
}

#[derive(Debug, Serialize)]
struct AuthoringErrorBody {
    code: &'static str,
    message: String,
}

struct SceneContext {
    path: PathBuf,
    session: AuthoringSession,
}

pub(super) fn dispatch(args: &[String]) -> Option<Result<CliRunResult, CliError>> {
    match args {
        [domain, command, project] if domain == "project" && command == "describe" => {
            Some(project_describe(Path::new(project)))
        }
        [domain, command, project, scene] if domain == "scene" && command == "inspect" => {
            Some(scene_inspect(Path::new(project), scene))
        }
        [domain, command, project, scene] if domain == "scene" && command == "validate" => {
            Some(scene_validate(Path::new(project), scene))
        }
        [domain, command, project, scene, commands]
            if domain == "scene" && command == "preview" =>
        {
            Some(scene_mutate(
                Path::new(project),
                scene,
                Path::new(commands),
                false,
            ))
        }
        [domain, command, project, scene, commands]
            if domain == "scene" && command == "apply" =>
        {
            Some(scene_mutate(
                Path::new(project),
                scene,
                Path::new(commands),
                true,
            ))
        }
        [domain, command, project, scene] if domain == "entity" && command == "find" => {
            Some(entity_find(Path::new(project), scene, ""))
        }
        [domain, command, project, scene, query] if domain == "entity" && command == "find" => {
            Some(entity_find(Path::new(project), scene, query))
        }
        [domain, command, project, scene, entity] if domain == "entity" && command == "inspect" => {
            Some(entity_inspect(Path::new(project), scene, entity))
        }
        [domain, command] if domain == "component" && command == "schemas" => {
            Some(component_schemas())
        }
        _ => None,
    }
}

fn project_describe(project_path: &Path) -> Result<CliRunResult, CliError> {
    let project = match ProjectRoot::open(project_path) {
        Ok(project) => project,
        Err(error) => {
            return Ok(input_error(
                "project_error",
                project_path,
                &error.to_string(),
            ));
        }
    };

    let output = ProjectDescribeOutput {
        project_id: project.project_id().clone(),
        name: project.config().name.clone(),
        engine_version: project.engine_version().to_owned(),
        capabilities: SCENE_CAPABILITIES.to_vec(),
    };
    Ok(CliRunResult::success(to_json(&output)?))
}

fn scene_inspect(project_path: &Path, scene_relative: &str) -> Result<CliRunResult, CliError> {
    let context = match load_scene_context(project_path, scene_relative) {
        Ok(context) => context,
        Err(result) => return Ok(result),
    };
    let service = SceneAuthoringService::new();

    match service.inspect(&context.session, &read_permissions()) {
        Ok(output) => Ok(CliRunResult::success(to_json(&output)?)),
        Err(error) => authoring_error(error),
    }
}

fn scene_validate(project_path: &Path, scene_relative: &str) -> Result<CliRunResult, CliError> {
    let context = match load_scene_context(project_path, scene_relative) {
        Ok(context) => context,
        Err(result) => return Ok(result),
    };
    let service = SceneAuthoringService::new();

    match service.validate(&context.session, &read_permissions()) {
        Ok(output) => Ok(CliRunResult::diagnostics(
            to_json(&output)?,
            !output.success,
        )),
        Err(error) => authoring_error(error),
    }
}

fn scene_mutate(
    project_path: &Path,
    scene_relative: &str,
    commands_path: &Path,
    persist: bool,
) -> Result<CliRunResult, CliError> {
    let mut context = match load_scene_context(project_path, scene_relative) {
        Ok(context) => context,
        Err(result) => return Ok(result),
    };
    let commands = match load_scene_commands(commands_path) {
        Ok(commands) => commands,
        Err(result) => return Ok(result),
    };
    let service = SceneAuthoringService::new();
    let permissions = write_permissions();
    let base = match service.inspect(&context.session, &permissions) {
        Ok(base) => base,
        Err(error) => return authoring_error(error),
    };

    let result = if persist {
        service.apply(
            &mut context.session,
            &permissions,
            base.revision,
            base.generation,
            commands,
        )
    } else {
        service.preview(
            &context.session,
            &permissions,
            base.revision,
            base.generation,
            commands,
        )
    };

    let mutation = match result {
        Ok(mutation) => mutation,
        Err(error) => return authoring_error(error),
    };

    if persist && mutation.success {
        let json = context
            .session
            .scene()
            .to_canonical_json()
            .map_err(scene_save_error_to_cli)?;
        replace_file_contents(&context.path, &json)
            .map_err(|source| CliError::Persist { source })?;
    }

    Ok(CliRunResult::diagnostics(
        to_json(&mutation)?,
        !mutation.success,
    ))
}

fn entity_find(
    project_path: &Path,
    scene_relative: &str,
    query: &str,
) -> Result<CliRunResult, CliError> {
    let context = match load_scene_context(project_path, scene_relative) {
        Ok(context) => context,
        Err(result) => return Ok(result),
    };
    let service = SceneAuthoringService::new();

    match service.find_entities(&context.session, &read_permissions(), query) {
        Ok(entities) => Ok(CliRunResult::success(to_json(&EntityFindOutput {
            entities,
        })?)),
        Err(error) => authoring_error(error),
    }
}

fn entity_inspect(
    project_path: &Path,
    scene_relative: &str,
    entity_text: &str,
) -> Result<CliRunResult, CliError> {
    let context = match load_scene_context(project_path, scene_relative) {
        Ok(context) => context,
        Err(result) => return Ok(result),
    };
    let entity = match EntityId::from_stable_id(StableId::new(entity_text)) {
        Ok(entity) => entity,
        Err(error) => {
            return Ok(input_error(
                "invalid_entity_id",
                Path::new("<entity-id>"),
                &error.to_string(),
            ));
        }
    };
    let service = SceneAuthoringService::new();

    match service.entity(&context.session, &read_permissions(), &entity) {
        Ok(entity) => Ok(CliRunResult::success(to_json(&EntityInspectOutput {
            entity,
        })?)),
        Err(error) => authoring_error(error),
    }
}

fn component_schemas() -> Result<CliRunResult, CliError> {
    let registry = ComponentSchemaRegistry::builtin();
    let output = ComponentSchemasOutput {
        schemas: registry.schemas().cloned().collect(),
    };
    Ok(CliRunResult::success(to_json(&output)?))
}

fn load_scene_context(
    project_path: &Path,
    scene_relative: &str,
) -> Result<SceneContext, CliRunResult> {
    let project = ProjectRoot::open(project_path)
        .map_err(|error| input_error("project_error", project_path, &error.to_string()))?;
    let scene_path = project.resolve_asset(scene_relative).map_err(|error| {
        input_error(
            "project_path_error",
            Path::new(scene_relative),
            &error.to_string(),
        )
    })?;
    let json = fs::read_to_string(&scene_path)
        .map_err(|error| input_error("io_error", &scene_path, &error.to_string()))?;
    let scene = load_scene_from_json(&json)
        .map_err(|error| scene_load_error(&scene_path, error))?;

    Ok(SceneContext {
        path: scene_path,
        session: AuthoringSession::new(scene),
    })
}

fn load_scene_commands(path: &Path) -> Result<Vec<AuthoringCommand>, CliRunResult> {
    let json = fs::read_to_string(path)
        .map_err(|error| input_error("io_error", path, &error.to_string()))?;
    serde_json::from_str(&json)
        .map_err(|error| input_error("invalid_json", path, &error.to_string()))
}

fn scene_load_error(path: &Path, error: SceneLoadError) -> CliRunResult {
    let kind = match &error {
        SceneLoadError::Json(_) => "invalid_json",
        SceneLoadError::DuplicateEntityId(_) => "invalid_scene",
        SceneLoadError::UnsupportedVersion { .. } => "unsupported_scene_version",
    };
    input_error(kind, path, &error.to_string())
}

fn scene_save_error_to_cli(error: SceneSaveError) -> CliError {
    match error {
        SceneSaveError::ValidationFailed { diagnostics } => CliError::Authoring { diagnostics },
        SceneSaveError::Json(error) => CliError::Json(error),
    }
}

fn authoring_error(error: SceneAuthoringError) -> Result<CliRunResult, CliError> {
    let output = AuthoringErrorOutput {
        success: false,
        error: AuthoringErrorBody {
            code: error.code(),
            message: error.to_string(),
        },
    };
    Ok(CliRunResult::diagnostics(to_json(&output)?, true))
}

fn input_error(kind: &'static str, path: &Path, message: &str) -> CliRunResult {
    CliRunResult::input_error(input_error_json(kind, path, message))
}

fn read_permissions() -> AuthoringPermissions {
    AuthoringPermissions::read_only()
}

fn write_permissions() -> AuthoringPermissions {
    AuthoringPermissions::read_only()
        .with(AuthoringPermission::Preview)
        .with(AuthoringPermission::ProjectDataWrite)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_cli_with_status;
    use engine_authoring::{
        AuthoringScene, ProjectConfig, PROJECT_SCHEMA_VERSION,
    };
    use serde_json::Value;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempProject {
        root: PathBuf,
        scene_relative: String,
    }

    impl TempProject {
        fn scene_path(&self) -> PathBuf {
            self.root.join("assets").join(&self.scene_relative)
        }

        fn commands_path(&self) -> PathBuf {
            self.root.join("scene-commands.json")
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn temp_project() -> TempProject {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "gameengine_scene_cli_{}_{}",
            std::process::id(),
            sequence
        ));
        fs::create_dir_all(&root).expect("temporary project directory");
        let project = ProjectRoot::create(
            &root,
            ProjectConfig {
                name: "Scene CLI Test".into(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project creation");
        let scene_relative = "scenes/main.scene.json".to_owned();
        let scene_path = project
            .resolve_asset_for_write(&scene_relative)
            .expect("scene write path");
        let json = AuthoringScene::new()
            .to_canonical_json()
            .expect("empty scene JSON");
        fs::write(scene_path, json).expect("scene fixture");

        TempProject {
            root,
            scene_relative,
        }
    }

    fn write_commands(project: &TempProject, commands: &[AuthoringCommand]) {
        fs::write(
            project.commands_path(),
            serde_json::to_string_pretty(commands).expect("command JSON"),
        )
        .expect("command fixture");
    }

    #[test]
    fn project_describe_reports_stable_project_identity_and_scene_capabilities() {
        let project = temp_project();
        let result = run_cli_with_status([
            "project".to_owned(),
            "describe".to_owned(),
            project.root.display().to_string(),
        ])
        .expect("project describe");
        let json: Value = serde_json::from_str(&result.output).expect("JSON output");

        assert_eq!(result.exit_code, 0);
        assert!(json["project_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("project_")));
        assert_eq!(json["name"], "Scene CLI Test");
        assert!(json["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .any(|capability| capability == "scene.apply"));
    }

    #[test]
    fn scene_preview_uses_shared_service_without_persisting() {
        let project = temp_project();
        let entity = EntityId::generate();
        write_commands(
            &project,
            &[AuthoringCommand::CreateEntity {
                id: entity.clone(),
                name: "previewed".into(),
                parent: None,
            }],
        );

        let result = run_cli_with_status([
            "scene".to_owned(),
            "preview".to_owned(),
            project.root.display().to_string(),
            project.scene_relative.clone(),
            project.commands_path().display().to_string(),
        ])
        .expect("scene preview");
        let json: Value = serde_json::from_str(&result.output).expect("JSON output");
        let persisted = load_scene_from_json(
            &fs::read_to_string(project.scene_path()).expect("persisted scene"),
        )
        .expect("persisted scene loads");

        assert_eq!(result.exit_code, 0);
        assert_eq!(json["success"], true);
        assert_eq!(json["diff"].as_array().expect("diff").len(), 1);
        assert!(persisted.entity(&entity).is_none());
    }

    #[test]
    fn scene_apply_matches_shared_service_diff_and_persists() {
        let project = temp_project();
        let entity = EntityId::generate();
        let command = AuthoringCommand::CreateEntity {
            id: entity.clone(),
            name: "shared_path".into(),
            parent: None,
        };
        write_commands(&project, std::slice::from_ref(&command));

        let source = load_scene_from_json(
            &fs::read_to_string(project.scene_path()).expect("source scene"),
        )
        .expect("source scene loads");
        let service = SceneAuthoringService::new();
        let permissions = write_permissions();
        let mut direct = AuthoringSession::new(source);
        let base = service
            .inspect(&direct, &permissions)
            .expect("direct inspect");
        let direct_result = service
            .apply(
                &mut direct,
                &permissions,
                base.revision,
                base.generation,
                vec![command],
            )
            .expect("direct apply");

        let result = run_cli_with_status([
            "scene".to_owned(),
            "apply".to_owned(),
            project.root.display().to_string(),
            project.scene_relative.clone(),
            project.commands_path().display().to_string(),
        ])
        .expect("scene apply");
        let json: Value = serde_json::from_str(&result.output).expect("JSON output");
        let persisted = load_scene_from_json(
            &fs::read_to_string(project.scene_path()).expect("persisted scene"),
        )
        .expect("persisted scene loads");

        assert_eq!(result.exit_code, 0);
        assert_eq!(json["success"], true);
        assert_eq!(
            json["diff"],
            serde_json::to_value(&direct_result.diff).expect("direct diff JSON")
        );
        assert_eq!(persisted.entity(&entity), direct.scene().entity(&entity));
    }

    #[test]
    fn entity_queries_read_the_same_scene_service_state() {
        let project = temp_project();
        let entity = EntityId::generate();
        write_commands(
            &project,
            &[AuthoringCommand::CreateEntity {
                id: entity.clone(),
                name: "hero".into(),
                parent: None,
            }],
        );
        run_cli_with_status([
            "scene".to_owned(),
            "apply".to_owned(),
            project.root.display().to_string(),
            project.scene_relative.clone(),
            project.commands_path().display().to_string(),
        ])
        .expect("seed scene");

        let find = run_cli_with_status([
            "entity".to_owned(),
            "find".to_owned(),
            project.root.display().to_string(),
            project.scene_relative.clone(),
            "hero".to_owned(),
        ])
        .expect("entity find");
        let find_json: Value = serde_json::from_str(&find.output).expect("find JSON");
        let inspect = run_cli_with_status([
            "entity".to_owned(),
            "inspect".to_owned(),
            project.root.display().to_string(),
            project.scene_relative.clone(),
            entity.as_str().to_owned(),
        ])
        .expect("entity inspect");
        let inspect_json: Value = serde_json::from_str(&inspect.output).expect("inspect JSON");

        assert_eq!(find.exit_code, 0);
        assert_eq!(find_json["entities"].as_array().expect("entities").len(), 1);
        assert_eq!(inspect.exit_code, 0);
        assert_eq!(inspect_json["entity"]["id"], entity.as_str());
    }

    #[test]
    fn scene_path_traversal_is_rejected_by_project_root() {
        let project = temp_project();
        let result = run_cli_with_status([
            "scene".to_owned(),
            "inspect".to_owned(),
            project.root.display().to_string(),
            "../project.json".to_owned(),
        ])
        .expect("path rejection is structured output");
        let json: Value = serde_json::from_str(&result.output).expect("JSON output");

        assert_eq!(result.exit_code, 2);
        assert_eq!(json["error"]["kind"], "project_path_error");
    }

    #[test]
    fn component_schemas_are_discovered_from_the_shared_builtin_registry() {
        let result =
            run_cli_with_status(["component".to_owned(), "schemas".to_owned()]).expect("schemas");
        let json: Value = serde_json::from_str(&result.output).expect("JSON output");

        assert_eq!(result.exit_code, 0);
        assert!(!json["schemas"].as_array().expect("schemas").is_empty());
    }
}
