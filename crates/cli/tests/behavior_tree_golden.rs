use engine_cli::run_cli_with_status;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn compile_output_matches_golden_file() {
    let graph = fixture("behavior_tree_valid.graph.json");
    let expected = fixture_text("behavior_tree_compile.golden.json");

    let result = run_cli_with_status([
        "behavior-tree".to_owned(),
        "compile".to_owned(),
        graph.display().to_string(),
    ])
    .expect("compile command must run");

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.output, trim_trailing_newline(&expected));
}

#[test]
fn apply_output_matches_golden_file_and_writes_graph() {
    let graph = temp_graph("apply_golden");
    fs::copy(fixture("behavior_tree_valid.graph.json"), &graph)
        .expect("fixture graph must copy to temp");
    let commands = fixture("behavior_tree_apply.commands.json");
    let expected = fixture_text("behavior_tree_apply.golden.json");

    let result = run_cli_with_status([
        "behavior-tree".to_owned(),
        "apply".to_owned(),
        graph.display().to_string(),
        commands.display().to_string(),
    ])
    .expect("apply command must run");

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.output, trim_trailing_newline(&expected));
    let saved = fs::read_to_string(&graph).expect("apply must leave graph readable");
    assert!(
        saved.contains("node_01234567890ABCDEFGHJKMNPQ4"),
        "apply must persist the new node"
    );
    let _ = fs::remove_file(graph);
}

#[test]
fn apply_failure_does_not_write_graph_regression() {
    let graph = temp_graph("apply_failure");
    fs::copy(fixture("behavior_tree_valid.graph.json"), &graph)
        .expect("fixture graph must copy to temp");
    let original = fs::read_to_string(&graph).expect("temp graph must be readable");
    let commands = fixture("behavior_tree_duplicate.commands.json");

    let result = run_cli_with_status([
        "behavior-tree".to_owned(),
        "apply".to_owned(),
        graph.display().to_string(),
        commands.display().to_string(),
    ])
    .expect("apply failure must still return structured output");

    assert_eq!(result.exit_code, 1);
    assert_eq!(
        fs::read_to_string(&graph).expect("temp graph must remain readable"),
        original
    );
    let _ = fs::remove_file(graph);
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn fixture_text(name: &str) -> String {
    fs::read_to_string(fixture(name)).expect("fixture text must be readable")
}

fn temp_graph(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time must be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "engine_cli_{name}_{}_{}.json",
        std::process::id(),
        nonce
    ))
}

fn trim_trailing_newline(text: &str) -> &str {
    text.strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text)
}
