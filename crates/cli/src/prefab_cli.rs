use super::{asset_cli, scene_cli, to_json, CliError, CliRunResult};
use engine_assets::prefab::{PrefabAssetError, PrefabAssetService};
use engine_authoring::{
    replace_file_contents, AuthoringPermission, AuthoringPermissions, EntityId,
    PrefabAuthoringError, PrefabAuthoringService, PrefabInstantiationRequest,
    SceneAuthoringService, SceneSaveError, StableId,
};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
struct ServiceErrorOutput {
    success: bool,
    error: ServiceErrorBody,
}

#[derive(Debug, Serialize)]
struct ServiceErrorBody {
    code: &'static str,
    message: String,
}

pub(super) fn dispatch(args: &[String]) -> Option<Result<CliRunResult, CliError>> {
    match args {
        [domain, command, project, scene, root, destination]
            if domain == "prefab" && command == "create" =>
        {
            Some(prefab_create(
                Path::new(project),
                scene,
                root,
                destination,
            ))
        }
        [domain, command, project, scene, source]
            if domain == "prefab" && command == "preview" =>
        {
            Some(prefab_mutate(
                Path::new(project),
                scene,
                source,
                None,
                false,
            ))
        }
        [domain, command, project, scene, source, parent]
            if domain == "prefab" && command == "preview" =>
        {
            Some(prefab_mutate(
                Path::new(project),
                scene,
                source,
                Some(parent.as_str()),
                false,
            ))
        }
        [domain, command, project, scene, source]
            if domain == "prefab" && command == "instantiate" =>
        {
            Some(prefab_mutate(
                Path::new(project),
                scene,
                source,
                None,
                true,
            ))
        }
        [domain, command, project, scene, source, parent]
            if domain == "prefab" && command == "instantiate" =>
        {
            Some(prefab_mutate(
                Path::new(project),
                scene,
                source,
                Some(parent.as_str()),
                true,
            ))
        }
        _ => None,
    }
}

fn prefab_create(
    project_path: &Path,
    scene_relative: &str,
    root_text: &str,
    destination: &str,
) -> Result<CliRunResult, CliError> {
    let root = match parse_entity_id(root_text) {
        Ok(root) => root,
        Err(result) => return Ok(result),
    };
    let context = match scene_cli::load_scene_context(project_path, scene_relative) {
        Ok(context) => context,
        Err(result) => return Ok(result),
    };
    let (project, mut manifest) = match asset_cli::load_catalog(project_path) {
        Ok(catalog) => catalog,
        Err(result) => return Ok(result),
    };
    let result = PrefabAssetService::new().create_from_root(
        &project,
        &mut manifest,
        &prefab_permissions(),
        context.session.scene(),
        &root,
        destination,
    );
    match result {
        Ok(output) => Ok(CliRunResult::success(to_json(&output)?)),
        Err(error) => prefab_asset_error(error),
    }
}

fn prefab_mutate(
    project_path: &Path,
    scene_relative: &str,
    source: &str,
    parent_text: Option<&str>,
    persist: bool,
) -> Result<CliRunResult, CliError> {
    let parent = match parent_text.map(parse_entity_id).transpose() {
        Ok(parent) => parent,
        Err(result) => return Ok(result),
    };
    let mut context = match scene_cli::load_scene_context(project_path, scene_relative) {
        Ok(context) => context,
        Err(result) => return Ok(result),
    };
    let (project, _manifest) = match asset_cli::load_catalog(project_path) {
        Ok(catalog) => catalog,
        Err(result) => return Ok(result),
    };
    let permissions = prefab_permissions();
    let loaded = match PrefabAssetService::new().load(&project, &permissions, source) {
        Ok(loaded) => loaded,
        Err(error) => return prefab_asset_error(error),
    };
    let snapshot = match SceneAuthoringService::new().inspect(&context.session, &permissions) {
        Ok(snapshot) => snapshot,
        Err(error) => return Ok(service_error(error.code(), error.to_string())),
    };
    let revision = snapshot.revision;
    let generation = snapshot.generation;
    let request = PrefabInstantiationRequest::new(loaded.source, parent, revision, generation);
    let service = PrefabAuthoringService::new();
    let result = if persist {
        service.apply_instantiation(
            &mut context.session,
            &permissions,
            &loaded.prefab,
            request,
        )
    } else {
        service.preview_instantiation(
            &context.session,
            &permissions,
            &loaded.prefab,
            request,
        )
    };
    let output = match result {
        Ok(output) => output,
        Err(error) => return prefab_authoring_error(error),
    };

    if persist && output.mutation.success {
        let json = context
            .session
            .scene()
            .to_canonical_json()
            .map_err(scene_save_error_to_cli)?;
        replace_file_contents(&context.path, &json)
            .map_err(|source| CliError::Persist { source })?;
    }

    Ok(CliRunResult::diagnostics(
        to_json(&output)?,
        !output.mutation.success,
    ))
}

fn parse_entity_id(text: &str) -> Result<EntityId, CliRunResult> {
    EntityId::from_stable_id(StableId::new(text)).map_err(|error| {
        service_error(
            "invalid_entity_id",
            format!("entity ID `{text}` is invalid: {error}"),
        )
    })
}

fn prefab_permissions() -> AuthoringPermissions {
    AuthoringPermissions::read_only()
        .with(AuthoringPermission::Preview)
        .with(AuthoringPermission::ProjectDataWrite)
        .with(AuthoringPermission::AssetWrite)
}

fn prefab_asset_error(error: PrefabAssetError) -> Result<CliRunResult, CliError> {
    Ok(service_error(error.code(), error.to_string()))
}

fn prefab_authoring_error(error: PrefabAuthoringError) -> Result<CliRunResult, CliError> {
    Ok(service_error(error.code(), error.to_string()))
}

fn service_error(code: &'static str, message: String) -> CliRunResult {
    let output = ServiceErrorOutput {
        success: false,
        error: ServiceErrorBody { code, message },
    };
    let json = serde_json::to_string_pretty(&output).unwrap_or_else(|_| {
        "{\"success\":false,\"error\":{\"code\":\"internal_error\"}}".into()
    });
    CliRunResult::diagnostics(json, true)
}

fn scene_save_error_to_cli(error: SceneSaveError) -> CliError {
    match error {
        SceneSaveError::ValidationFailed { diagnostics } => CliError::Authoring { diagnostics },
        SceneSaveError::Json(error) => CliError::Json(error),
    }
}
