//! Thin file-oriented CLI adapter for ADR 0121 typed-document authoring.

use super::{to_json, CliError, CliRunResult};
use engine_authoring::{
    replace_file_contents, AnimationSet, AuthoringPermission, AuthoringPermissions, Diagnostic,
    MaterialAsset, ProjectRoot, ProjectSettings, TypedAuthoringDocument,
    TypedDocumentAuthoringError, TypedDocumentAuthoringService, TypedDocumentAuthoringState,
};
use serde::de::DeserializeOwned;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn dispatch(args: &[String]) -> Option<Result<CliRunResult, CliError>> {
    match args {
        [domain, command, project, relative]
            if domain == "material" && command == "inspect" =>
        {
            Some(material_inspect(Path::new(project), relative))
        }
        [domain, command, project, relative]
            if domain == "material" && command == "validate" =>
        {
            Some(material_validate(Path::new(project), relative))
        }
        [domain, command, project, relative, replacement]
            if domain == "material" && (command == "preview" || command == "apply") =>
        {
            Some(material_mutate(
                Path::new(project),
                relative,
                Path::new(replacement),
                command == "apply",
            ))
        }
        [domain, command, project]
            if domain == "project_settings" && command == "inspect" =>
        {
            Some(project_settings_inspect(Path::new(project)))
        }
        [domain, command, project]
            if domain == "project_settings" && command == "validate" =>
        {
            Some(project_settings_validate(Path::new(project)))
        }
        [domain, command, project, replacement]
            if domain == "project_settings"
                && (command == "preview" || command == "apply") =>
        {
            Some(project_settings_mutate(
                Path::new(project),
                Path::new(replacement),
                command == "apply",
            ))
        }
        [domain, command, project, relative]
            if domain == "animation_set" && command == "inspect" =>
        {
            Some(animation_set_inspect(Path::new(project), relative))
        }
        [domain, command, project, relative]
            if domain == "animation_set" && command == "validate" =>
        {
            Some(animation_set_validate(Path::new(project), relative))
        }
        [domain, command, project, relative, replacement]
            if domain == "animation_set"
                && (command == "preview" || command == "apply") =>
        {
            Some(animation_set_mutate(
                Path::new(project),
                relative,
                Path::new(replacement),
                command == "apply",
            ))
        }
        _ if args.first().is_some_and(|value| {
            value == "material" || value == "project_settings" || value == "animation_set"
        }) => Some(Err(CliError::UnknownCommand {
            args: args.join(" "),
        })),
        _ => None,
    }
}

fn material_inspect(project: &Path, relative: &str) -> Result<CliRunResult, CliError> {
    let (_, document) = load_material(project, relative)?;
    inspect(document)
}

fn material_validate(project: &Path, relative: &str) -> Result<CliRunResult, CliError> {
    let (_, document) = load_material(project, relative)?;
    validate(document)
}

fn material_mutate(
    project: &Path,
    relative: &str,
    replacement_path: &Path,
    persist: bool,
) -> Result<CliRunResult, CliError> {
    let (path, document) = load_material(project, relative)?;
    let replacement: MaterialAsset = load_json(replacement_path)?;
    mutate(document, replacement, persist, |document| {
        let json = document.to_json().map_err(CliError::Json)?;
        replace_file_contents(&path, &json).map_err(|source| CliError::Persist { source })
    })
}

fn project_settings_inspect(project: &Path) -> Result<CliRunResult, CliError> {
    let (_, document) = load_project_settings(project)?;
    inspect(document)
}

fn project_settings_validate(project: &Path) -> Result<CliRunResult, CliError> {
    let (_, document) = load_project_settings(project)?;
    validate(document)
}

fn project_settings_mutate(
    project: &Path,
    replacement_path: &Path,
    persist: bool,
) -> Result<CliRunResult, CliError> {
    let (root, document) = load_project_settings(project)?;
    let replacement: ProjectSettings = load_json(replacement_path)?;
    mutate(document, replacement, persist, |document| {
        document.save(root.path()).map_err(|error| authoring_message(
            "project_settings.save_failed",
            error.to_string(),
        ))
    })
}

fn animation_set_inspect(project: &Path, relative: &str) -> Result<CliRunResult, CliError> {
    let (_, document) = load_animation_set(project, relative)?;
    inspect(document)
}

fn animation_set_validate(project: &Path, relative: &str) -> Result<CliRunResult, CliError> {
    let (_, document) = load_animation_set(project, relative)?;
    validate(document)
}

fn animation_set_mutate(
    project: &Path,
    relative: &str,
    replacement_path: &Path,
    persist: bool,
) -> Result<CliRunResult, CliError> {
    let (path, document) = load_animation_set(project, relative)?;
    let replacement: AnimationSet = load_json(replacement_path)?;
    mutate(document, replacement, persist, |document| {
        let json = document
            .to_canonical_json()
            .map_err(|error| authoring_message("animation_set.invalid", error.to_string()))?;
        replace_file_contents(&path, &json).map_err(|source| CliError::Persist { source })
    })
}

fn inspect<T: TypedAuthoringDocument>(document: T) -> Result<CliRunResult, CliError> {
    let state = TypedDocumentAuthoringState::new();
    let output = TypedDocumentAuthoringService::new()
        .inspect(&document, &state, &read_permissions())
        .map_err(authoring_error)?;
    Ok(CliRunResult::success(to_json(&output)?))
}

fn validate<T: TypedAuthoringDocument>(document: T) -> Result<CliRunResult, CliError> {
    let state = TypedDocumentAuthoringState::new();
    let output = TypedDocumentAuthoringService::new()
        .validate(&document, &state, &read_permissions())
        .map_err(authoring_error)?;
    Ok(CliRunResult::diagnostics(to_json(&output)?, !output.success))
}

fn mutate<T, F>(
    mut document: T,
    replacement: T,
    persist: bool,
    persist_document: F,
) -> Result<CliRunResult, CliError>
where
    T: TypedAuthoringDocument,
    F: FnOnce(&T) -> Result<(), CliError>,
{
    let service = TypedDocumentAuthoringService::new();
    let permissions = writable_permissions();
    let mut state = TypedDocumentAuthoringState::new();
    let base = service
        .inspect(&document, &state, &permissions)
        .map_err(authoring_error)?;
    let output = if persist {
        service.apply(
            &mut document,
            &mut state,
            &permissions,
            base.revision,
            base.generation,
            replacement,
        )
    } else {
        service.preview(
            &document,
            &state,
            &permissions,
            base.revision,
            base.generation,
            replacement,
        )
    }
    .map_err(authoring_error)?;
    if persist && output.success && !output.diff.is_empty() {
        persist_document(&document)?;
    }
    Ok(CliRunResult::diagnostics(to_json(&output)?, !output.success))
}

fn load_material(project: &Path, relative: &str) -> Result<(PathBuf, MaterialAsset), CliError> {
    let root = open_project(project)?;
    let path = root
        .resolve_asset(relative)
        .map_err(|error| authoring_message("project.invalid_asset_path", error.to_string()))?;
    let json = read_text(&path)?;
    let document = MaterialAsset::from_json(&json)
        .map_err(|error| authoring_message("material.invalid", error.to_string()))?;
    Ok((path, document))
}

fn load_animation_set(
    project: &Path,
    relative: &str,
) -> Result<(PathBuf, AnimationSet), CliError> {
    let root = open_project(project)?;
    let path = root
        .resolve_asset(relative)
        .map_err(|error| authoring_message("project.invalid_asset_path", error.to_string()))?;
    let json = read_text(&path)?;
    let document = AnimationSet::from_json(&json)
        .map_err(|error| authoring_message("animation_set.invalid", error.to_string()))?;
    Ok((path, document))
}

fn load_project_settings(project: &Path) -> Result<(ProjectRoot, ProjectSettings), CliError> {
    let root = open_project(project)?;
    let document = ProjectSettings::load(root.path()).map_err(|error| {
        authoring_message("project_settings.load_failed", error.to_string())
    })?;
    Ok((root, document))
}

fn open_project(path: &Path) -> Result<ProjectRoot, CliError> {
    ProjectRoot::open(path)
        .map_err(|error| authoring_message("project.open_failed", error.to_string()))
}

fn read_text(path: &Path) -> Result<String, CliError> {
    fs::read_to_string(path).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn load_json<T: DeserializeOwned>(path: &Path) -> Result<T, CliError> {
    let json = read_text(path)?;
    serde_json::from_str(&json).map_err(|source| CliError::InvalidJson {
        path: path.to_path_buf(),
        source,
    })
}

fn authoring_error(error: TypedDocumentAuthoringError) -> CliError {
    authoring_message(error.code(), error.to_string())
}

fn authoring_message(code: &str, message: String) -> CliError {
    CliError::Authoring {
        diagnostics: vec![Diagnostic::error(code, message)],
    }
}

fn read_permissions() -> AuthoringPermissions {
    AuthoringPermissions::read_only()
}

fn writable_permissions() -> AuthoringPermissions {
    AuthoringPermissions::read_only()
        .with(AuthoringPermission::Preview)
        .with(AuthoringPermission::ProjectDataWrite)
}
