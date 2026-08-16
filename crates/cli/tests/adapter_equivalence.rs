//! Adapter-to-adapter equivalence tests for specialized authoring operations.
//!
//! ADR 0132 section 5 requires a specialized adapter operation to have
//! adapter-equivalence tests when another adapter exposes the same intent.
//! Proving that each adapter calls the shared service is weaker than this: it
//! leaves argument mapping, defaulting, and result shaping unchecked.
//!
//! Each test therefore builds one fixture, runs the same intent through the CLI
//! and through the MCP handler, and compares the semantic results directly.
//! Generic Scene, Graph, GraphView, and UI parity is covered by the Editor
//! parity tests, which additionally compare the live Editor session.

use engine_assets::asset::{AssetManifest, ImportSettings, ManifestEntry};
use engine_authoring::{
    load_scene_from_json, AuthoringCommand, AuthoringPermission, AuthoringPermissions,
    AuthoringScene, AuthoringSession, BehaviorTreeAuthoringService, BehaviorTreeDomain, EdgeId,
    EntityId, Graph, GraphCommand, GraphId, NodeId, ProjectConfig, ProjectRoot,
    SceneAuthoringService, Transaction, VfxAuthoringService, VfxCommand, VfxTemplate,
    PROJECT_SCHEMA_VERSION,
};
use engine_cli::run_cli_with_status;
use engine_mcp::{
    AssetInspectInput, AssetMcpTools, AssetSearchInput, BehaviorTreeApplyInput,
    BehaviorTreeGraphInput, BehaviorTreeMcpTools, PrefabCreateInput, PrefabInstantiateInput,
    PrefabMcpTools, VfxEffectInput, VfxMcpTools, VfxMutationInput, VfxTemplateInput,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const SCENE_RELATIVE: &str = "scenes/main.scene.json";
const PREFAB_RELATIVE: &str = "prefabs/hero.prefab.json";

/// Temporary project directory removed when the test ends.
struct TempProject {
    root: PathBuf,
}

impl TempProject {
    fn scene_path(&self) -> PathBuf {
        self.root.join("assets").join(SCENE_RELATIVE)
    }

    fn argument(&self) -> String {
        self.root.display().to_string()
    }

    fn open(&self) -> ProjectRoot {
        ProjectRoot::open(&self.root).expect("fixture project opens")
    }

    fn scene_session(&self) -> AuthoringSession {
        let json = fs::read_to_string(self.scene_path()).expect("fixture scene readable");
        AuthoringSession::new(load_scene_from_json(&json).expect("fixture scene parses"))
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn temp_directory(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "gameengine_equivalence_{label}_{}_{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("temporary directory");
    path
}

/// Creates a project whose Scene and manifest content is byte-identical for
/// every caller, so both adapters observe exactly the same fixture.
fn project_fixture(label: &str, scene_json: &str, manifest: &AssetManifest) -> TempProject {
    let root = temp_directory(label);
    let project = ProjectRoot::create(
        &root,
        ProjectConfig {
            name: "AdapterEquivalence".into(),
            schema_version: PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project fixture");
    // `resolve_asset_for_write` canonicalizes the parent directory, so prefab
    // destinations need their folder before either adapter runs.
    fs::create_dir_all(project.assets_root().join("prefabs")).expect("prefab folder");
    let scene_path = project
        .resolve_asset_for_write(SCENE_RELATIVE)
        .expect("scene write path");
    fs::write(scene_path, scene_json).expect("scene fixture");
    fs::write(
        project.path().join("asset_manifest.json"),
        manifest.to_canonical_json().expect("manifest serializes"),
    )
    .expect("manifest fixture");
    TempProject { root }
}

fn writable() -> AuthoringPermissions {
    AuthoringPermissions::read_only()
        .with(AuthoringPermission::Preview)
        .with(AuthoringPermission::ProjectDataWrite)
        .with(AuthoringPermission::AssetWrite)
}

fn cli(args: [&str; 4]) -> Value {
    cli_result(args.iter().map(|value| (*value).to_owned()).collect())
}

fn cli_result(args: Vec<String>) -> Value {
    let result = run_cli_with_status(args.clone())
        .unwrap_or_else(|error| panic!("CLI `{}` failed: {error}", args.join(" ")));
    assert_eq!(result.exit_code, 0, "CLI reported failure: {}", result.output);
    serde_json::from_str(&result.output).expect("CLI output is JSON")
}

/// Converts an MCP handler result the way a transport does: serialize to JSON
/// text, then parse it back.
///
/// `serde_json::to_value` would widen `f32` fields to their exact `f64`
/// expansion, which no JSON client ever observes and which the CLI's own
/// string output does not contain.
fn value<T: Serialize>(output: T) -> Value {
    let json = serde_json::to_string(&output).expect("MCP output serializes");
    serde_json::from_str(&json).expect("MCP output is JSON")
}

/// Removes identity and bookkeeping that is generated per run rather than
/// derived from the fixture, so two adapters can be compared for semantic
/// equality.
///
/// Generated stable IDs are replaced by their first-appearance position, which
/// keeps structure and cross-references observable while ignoring the exact
/// ULID each run produced.
fn semantic(value: Value) -> Value {
    let mut identities = Vec::new();
    normalize(value, &mut identities)
}

fn normalize(value: Value, identities: &mut Vec<String>) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .filter(|(key, _)| !is_run_scoped_field(key))
                .map(|(key, field)| (key, normalize(field, identities)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| normalize(item, identities))
                .collect(),
        ),
        Value::String(text) => Value::String(normalize_identity(text, identities)),
        other => other,
    }
}

/// Fields whose value depends on when or in which process the adapter ran.
///
/// Scene revisions are process-monotonic and diagnostics carry a wall-clock
/// stamp, so neither can be equal across two adapter runs even when the
/// authoring result is identical.
fn is_run_scoped_field(key: &str) -> bool {
    matches!(
        key,
        "revision" | "generation" | "base_revision" | "base_generation" | "timestamp_ms"
    )
}

fn normalize_identity(text: String, identities: &mut Vec<String>) -> String {
    let Some((prefix, suffix)) = text.split_once('_') else {
        return text;
    };
    let is_stable_id = suffix.len() == 26
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit());
    if !is_stable_id {
        return text;
    }
    let prefix = prefix.to_owned();
    let position = identities
        .iter()
        .position(|known| known == &text)
        .unwrap_or_else(|| {
            identities.push(text);
            identities.len() - 1
        });
    format!("{prefix}_#{position}")
}

/// Replaces one fixture project's absolute path with a placeholder.
///
/// Prefab instantiation records the resolved prefab source on the instance
/// component, so two adapters running against two temporary projects report
/// different absolute paths for the same authoring result.
fn without_project_path(value: Value, project: &TempProject) -> Value {
    let mut text = serde_json::to_string(&value).expect("value serializes");
    let canonical = project
        .root
        .canonicalize()
        .unwrap_or_else(|_| project.root.clone());
    for variant in [canonical.display().to_string(), project.argument()] {
        // Windows separators are escaped once inside JSON text.
        text = text.replace(&variant.replace('\\', "\\\\"), "<project>");
        text = text.replace(&variant, "<project>");
    }
    serde_json::from_str(&text).expect("normalized value is JSON")
}

/// Summarizes a committed Scene by hierarchy path and components.
///
/// Canonical Scene JSON orders entities by stable ID, and prefab instantiation
/// mints a fresh ID per entity, so array position carries no meaning across two
/// runs. Comparing name paths and components compares what the adapters
/// actually authored.
fn scene_shape(scene: &Value) -> Value {
    let entities = scene["entities"].as_array().cloned().unwrap_or_default();
    let by_id: BTreeMap<String, Value> = entities
        .iter()
        .map(|entity| {
            (
                entity["id"].as_str().unwrap_or_default().to_owned(),
                entity.clone(),
            )
        })
        .collect();
    let mut rows = entities
        .iter()
        .map(|entity| {
            let mut path = Vec::new();
            let mut cursor = Some(entity.clone());
            // The fixture hierarchy is a tree; the bound only guards against a
            // malformed parent cycle turning a failure into a hang.
            while let Some(current) = cursor.filter(|_| path.len() <= entities.len()) {
                path.push(current["name"].as_str().unwrap_or_default().to_owned());
                cursor = current["parent"]
                    .as_str()
                    .and_then(|parent| by_id.get(parent).cloned());
            }
            path.reverse();
            serde_json::json!({
                "path": path.join("/"),
                "components": entity["components"].clone(),
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row.to_string());
    Value::Array(rows)
}

fn scene_with_hierarchy() -> (String, EntityId, EntityId) {
    let mut scene = AuthoringScene::new();
    let root = EntityId::generate();
    let child = EntityId::generate();
    let mut transaction = Transaction::begin(&scene);
    transaction.apply(AuthoringCommand::CreateEntity {
        id: root.clone(),
        name: "hero".into(),
        parent: None,
    });
    transaction.apply(AuthoringCommand::CreateEntity {
        id: child.clone(),
        name: "hero_weapon".into(),
        parent: Some(root.clone()),
    });
    transaction
        .commit(&mut scene)
        .expect("fixture scene commits");
    (
        scene.to_canonical_json().expect("fixture scene serializes"),
        root,
        child,
    )
}

fn manifest_with_texture() -> (AssetManifest, engine_authoring::AssetId) {
    let mut manifest = AssetManifest::default();
    let id = engine_authoring::AssetId::generate();
    manifest.insert(
        id.clone(),
        ManifestEntry {
            path: "textures/hero.png".into(),
            name: Some("hero_texture".into()),
            import_settings: ImportSettings::default(),
        },
    );
    (manifest, id)
}

fn behavior_tree_fixture(domain: &BehaviorTreeDomain) -> (Graph, NodeId) {
    let mut graph = Graph::new(
        GraphId::generate(),
        domain.graph_kind().clone(),
        "equivalence_tree",
    );
    let root = NodeId::generate();
    let sequence = NodeId::generate();
    let action = NodeId::generate();
    graph
        .nodes
        .insert(root.clone(), domain.root_node(root.clone()));
    graph
        .nodes
        .insert(sequence.clone(), domain.sequence_node(sequence.clone()));
    graph
        .nodes
        .insert(action.clone(), domain.action_node(action.clone(), "idle"));
    let root_edge = domain.child_edge(EdgeId::generate(), root, sequence.clone(), 0);
    graph.edges.insert(root_edge.id.clone(), root_edge);
    let action_edge = domain.child_edge(EdgeId::generate(), sequence.clone(), action, 0);
    graph.edges.insert(action_edge.id.clone(), action_edge);
    (graph, sequence)
}

/// Returns an independent copy of a fixture graph.
///
/// `Graph` is deliberately not `Clone`, and every MCP handler takes the graph
/// by value, so each call reloads it from the same canonical fixture JSON.
fn reload_graph(json: &str) -> Graph {
    serde_json::from_str(json).expect("fixture graph parses")
}

fn write_json<T: Serialize>(path: &Path, value: &T) {
    fs::write(
        path,
        serde_json::to_string_pretty(value).expect("fixture serializes"),
    )
    .expect("fixture written");
}

#[test]
fn vfx_apply_is_equivalent_across_cli_and_mcp() {
    let service = VfxAuthoringService::new();
    let effect = service.template(VfxTemplate::Spark);
    let commands = vec![VfxCommand::SetEffectSeed { seed: 77 }];
    let directory = temp_directory("vfx_apply");
    let effect_path = directory.join("effect.vfx.json");
    let commands_path = directory.join("commands.json");
    fs::write(
        &effect_path,
        service
            .effect_to_canonical_json(&effect)
            .expect("fixture serializes"),
    )
    .expect("effect fixture");
    write_json(&commands_path, &commands);

    let through_cli = cli([
        "vfx",
        "apply",
        &effect_path.display().to_string(),
        &commands_path.display().to_string(),
    ]);
    let through_mcp = value(
        VfxMcpTools::new()
            .apply(
                &writable(),
                VfxMutationInput {
                    effect: effect.clone(),
                    commands,
                },
            )
            .expect("MCP apply"),
    );

    assert_eq!(semantic(through_cli.clone()), semantic(through_mcp.clone()));
    // Persistence is part of the CLI operation, so the committed document must
    // match the document MCP hands back to its host for saving.
    let persisted = fs::read_to_string(&effect_path).expect("committed effect");
    let committed = service
        .effect_to_canonical_json(
            &serde_json::from_value(through_mcp["effect"].clone()).expect("committed MCP effect"),
        )
        .expect("committed MCP effect serializes");
    assert_eq!(persisted, committed);
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn vfx_read_operations_are_equivalent_across_cli_and_mcp() {
    let service = VfxAuthoringService::new();
    let effect = service.template(VfxTemplate::Burst);
    let directory = temp_directory("vfx_read");
    let effect_path = directory.join("effect.vfx.json");
    fs::write(
        &effect_path,
        service
            .effect_to_canonical_json(&effect)
            .expect("fixture serializes"),
    )
    .expect("effect fixture");
    let path = effect_path.display().to_string();
    let tools = VfxMcpTools::new();
    let permissions = AuthoringPermissions::read_only();

    let cli_schemas = cli_result(vec!["vfx".to_owned(), "schemas".to_owned()]);
    let cli_validate = cli_result(vec!["vfx".to_owned(), "validate".to_owned(), path.clone()]);
    let cli_inspect = cli_result(vec!["vfx".to_owned(), "inspect".to_owned(), path]);

    assert_eq!(
        cli_schemas,
        value(tools.schemas(&permissions).expect("MCP schemas"))
    );
    assert_eq!(
        cli_validate,
        value(
            tools
                .validate(
                    &permissions,
                    VfxEffectInput {
                        effect: effect.clone()
                    }
                )
                .expect("MCP validate")
        )
    );
    assert_eq!(
        cli_inspect,
        value(
            tools
                .inspect(&permissions, VfxEffectInput { effect })
                .expect("MCP inspect")
        )
    );
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn vfx_template_creation_is_equivalent_across_cli_and_mcp() {
    let directory = temp_directory("vfx_template");
    let created_path = directory.join("created.vfx.json");

    let through_cli = cli([
        "vfx",
        "create",
        "smoke",
        &created_path.display().to_string(),
    ]);
    let through_mcp = value(
        VfxMcpTools::new()
            .template(
                &AuthoringPermissions::read_only(),
                VfxTemplateInput {
                    template: VfxTemplate::Smoke,
                },
            )
            .expect("MCP template"),
    );

    assert_eq!(semantic(through_cli.clone()), semantic(through_mcp));
    // Each template instantiation mints fresh VFX IDs, so persistence is proven
    // against the document this CLI run returned rather than the MCP run's.
    let persisted: Value = serde_json::from_str(
        &fs::read_to_string(&created_path).expect("created effect"),
    )
    .expect("created effect is JSON");
    assert_eq!(semantic(persisted), semantic(through_cli));
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn asset_queries_are_equivalent_across_cli_and_mcp() {
    let (scene_json, _root, _child) = scene_with_hierarchy();
    let (manifest, asset_id) = manifest_with_texture();
    let project = project_fixture("asset", &scene_json, &manifest);
    let tools = AssetMcpTools::new();
    let permissions = AuthoringPermissions::read_only();
    let root = project.open();

    let cli_search = cli(["asset", "search", &project.argument(), "hero"]);
    let cli_inspect = cli([
        "asset",
        "inspect",
        &project.argument(),
        asset_id.as_str(),
    ]);

    assert_eq!(
        semantic(cli_search),
        semantic(value(
            tools
                .asset_search(
                    &root,
                    &manifest,
                    &permissions,
                    AssetSearchInput {
                        query: "hero".into()
                    },
                )
                .expect("MCP asset search")
        ))
    );
    assert_eq!(
        semantic(cli_inspect),
        semantic(value(
            tools
                .asset_inspect(
                    &root,
                    &manifest,
                    &permissions,
                    AssetInspectInput { asset_id },
                )
                .expect("MCP asset inspect")
        ))
    );
}

#[test]
fn prefab_create_is_equivalent_across_cli_and_mcp() {
    let (scene_json, root_entity, _child) = scene_with_hierarchy();
    let (manifest, _asset_id) = manifest_with_texture();
    let cli_project = project_fixture("prefab_create_cli", &scene_json, &manifest);
    let mcp_project = project_fixture("prefab_create_mcp", &scene_json, &manifest);

    let through_cli = cli_result(vec![
        "prefab".to_owned(),
        "create".to_owned(),
        cli_project.argument(),
        SCENE_RELATIVE.to_owned(),
        root_entity.as_str().to_owned(),
        PREFAB_RELATIVE.to_owned(),
    ]);
    let mut mcp_manifest = manifest.clone();
    let through_mcp = value(
        PrefabMcpTools::new()
            .prefab_create(
                &mcp_project.open(),
                &mut mcp_manifest,
                &writable(),
                mcp_project.scene_session().scene(),
                PrefabCreateInput {
                    root_entity,
                    destination: PREFAB_RELATIVE.into(),
                },
            )
            .expect("MCP prefab create"),
    );

    assert_eq!(semantic(through_cli), semantic(through_mcp));
    let cli_document: Value = serde_json::from_str(
        &fs::read_to_string(cli_project.root.join("assets").join(PREFAB_RELATIVE))
            .expect("CLI prefab document"),
    )
    .expect("CLI prefab JSON");
    let mcp_document: Value = serde_json::from_str(
        &fs::read_to_string(mcp_project.root.join("assets").join(PREFAB_RELATIVE))
            .expect("MCP prefab document"),
    )
    .expect("MCP prefab JSON");
    assert_eq!(semantic(cli_document), semantic(mcp_document));
}

#[test]
fn prefab_instantiate_is_equivalent_across_cli_and_mcp() {
    let (scene_json, root_entity, _child) = scene_with_hierarchy();
    let (manifest, _asset_id) = manifest_with_texture();
    let cli_project = project_fixture("prefab_instantiate_cli", &scene_json, &manifest);
    let mcp_project = project_fixture("prefab_instantiate_mcp", &scene_json, &manifest);
    for project in [&cli_project, &mcp_project] {
        let result = run_cli_with_status([
            "prefab".to_owned(),
            "create".to_owned(),
            project.argument(),
            SCENE_RELATIVE.to_owned(),
            root_entity.as_str().to_owned(),
            PREFAB_RELATIVE.to_owned(),
        ])
        .expect("prefab fixture");
        assert_eq!(result.exit_code, 0, "{}", result.output);
    }

    let through_cli = cli_result(vec![
        "prefab".to_owned(),
        "instantiate".to_owned(),
        cli_project.argument(),
        SCENE_RELATIVE.to_owned(),
        PREFAB_RELATIVE.to_owned(),
    ]);

    let mut session = mcp_project.scene_session();
    let permissions = writable();
    let base = SceneAuthoringService::new()
        .inspect(&session, &permissions)
        .expect("MCP scene base");
    let mutation = PrefabMcpTools::new()
        .prefab_instantiate(
            &mcp_project.open(),
            &mut session,
            &permissions,
            PrefabInstantiateInput {
                source: PREFAB_RELATIVE.into(),
                parent: None,
                expected_revision: base.revision,
                expected_generation: base.generation,
            },
        )
        .expect("MCP prefab instantiate");
    let through_mcp = value(&mutation);

    assert_eq!(
        semantic(without_project_path(through_cli, &cli_project)),
        semantic(without_project_path(through_mcp, &mcp_project))
    );
    // The CLI persists the Scene as part of `prefab instantiate`, so the two
    // adapters must also agree on the committed document.
    let cli_scene: Value = serde_json::from_str(
        &fs::read_to_string(cli_project.scene_path()).expect("CLI committed scene"),
    )
    .expect("CLI scene JSON");
    let mcp_scene: Value = serde_json::from_str(
        &session
            .scene()
            .to_canonical_json()
            .expect("MCP committed scene"),
    )
    .expect("MCP scene JSON");
    assert_eq!(
        scene_shape(&without_project_path(cli_scene, &cli_project)),
        scene_shape(&without_project_path(mcp_scene, &mcp_project))
    );
}

#[test]
fn behavior_tree_apply_is_equivalent_across_cli_and_mcp() {
    let domain = BehaviorTreeDomain::new();
    let (graph, sequence) = behavior_tree_fixture(&domain);
    let new_action = NodeId::generate();
    let commands = vec![
        GraphCommand::AddNode {
            node: domain.action_node(new_action.clone(), "extra_action"),
        },
        GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), sequence, new_action, 1),
        },
    ];
    let service = BehaviorTreeAuthoringService::new();
    let directory = temp_directory("behavior_tree");
    let graph_path = directory.join("tree.graph.json");
    let commands_path = directory.join("commands.json");
    let fixture_json = service
        .graph_to_canonical_json(&graph)
        .expect("fixture serializes");
    fs::write(&graph_path, &fixture_json).expect("graph fixture");
    write_json(&commands_path, &commands);

    let through_cli = cli([
        "behavior-tree",
        "apply",
        &graph_path.display().to_string(),
        &commands_path.display().to_string(),
    ]);
    let through_mcp = BehaviorTreeMcpTools::new()
        .behavior_tree_apply(
            &writable(),
            BehaviorTreeApplyInput {
                graph: reload_graph(&fixture_json),
                commands,
            },
        )
        .expect("MCP behavior tree apply");

    // The CLI reports the shared transaction result and persists the graph,
    // while MCP returns the same result plus the updated graph in-band.
    assert_eq!(
        semantic(through_cli),
        semantic(serde_json::json!({
            "success": through_mcp.success,
            "diagnostics": value(&through_mcp.diagnostics),
            "diff": value(&through_mcp.diff),
        }))
    );
    assert_eq!(
        fs::read_to_string(&graph_path).expect("CLI committed graph"),
        service
            .graph_to_canonical_json(through_mcp.graph().expect("MCP committed graph"))
            .expect("MCP graph serializes")
    );
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn behavior_tree_read_operations_are_equivalent_across_cli_and_mcp() {
    let domain = BehaviorTreeDomain::new();
    let (graph, _sequence) = behavior_tree_fixture(&domain);
    let service = BehaviorTreeAuthoringService::new();
    let directory = temp_directory("behavior_tree_read");
    let graph_path = directory.join("tree.graph.json");
    let fixture_json = service
        .graph_to_canonical_json(&graph)
        .expect("fixture serializes");
    fs::write(&graph_path, &fixture_json).expect("graph fixture");
    let path = graph_path.display().to_string();
    let tools = BehaviorTreeMcpTools::new();
    let permissions = AuthoringPermissions::read_only();

    for (command, mcp) in [
        (
            "validate",
            value(
                tools
                    .behavior_tree_validate(
                        &permissions,
                        BehaviorTreeGraphInput {
                            graph: reload_graph(&fixture_json),
                        },
                    )
                    .expect("MCP validate"),
            ),
        ),
        (
            "compile",
            value(
                tools
                    .behavior_tree_compile(
                        &permissions,
                        BehaviorTreeGraphInput {
                            graph: reload_graph(&fixture_json),
                        },
                    )
                    .expect("MCP compile"),
            ),
        ),
        (
            "layout",
            value(
                tools
                    .behavior_tree_layout(
                        &permissions,
                        BehaviorTreeGraphInput {
                            graph: reload_graph(&fixture_json),
                        },
                    )
                    .expect("MCP layout"),
            ),
        ),
        (
            "nodes",
            value(
                tools
                    .behavior_tree_nodes(
                        &permissions,
                        BehaviorTreeGraphInput {
                            graph: reload_graph(&fixture_json),
                        },
                    )
                    .expect("MCP nodes"),
            ),
        ),
        (
            "edges",
            value(
                tools
                    .behavior_tree_edges(
                        &permissions,
                        BehaviorTreeGraphInput {
                            graph: reload_graph(&fixture_json),
                        },
                    )
                    .expect("MCP edges"),
            ),
        ),
    ] {
        let through_cli = cli_result(vec![
            "behavior-tree".to_owned(),
            command.to_owned(),
            path.clone(),
        ]);
        assert_eq!(
            semantic(through_cli),
            semantic(mcp),
            "`behavior-tree {command}` must match its MCP tool"
        );
    }

    assert_eq!(
        cli_result(vec!["behavior-tree".to_owned(), "schemas".to_owned()]),
        value(
            tools
                .behavior_tree_schemas(&permissions)
                .expect("MCP schemas")
        )
    );
    let _ = fs::remove_dir_all(directory);
}
