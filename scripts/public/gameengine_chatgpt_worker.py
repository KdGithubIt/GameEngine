#!/usr/bin/env python3
"""Current-protocol GameEngine ChatGPT Worker.

This helper is for a producer that has a real GameEngine checkout plus git/gh.
An input patch is used only to materialize intended product changes in a disposable
exact-target worktree. The authoritative Dispatcher request is always regenerated
by .github/chatgpt/request_protocol.py build and published through the existing
stage -> signal -> trusted publisher -> dispatcher path.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable

EXPECTED_REPOSITORY = "KdGithubIt/GameEngine"
TARGET_RE = re.compile(r"^chatgpt/gameengine-[a-z0-9][a-z0-9._/-]{0,80}$")
REQUEST_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
SHA40_RE = re.compile(r"^[0-9a-fA-F]{40}$")
STAGE_SIGNAL = "gameengine-chatgpt-stage-signal.yml"
PUBLISHER = "gameengine-chatgpt-transport-publisher.yml"
DISPATCHER = "gameengine-chatgpt-dispatcher.yml"
VALIDATION = "gameengine-windows-validation.yml"
VISUAL = "gameengine-editor-visual-validation.yml"


class WorkerError(RuntimeError):
    pass


def run(cwd: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(
        list(args),
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and proc.returncode != 0:
        detail = proc.stderr.strip() or proc.stdout.strip()
        raise WorkerError(f"command failed ({' '.join(args)}): {detail}")
    return proc


def git(cwd: Path, *args: str, check: bool = True) -> str:
    return run(cwd, "git", *args, check=check).stdout.strip()


def gh(cwd: Path, *args: str, check: bool = True) -> str:
    return run(cwd, "gh", *args, check=check).stdout.strip()


def require_tool(name: str) -> None:
    if shutil.which(name) is None:
        raise WorkerError(f"required tool is not available: {name}")


def require_target_branch(branch: str) -> None:
    if not TARGET_RE.fullmatch(branch) or ".." in branch or "//" in branch:
        raise WorkerError("target branch must be a safe chatgpt/gameengine-* branch")


def require_request_id(request_id: str) -> None:
    if not REQUEST_RE.fullmatch(request_id):
        raise WorkerError("request id contains invalid characters or is too long")


def ls_remote(repo: Path, remote: str, ref: str, *, required: bool = True) -> str:
    output = git(repo, "ls-remote", remote, ref)
    if not output:
        if required:
            raise WorkerError(f"remote ref does not exist: {ref}")
        return ""
    sha = output.split()[0].lower()
    if not SHA40_RE.fullmatch(sha):
        raise WorkerError(f"remote ref did not resolve to a full SHA: {ref}")
    return sha


def gh_json(repo: Path, *args: str) -> Any:
    raw = gh(repo, *args)
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise WorkerError(f"gh returned invalid JSON for {' '.join(args)}: {exc}") from exc


def list_runs(repo: Path, workflow: str) -> list[dict[str, Any]]:
    data = gh_json(
        repo,
        "run",
        "list",
        "--repo",
        EXPECTED_REPOSITORY,
        "--workflow",
        workflow,
        "--limit",
        "50",
        "--json",
        "databaseId,status,conclusion,displayTitle,headBranch,headSha,createdAt,url",
    )
    if not isinstance(data, list):
        raise WorkerError(f"unexpected gh run list result for {workflow}")
    return data


def wait_for_run(
    repo: Path,
    workflow: str,
    predicate: Callable[[dict[str, Any]], bool],
    timeout_seconds: int,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        for item in list_runs(repo, workflow):
            if predicate(item):
                run_id = str(item["databaseId"])
                watched = run(
                    repo,
                    "gh",
                    "run",
                    "watch",
                    run_id,
                    "--repo",
                    EXPECTED_REPOSITORY,
                    "--exit-status",
                    check=False,
                )
                final = gh_json(
                    repo,
                    "run",
                    "view",
                    run_id,
                    "--repo",
                    EXPECTED_REPOSITORY,
                    "--json",
                    "databaseId,status,conclusion,displayTitle,headBranch,headSha,url",
                )
                if watched.returncode != 0 or final.get("conclusion") != "success":
                    raise WorkerError(
                        f"{workflow} failed: {final.get('url', '')} ({final.get('conclusion', 'unknown')})"
                    )
                return final
        time.sleep(5)
    raise WorkerError(f"timed out waiting for workflow: {workflow}")


def copy_builder_output(output_dir: Path, request_dir: Path) -> list[Path]:
    parts = sorted(output_dir.glob("part-*.patch"))
    if not parts:
        raise WorkerError("request builder produced no patch parts")
    request_dir.mkdir(parents=True, exist_ok=False)
    copied: list[Path] = []
    for source in parts:
        target = request_dir / source.name
        shutil.copyfile(source, target)
        copied.append(target)
    return copied


def resolve_open_pr(repo: Path, target_branch: str, target_head: str) -> dict[str, Any]:
    items = gh_json(
        repo,
        "pr",
        "list",
        "--repo",
        EXPECTED_REPOSITORY,
        "--state",
        "open",
        "--head",
        target_branch,
        "--json",
        "number,url,title,isDraft,headRefOid,body",
    )
    for item in items:
        if str(item.get("headRefOid", "")).lower() == target_head.lower():
            return item
    raise WorkerError("dispatcher succeeded but matching open PR was not found")


def materialize_and_build(
    repo: Path,
    patch_file: Path,
    target_branch: str,
    expected_head: str,
    baseline_main: str,
    request_id: str,
    commit_message: str,
    pr_title: str,
    pr_body_file: Path,
    remote: str,
    temp_root: Path,
) -> Path:
    authoring = temp_root / "authoring"
    output_dir = temp_root / "request"
    git(repo, "worktree", "add", "--detach", "--force", str(authoring), expected_head)
    try:
        apply_result = run(
            authoring,
            "git",
            "apply",
            "--index",
            "--whitespace=error-all",
            str(patch_file.resolve()),
            check=False,
        )
        if apply_result.returncode != 0:
            raise WorkerError(f"input patch does not apply cleanly to exact target: {apply_result.stderr.strip()}")

        builder = repo / ".github" / "chatgpt" / "request_protocol.py"
        if not builder.is_file():
            raise WorkerError("canonical request builder is missing")
        result = run(
            repo,
            sys.executable,
            str(builder),
            "build",
            "--workspace",
            str(authoring),
            "--target-branch",
            target_branch,
            "--expected-head-sha",
            expected_head,
            "--baseline-main-sha",
            baseline_main,
            "--request-id",
            request_id,
            "--commit-message",
            commit_message,
            "--pr-title",
            pr_title,
            "--pr-body-file",
            str(pr_body_file.resolve()),
            "--output-dir",
            str(output_dir),
            "--remote",
            remote,
        )
        built = json.loads(result.stdout)
        if built.get("schema_version") != 2 or built.get("request_id") != request_id:
            raise WorkerError("canonical builder returned an unexpected request manifest")
        return output_dir
    finally:
        git(repo, "worktree", "remove", "--force", str(authoring), check=False)


def publish_stage(
    repo: Path,
    output_dir: Path,
    request_id: str,
    target_branch: str,
    expected_head: str,
    baseline_main: str,
    remote: str,
    temp_root: Path,
) -> tuple[str, str]:
    stage_branch = f"chatgpt-dispatch-stage-{request_id}"
    if ls_remote(repo, remote, f"refs/heads/{stage_branch}", required=False):
        raise WorkerError("stage branch already exists; use a new request id")

    if ls_remote(repo, remote, f"refs/heads/{target_branch}") != expected_head:
        raise WorkerError("target branch moved after build; rebuild from current state")
    if ls_remote(repo, remote, "refs/heads/main") != baseline_main:
        raise WorkerError("main moved after build; rebuild from current state")

    stage = temp_root / "stage"
    git(repo, "worktree", "add", "--detach", "--force", str(stage), baseline_main)
    try:
        git(stage, "switch", "-c", stage_branch)
        git(stage, "config", "user.name", "GameEngine ChatGPT Worker")
        git(stage, "config", "user.email", "41898282+github-actions[bot]@users.noreply.github.com")
        request_dir = stage / ".chatgpt-requests" / request_id
        copied = copy_builder_output(output_dir, request_dir)
        git(stage, "add", *[str(path.relative_to(stage)) for path in copied])
        git(stage, "commit", "-m", f"Stage ChatGPT request {request_id} patch parts")

        if ls_remote(repo, remote, f"refs/heads/{target_branch}") != expected_head:
            raise WorkerError("target branch moved before ready publication")
        if ls_remote(repo, remote, "refs/heads/main") != baseline_main:
            raise WorkerError("main moved before ready publication")

        ready = request_dir / "ready.json"
        shutil.copyfile(output_dir / "ready.json", ready)
        git(stage, "add", str(ready.relative_to(stage)))
        git(stage, "commit", "-m", f"Mark ChatGPT request {request_id} ready")
        ready_commit = git(stage, "rev-parse", "HEAD").lower()
        git(
            stage,
            "push",
            f"--force-with-lease=refs/heads/{stage_branch}:",
            remote,
            f"HEAD:refs/heads/{stage_branch}",
        )
        return stage_branch, ready_commit
    finally:
        git(repo, "worktree", "remove", "--force", str(stage), check=False)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", default=".")
    parser.add_argument("--patch-file", required=True)
    parser.add_argument("--target-branch", required=True)
    parser.add_argument("--request-id", required=True)
    parser.add_argument("--commit-message", required=True)
    parser.add_argument("--pr-title", required=True)
    parser.add_argument("--pr-body-file", required=True)
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--timeout-seconds", type=int, default=3600)
    args = parser.parse_args(argv)

    try:
        require_tool("git")
        require_tool("gh")
        repo = Path(args.workspace).resolve()
        patch_file = Path(args.patch_file).resolve()
        pr_body_file = Path(args.pr_body_file).resolve()
        require_target_branch(args.target_branch)
        require_request_id(args.request_id)
        if not patch_file.is_file() or not pr_body_file.is_file():
            raise WorkerError("patch file and PR body file must exist")
        root = Path(git(repo, "rev-parse", "--show-toplevel")).resolve()
        repo = root
        identity = gh_json(repo, "repo", "view", "--json", "nameWithOwner")
        if identity.get("nameWithOwner") != EXPECTED_REPOSITORY:
            raise WorkerError(f"worker is restricted to {EXPECTED_REPOSITORY}")

        expected_head = ls_remote(repo, args.remote, f"refs/heads/{args.target_branch}")
        baseline_main = ls_remote(repo, args.remote, "refs/heads/main")
        git(repo, "fetch", "--no-tags", args.remote, expected_head, baseline_main)
        ancestry = run(repo, "git", "merge-base", "--is-ancestor", baseline_main, expected_head, check=False)
        if ancestry.returncode != 0:
            raise WorkerError("target branch does not contain current main; direct builder protocol cannot publish stale baseline")

        with tempfile.TemporaryDirectory(prefix="gameengine-chatgpt-worker-") as temp:
            temp_root = Path(temp)
            output_dir = materialize_and_build(
                repo,
                patch_file,
                args.target_branch,
                expected_head,
                baseline_main,
                args.request_id,
                args.commit_message,
                args.pr_title,
                pr_body_file,
                args.remote,
                temp_root,
            )
            stage_branch, ready_commit = publish_stage(
                repo,
                output_dir,
                args.request_id,
                args.target_branch,
                expected_head,
                baseline_main,
                args.remote,
                temp_root,
            )

        timeout = max(args.timeout_seconds, 60)
        stage_run = wait_for_run(
            repo,
            STAGE_SIGNAL,
            lambda r: r.get("headBranch") == stage_branch and str(r.get("headSha", "")).lower() == ready_commit,
            timeout,
        )
        publisher_run = wait_for_run(
            repo,
            PUBLISHER,
            lambda r: stage_branch in str(r.get("displayTitle", "")),
            timeout,
        )

        git(repo, "fetch", "--no-tags", args.remote, "refs/heads/chatgpt-dispatch:refs/remotes/origin/chatgpt-dispatch")
        published_commit = git(
            repo,
            "log",
            "-1",
            "--format=%H",
            "refs/remotes/origin/chatgpt-dispatch",
            "--",
            f".chatgpt-requests/{args.request_id}/ready.json",
        ).lower()
        if not SHA40_RE.fullmatch(published_commit):
            raise WorkerError("could not resolve published transport commit")

        dispatcher_run = wait_for_run(
            repo,
            DISPATCHER,
            lambda r: published_commit in str(r.get("displayTitle", "")),
            timeout,
        )
        target_head = ls_remote(repo, args.remote, f"refs/heads/{args.target_branch}")
        if target_head == expected_head:
            raise WorkerError("dispatcher reported success but target branch did not advance")

        validation_run = wait_for_run(
            repo,
            VALIDATION,
            lambda r: args.request_id in str(r.get("displayTitle", "")) and target_head in str(r.get("displayTitle", "")),
            timeout,
        )
        pr = resolve_open_pr(repo, args.target_branch, target_head)

        visual = "not-requested"
        body = str(pr.get("body") or "")
        if "<!-- gameengine-visual-validation:" in body:
            visual_run = wait_for_run(
                repo,
                VISUAL,
                lambda r: f"PR #{pr['number']}" in str(r.get("displayTitle", "")) and target_head in str(r.get("displayTitle", "")),
                timeout,
            )
            visual = f"workflow-success-human-review-required:{visual_run.get('url', '')}"

        result = {
            "request_id": args.request_id,
            "stage_branch": stage_branch,
            "stage_ready_commit": ready_commit,
            "published_request_commit": published_commit,
            "target_branch": args.target_branch,
            "target_head": target_head,
            "pull_request": pr.get("url"),
            "stage_signal": stage_run.get("url"),
            "publisher": publisher_run.get("url"),
            "dispatcher": dispatcher_run.get("url"),
            "windows_validation": validation_run.get("url"),
            "visual_validation": visual,
        }
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    except (WorkerError, OSError, subprocess.SubprocessError, json.JSONDecodeError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
