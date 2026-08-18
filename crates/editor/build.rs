//! Records the exact GameEngine commit this Editor was built from.
//!
//! ADR 0142 requires every comparable benchmark result to name the exact
//! engine commit it ran against. A commit typed into the UI could be wrong or
//! stale, so the identity is captured here, at build time, from the tree that
//! actually produced the binary.
//!
//! When the commit cannot be determined - a source archive with no repository,
//! or a machine without git - the variable is set to an empty string. The
//! benchmark runner then refuses to start an experiment rather than recording a
//! guessed identity.

use std::process::Command;

fn main() {
    if let Some(git_directory) = locate_git_directory() {
        // A new commit changes HEAD, and a checkout of a different branch
        // changes what HEAD points at, so both are worth watching.
        println!("cargo:rerun-if-changed={}/HEAD", git_directory.display());
        println!("cargo:rerun-if-changed={}/refs", git_directory.display());
    }
    println!("cargo:rerun-if-env-changed=GAMEENGINE_COMMIT_HEAD");
    let head = std::env::var("GAMEENGINE_COMMIT_HEAD")
        .ok()
        .filter(|head| is_full_git_sha(head))
        .or_else(current_commit_head)
        .unwrap_or_default();
    println!("cargo:rustc-env=GAMEENGINE_COMMIT_HEAD={head}");
}

fn current_commit_head() -> Option<String> {
    let output = Command::new("git").args(["rev-parse", "HEAD"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let head = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    is_full_git_sha(&head).then_some(head)
}

fn locate_git_directory() -> Option<std::path::PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let directory = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    let directory = std::path::PathBuf::from(directory);
    directory.is_dir().then_some(directory)
}

fn is_full_git_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
