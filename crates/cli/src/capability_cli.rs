//! Registry-driven generic authoring CLI surface (ADR 0132).
//!
//! The CLI keeps its ergonomic per-domain commands, and adds one generic path
//! driven by the canonical authoring capability registry so headless clients can
//! discover semantic operations instead of hard-coding a command list.
//!
//! Each capability binds to one command path: every dot-separated capability
//! segment becomes one argument segment, so `scene.apply` runs the same code as
//! `engine-cli scene apply`. Generic invocation therefore adds no second
//! authoring implementation and cannot drift from the domain commands.

use super::{CliError, CliRunResult, to_json};
use engine_authoring::{
    AuthoringCapability, AuthoringCapabilityId, AuthoringCapabilityKind,
    AuthoringCapabilityRegistry, AuthoringCapabilitySummary, Diagnostic,
};
use serde::Serialize;

/// Generic operation requested from the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Inspect,
    Validate,
    Preview,
    Apply,
}

impl Verb {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "inspect" => Some(Self::Inspect),
            "validate" => Some(Self::Validate),
            "preview" => Some(Self::Preview),
            "apply" => Some(Self::Apply),
            _ => None,
        }
    }

    fn kind(self) -> AuthoringCapabilityKind {
        match self {
            Self::Inspect => AuthoringCapabilityKind::Query,
            Self::Validate => AuthoringCapabilityKind::Validation,
            Self::Preview => AuthoringCapabilityKind::PreviewMutation,
            Self::Apply => AuthoringCapabilityKind::CommittedMutation,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Validate => "validate",
            Self::Preview => "preview",
            Self::Apply => "apply",
        }
    }
}

#[derive(Debug, Serialize)]
struct CapabilitySummaryListOutput {
    capabilities: Vec<AuthoringCapabilitySummary>,
}

#[derive(Debug, Serialize)]
struct CapabilityListOutput<'a> {
    capabilities: Vec<&'a AuthoringCapability>,
}

#[derive(Debug, Serialize)]
struct CapabilityDescribeOutput<'a> {
    capability: &'a AuthoringCapability,
    command: Vec<&'a str>,
}

pub(super) fn dispatch(args: &[String]) -> Option<Result<CliRunResult, CliError>> {
    match args {
        [domain, command] if domain == "authoring" && command == "list" => Some(list()),
        [domain, command] if domain == "authoring" && command == "capabilities" => {
            Some(capabilities())
        }
        [domain, command, capability] if domain == "authoring" && command == "describe" => {
            Some(describe(capability))
        }
        [domain, verb, capability, rest @ ..] if domain == "authoring" => match Verb::parse(verb) {
            Some(verb) => Some(invoke(verb, capability, rest)),
            None => Some(Err(CliError::UnknownCommand {
                args: args.join(" "),
            })),
        },
        _ if args.first().is_some_and(|value| value == "authoring") => {
            Some(Err(CliError::UnknownCommand {
                args: args.join(" "),
            }))
        }
        _ => None,
    }
}

fn list() -> Result<CliRunResult, CliError> {
    let registry = AuthoringCapabilityRegistry::builtin();
    let output = CapabilitySummaryListOutput {
        capabilities: registry.summaries().collect(),
    };
    Ok(CliRunResult::success(to_json(&output)?))
}

fn capabilities() -> Result<CliRunResult, CliError> {
    let registry = AuthoringCapabilityRegistry::builtin();
    let output = CapabilityListOutput {
        capabilities: registry.capabilities().collect(),
    };
    Ok(CliRunResult::success(to_json(&output)?))
}

fn describe(capability: &str) -> Result<CliRunResult, CliError> {
    let registry = AuthoringCapabilityRegistry::builtin();
    let id = parse_capability_id(capability)?;
    let capability = registry.require(&id).map_err(capability_error)?;
    let output = CapabilityDescribeOutput {
        command: capability.id.segments().collect(),
        capability,
    };
    Ok(CliRunResult::success(to_json(&output)?))
}

fn invoke(verb: Verb, capability: &str, rest: &[String]) -> Result<CliRunResult, CliError> {
    let registry = AuthoringCapabilityRegistry::builtin();
    let id = parse_capability_id(capability)?;
    let capability = registry.require(&id).map_err(capability_error)?;

    if !capability.is_generic() {
        return Err(diagnostic_error(
            "cli.capability_not_generic",
            format!(
                "capability `{}` requires its specialized command",
                capability.id
            ),
        ));
    }
    if capability.kind != verb.kind() {
        return Err(diagnostic_error(
            "cli.capability_verb_mismatch",
            format!(
                "capability `{}` does not answer `authoring {}`",
                capability.id,
                verb.as_str()
            ),
        ));
    }
    // The registry reserves the generic namespace, so a resolved command path
    // can never dispatch back into `authoring`.
    if capability.id.is_reserved() {
        return Err(diagnostic_error(
            "cli.capability_reserved_namespace",
            format!("capability `{}` uses the reserved namespace", capability.id),
        ));
    }

    let mut command: Vec<String> = capability
        .id
        .segments()
        .map(|segment| segment.to_owned())
        .collect();
    command.extend(rest.iter().cloned());
    super::run_cli_with_status(command)
}

fn parse_capability_id(value: &str) -> Result<AuthoringCapabilityId, CliError> {
    AuthoringCapabilityId::try_new(value)
        .map_err(|error| diagnostic_error("cli.capability_invalid_id", error.to_string()))
}

fn capability_error(error: engine_authoring::AuthoringCapabilityError) -> CliError {
    diagnostic_error(error.code(), error.to_string())
}

fn diagnostic_error(code: &str, message: String) -> CliError {
    CliError::Authoring {
        diagnostics: vec![Diagnostic::error(code, message)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_cli_with_status;
    use engine_authoring::{
        AuthoringCommand, AuthoringScene, EntityId, PROJECT_SCHEMA_VERSION, ProjectConfig,
        ProjectRoot,
    };
    use serde_json::Value;
    use std::fs;
    use std::path::PathBuf;
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

    fn temp_project(commands: &[AuthoringCommand]) -> TempProject {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "gameengine_capability_cli_{}_{}",
            std::process::id(),
            sequence
        ));
        fs::create_dir_all(&root).expect("temporary project directory");
        let project = ProjectRoot::create(
            &root,
            ProjectConfig {
                name: "Capability CLI Test".into(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project creation");
        let scene_relative = "scenes/main.scene.json".to_owned();
        let scene_path = project
            .resolve_asset_for_write(&scene_relative)
            .expect("scene write path");
        fs::write(
            scene_path,
            AuthoringScene::new()
                .to_canonical_json()
                .expect("empty scene JSON"),
        )
        .expect("scene fixture");
        let project = TempProject {
            root,
            scene_relative,
        };
        fs::write(
            project.commands_path(),
            serde_json::to_string_pretty(commands).expect("command JSON"),
        )
        .expect("command fixture");
        project
    }

    fn diagnostic_code(error: &CliError) -> String {
        match error {
            CliError::Authoring { diagnostics } => diagnostics
                .first()
                .map(|diagnostic| diagnostic.code.clone())
                .unwrap_or_default(),
            other => panic!("expected an authoring diagnostic, got {other}"),
        }
    }

    #[test]
    fn list_command_reports_canonical_compact_summaries() {
        let result = run_cli_with_status(["authoring".to_owned(), "list".to_owned()])
            .expect("compact capability discovery");
        let json: Value = serde_json::from_str(&result.output).expect("JSON output");
        let expected = serde_json::to_value(CapabilitySummaryListOutput {
            capabilities: AuthoringCapabilityRegistry::builtin().summaries().collect(),
        })
        .expect("canonical compact summaries must serialize");

        assert_eq!(result.exit_code, 0);
        assert_eq!(json, expected);
        assert!(json["capabilities"].as_array().is_some_and(|items| {
            items.iter().all(|item| {
                item.get("id").is_some()
                    && item.get("domain").is_some()
                    && item.get("kind").is_some()
                    && item.get("exposure").is_some()
                    && item.get("description").is_some()
                    && item.get("input").is_none()
                    && item.get("output").is_none()
                    && item.get("permission").is_none()
                    && item.get("transaction").is_none()
            })
        }));
    }

    #[test]
    fn capabilities_command_reports_the_canonical_registry() {
        let result = run_cli_with_status(["authoring".to_owned(), "capabilities".to_owned()])
            .expect("capability discovery");
        let json: Value = serde_json::from_str(&result.output).expect("JSON output");
        let listed = json["capabilities"]
            .as_array()
            .expect("capability array")
            .iter()
            .map(|capability| capability["id"].as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        let expected = AuthoringCapabilityRegistry::builtin()
            .capabilities()
            .map(|capability| capability.id.as_str().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(result.exit_code, 0);
        assert_eq!(listed, expected);
    }

    #[test]
    fn describe_reports_the_bound_command_path() {
        let result = run_cli_with_status([
            "authoring".to_owned(),
            "describe".to_owned(),
            "graph.layout.apply".to_owned(),
        ])
        .expect("capability description");
        let json: Value = serde_json::from_str(&result.output).expect("JSON output");

        assert_eq!(
            json["command"],
            serde_json::json!(["graph", "layout", "apply"])
        );
        assert_eq!(json["capability"]["permission"], "project_data_write");
        assert_eq!(json["capability"]["transaction"], "atomic_commit");
    }

    #[test]
    fn generic_apply_matches_the_domain_command() {
        let entity = EntityId::generate();
        let commands = vec![AuthoringCommand::CreateEntity {
            id: entity.clone(),
            name: "created_generically".into(),
            parent: None,
        }];
        let generic = temp_project(&commands);
        let direct = temp_project(&commands);

        let generic_result = run_cli_with_status([
            "authoring".to_owned(),
            "apply".to_owned(),
            "scene.apply".to_owned(),
            generic.root.display().to_string(),
            generic.scene_relative.clone(),
            generic.commands_path().display().to_string(),
        ])
        .expect("generic scene apply");
        let direct_result = run_cli_with_status([
            "scene".to_owned(),
            "apply".to_owned(),
            direct.root.display().to_string(),
            direct.scene_relative.clone(),
            direct.commands_path().display().to_string(),
        ])
        .expect("domain scene apply");

        assert_eq!(generic_result.exit_code, 0, "{}", generic_result.output);
        assert_eq!(generic_result.exit_code, direct_result.exit_code);
        // Scene revisions are process-monotonic, so equivalence is asserted on
        // the semantic mutation result rather than on the revision tokens.
        assert_eq!(
            mutation_semantics(&generic_result.output),
            mutation_semantics(&direct_result.output)
        );
        assert_eq!(
            scene_entities(&generic.scene_path()),
            scene_entities(&direct.scene_path())
        );
        assert!(
            scene_entities(&generic.scene_path())
                .iter()
                .any(|value| value["id"] == serde_json::json!(entity.as_str()))
        );
    }

    fn mutation_semantics(output: &str) -> Value {
        let json: Value = serde_json::from_str(output).expect("JSON output");
        serde_json::json!({
            "success": json["success"],
            "diagnostics": json["diagnostics"],
            "diff": json["diff"],
        })
    }

    fn scene_entities(path: &PathBuf) -> Vec<Value> {
        let scene: Value =
            serde_json::from_str(&fs::read_to_string(path).expect("persisted scene"))
                .expect("scene JSON");
        scene["entities"].as_array().cloned().unwrap_or_default()
    }

    #[test]
    fn every_generic_capability_has_a_documented_command_path() {
        let help = crate::help_text();

        for capability in AuthoringCapabilityRegistry::builtin()
            .capabilities()
            .filter(|capability| capability.is_generic())
        {
            let command = capability.id.segments().collect::<Vec<_>>().join(" ");
            assert!(
                help.contains(&format!("engine-cli {command} "))
                    || help.contains(&format!("engine-cli {command}\n")),
                "generic capability `{}` has no headless command path",
                capability.id
            );
        }
    }

    #[test]
    fn specialized_capabilities_are_not_callable_generically() {
        let error = run_cli_with_status([
            "authoring".to_owned(),
            "apply".to_owned(),
            "behavior_tree.apply".to_owned(),
            "graph.json".to_owned(),
            "commands.json".to_owned(),
        ])
        .expect_err("specialized capabilities need their declared command");

        assert_eq!(diagnostic_code(&error), "cli.capability_not_generic");
    }

    #[test]
    fn generic_verb_must_match_the_capability_kind() {
        let error = run_cli_with_status([
            "authoring".to_owned(),
            "apply".to_owned(),
            "scene.inspect".to_owned(),
        ])
        .expect_err("a query capability cannot be applied");

        assert_eq!(diagnostic_code(&error), "cli.capability_verb_mismatch");
    }

    #[test]
    fn unknown_capability_uses_the_registry_error_code() {
        let error = run_cli_with_status([
            "authoring".to_owned(),
            "inspect".to_owned(),
            "scene.does_not_exist".to_owned(),
        ])
        .expect_err("unknown capabilities must be rejected");

        assert_eq!(diagnostic_code(&error), "authoring.capability_unknown");
    }

    #[test]
    fn unknown_generic_verb_reports_usage() {
        let error = run_cli_with_status([
            "authoring".to_owned(),
            "compile".to_owned(),
            "scene.apply".to_owned(),
        ])
        .expect_err("only the generic verbs are supported");

        assert!(matches!(error, CliError::UnknownCommand { .. }));
    }
}
