//! Thin file-oriented CLI adapter for VFX authoring.

use super::{to_json, CliError, CliRunResult};
use engine_authoring::{
    replace_file_contents, Diagnostic, VfxAuthoringService, VfxCommand, VfxEffect, VfxTemplate,
};
use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Serialize)]
struct VfxInspectOutput {
    success: bool,
    effect: VfxEffect,
    validation: engine_authoring::VfxValidation,
    compilation: engine_authoring::VfxCompilation,
}

pub(super) fn dispatch(args: &[String]) -> Option<Result<CliRunResult, CliError>> {
    match args {
        [domain, command] if domain == "vfx" && command == "schemas" => Some(
            to_json(&VfxAuthoringService::new().schemas()).map(CliRunResult::success),
        ),
        [domain, command, path] if domain == "vfx" && command == "inspect" => {
            Some(inspect(Path::new(path)))
        }
        [domain, command, path] if domain == "vfx" && command == "validate" => {
            Some(validate(Path::new(path)))
        }
        [domain, command, path, commands_path] if domain == "vfx" && command == "preview" => {
            Some(mutate(Path::new(path), Path::new(commands_path), false))
        }
        [domain, command, path, commands_path] if domain == "vfx" && command == "apply" => {
            Some(mutate(Path::new(path), Path::new(commands_path), true))
        }
        [domain, command, template, path] if domain == "vfx" && command == "create" => {
            Some(create(template, Path::new(path)))
        }
        _ if args.first().is_some_and(|value| value == "vfx") => {
            Some(Err(CliError::UnknownCommand {
                args: args.join(" "),
            }))
        }
        _ => None,
    }
}

fn inspect(path: &Path) -> Result<CliRunResult, CliError> {
    let effect = load_effect(path)?;
    let service = VfxAuthoringService::new();
    let validation = service.validate(&effect);
    let success = validation.success;
    let output = VfxInspectOutput {
        success,
        compilation: service.compile(&effect),
        validation,
        effect,
    };
    Ok(CliRunResult::diagnostics(to_json(&output)?, !success))
}

fn validate(path: &Path) -> Result<CliRunResult, CliError> {
    let effect = load_effect(path)?;
    let output = VfxAuthoringService::new().validate(&effect);
    Ok(CliRunResult::diagnostics(to_json(&output)?, !output.success))
}

fn mutate(path: &Path, commands_path: &Path, persist: bool) -> Result<CliRunResult, CliError> {
    let effect = load_effect(path)?;
    let commands: Vec<VfxCommand> = load_json(commands_path)?;
    let service = VfxAuthoringService::new();
    let output = service.apply(&effect, &commands);
    if persist && output.success {
        let committed = output.effect.as_ref().ok_or_else(|| CliError::Authoring {
            diagnostics: vec![Diagnostic::error(
                "vfx.adapter_missing_commit",
                "successful VFX apply returned no committed document",
            )],
        })?;
        let json = service
            .effect_to_canonical_json(committed)
            .map_err(vfx_error)?;
        replace_file_contents(path, &json).map_err(|source| CliError::Persist { source })?;
    }
    Ok(CliRunResult::diagnostics(to_json(&output)?, !output.success))
}

fn create(template: &str, path: &Path) -> Result<CliRunResult, CliError> {
    let template = parse_template(template).ok_or_else(|| CliError::Authoring {
        diagnostics: vec![Diagnostic::error(
            "vfx.unknown_template",
            format!("unknown VFX template `{template}`; expected spark, smoke, burst, or trail"),
        )],
    })?;
    let service = VfxAuthoringService::new();
    let effect = service.template(template);
    let json = service.effect_to_canonical_json(&effect).map_err(vfx_error)?;
    replace_file_contents(path, &json).map_err(|source| CliError::Persist { source })?;
    Ok(CliRunResult::success(to_json(&effect)?))
}

fn load_effect(path: &Path) -> Result<VfxEffect, CliError> {
    let json = fs::read_to_string(path).map_err(|source| CliError::Io {
        path: path.to_owned(),
        source,
    })?;
    VfxAuthoringService::new()
        .effect_from_json(&json)
        .map_err(vfx_error)
}

fn load_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CliError> {
    let json = fs::read_to_string(path).map_err(|source| CliError::Io {
        path: path.to_owned(),
        source,
    })?;
    serde_json::from_str(&json).map_err(|source| CliError::InvalidJson {
        path: path.to_owned(),
        source,
    })
}

fn parse_template(value: &str) -> Option<VfxTemplate> {
    match value {
        "spark" => Some(VfxTemplate::Spark),
        "smoke" => Some(VfxTemplate::Smoke),
        "burst" => Some(VfxTemplate::Burst),
        "trail" => Some(VfxTemplate::Trail),
        _ => None,
    }
}

fn vfx_error(error: impl std::fmt::Display) -> CliError {
    CliError::Authoring {
        diagnostics: vec![Diagnostic::error("vfx.adapter_error", error.to_string())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::VfxCommand;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn preview_and_apply_share_transaction_semantics() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("engine_cli_vfx_{stamp}"));
        fs::create_dir_all(&temp).expect("create temp directory");
        let effect_path = temp.join("effect.vfx.json");
        let commands_path = temp.join("commands.json");
        let service = VfxAuthoringService::new();
        let effect = service.template(VfxTemplate::Spark);
        fs::write(
            &effect_path,
            service
                .effect_to_canonical_json(&effect)
                .expect("fixture serializes"),
        )
        .expect("write effect");
        fs::write(
            &commands_path,
            serde_json::to_string(&vec![VfxCommand::SetEffectSeed { seed: 77 }])
                .expect("commands serialize"),
        )
        .expect("write commands");

        let preview = mutate(&effect_path, &commands_path, false).expect("preview succeeds");
        let after_preview = fs::read_to_string(&effect_path).expect("effect remains readable");
        assert_eq!(preview.exit_code, 0);
        assert_eq!(service.effect_from_json(&after_preview).expect("effect parses").seed, effect.seed);

        let applied = mutate(&effect_path, &commands_path, true).expect("apply succeeds");
        assert_eq!(applied.exit_code, 0);
        let committed = service
            .effect_from_json(&fs::read_to_string(&effect_path).expect("effect readable"))
            .expect("effect parses");
        assert_eq!(committed.seed, 77);
        let _ = fs::remove_dir_all(temp);
    }
}
