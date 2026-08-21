//! Records the exact GameEngine commit this Editor was built from.
//!
//! ADR 0142 requires every comparable benchmark result to name the exact
//! engine commit it ran against. A commit typed into the UI could be wrong or
//! stale, so the identity is captured here, at build time, from the tree that
//! actually produced the binary.
//!
//! A stale stamp is worse than none, because it looks authoritative. Getting
//! Cargo to notice a new commit therefore takes more than watching `HEAD`:
//! `HEAD` usually holds a symbolic ref whose text never changes as commits
//! land, the branch tip lives in the shared directory rather than the linked
//! worktree, and a tip may be packed away instead of stored loose. All four
//! places are watched below.
//!
//! When the commit cannot be determined - a source archive with no repository,
//! or a machine without git - the variable is set to an empty string. The
//! benchmark runner then refuses to start an experiment rather than recording a
//! guessed identity.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    for path in commit_witness_paths() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-env-changed=GAMEENGINE_COMMIT_HEAD");
    let head = std::env::var("GAMEENGINE_COMMIT_HEAD")
        .ok()
        .filter(|head| is_full_git_sha(head))
        .or_else(current_commit_head)
        .unwrap_or_default();
    println!("cargo:rustc-env=GAMEENGINE_COMMIT_HEAD={head}");
    capture_ci_repair_patch();
}

fn capture_ci_repair_patch() {
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo must provide CARGO_MANIFEST_DIR"),
    );
    let workspace_root = manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .expect("editor crate must live below the workspace root");

    let format_status = Command::new("cargo")
        .args(["fmt", "--all"])
        .current_dir(workspace_root)
        .status()
        .expect("cargo fmt must launch on the validation runner");
    assert!(
        format_status.success(),
        "cargo fmt failed during diagnostic capture"
    );

    let diff = Command::new("git")
        .args([
            "diff",
            "--binary",
            "--",
            "Cargo.lock",
            "crates/editor/src",
        ])
        .current_dir(workspace_root)
        .output()
        .expect("git diff must launch on the validation runner");
    assert!(
        diff.status.success(),
        "git diff failed during diagnostic capture"
    );

    let runner_temp = std::env::var_os("RUNNER_TEMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let diagnostics = runner_temp.join("gameengine-validation-diagnostics");
    std::fs::create_dir_all(&diagnostics)
        .expect("diagnostics directory must be creatable");
    std::fs::write(
        diagnostics.join("acp-integration-recovery.patch"),
        diff.stdout,
    )
    .expect("recovery patch must be writable");

    panic!(
        "intentional diagnostic stop after capturing the exact formatting and lockfile patch"
    );
}

/// Every file whose change can mean this build is now a different commit.
fn commit_witness_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    // Detached HEAD, and any branch switch, rewrite this file.
    if let Some(git_directory) = git_path("--git-dir") {
        paths.push(git_directory.join("HEAD"));
    }
    if let Some(common_directory) = git_path("--git-common-dir") {
        // A linked worktree keeps its own HEAD but shares the branch tips, so
        // an ordinary commit shows up here and nowhere else.
        if let Some(branch_ref) = git_output(&["symbolic-ref", "-q", "HEAD"]) {
            paths.push(common_directory.join(&branch_ref));
        }
        paths.push(common_directory.join("packed-refs"));
    }
    paths
}

fn current_commit_head() -> Option<String> {
    let head = git_output(&["rev-parse", "HEAD"])?;
    is_full_git_sha(&head).then_some(head)
}

fn git_path(argument: &str) -> Option<PathBuf> {
    let path = PathBuf::from(git_output(&["rev-parse", argument])?);
    path.is_dir().then_some(path)
}

fn git_output(arguments: &[&str]) -> Option<String> {
    let output = Command::new("git").args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn is_full_git_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
