#!/usr/bin/env python3
"""Build and preflight GameEngine ChatGPT Dispatcher requests.

The normal producer path stages intended product changes in a real checkout and
lets Git generate the unified diff. The same module can preflight a staged
request against the exact remote target before trusted transport publication.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
from typing import Any, Iterable

MAX_PART_BYTES = 60_000
MAX_PATCH_BYTES = 4 * 1024 * 1024
REQUEST_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
TARGET_BRANCH_RE = re.compile(r"^chatgpt/gameengine-[a-z0-9][a-z0-9._/-]{0,80}$")
SHA40_RE = re.compile(r"^[0-9a-fA-F]{40}$")
SHA256_RE = re.compile(r"^[0-9a-fA-F]{64}$")

ALLOWED_TOP_LEVEL_FILES = {
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rustfmt.toml",
    ".gitattributes",
    ".gitignore",
    "AGENTS.md",
    "CLAUDE.md",
    "README.md",
}
ALLOWED_PREFIXES = ("crates/", "examples/", "docs/", "scripts/")
FORBIDDEN_PREFIXES = (".github/", ".chatgpt-requests/")


class ProtocolError(RuntimeError):
    pass


def _run(
    cwd: Path,
    *args: str,
    input_bytes: bytes | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[bytes]:
    proc = subprocess.run(
        list(args),
        cwd=cwd,
        input=input_bytes,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and proc.returncode != 0:
        command = " ".join(args)
        stderr = proc.stderr.decode("utf-8", errors="replace").strip()
        raise ProtocolError(f"command failed ({command}): {stderr}")
    return proc


def _git(cwd: Path, *args: str, check: bool = True) -> bytes:
    return _run(cwd, "git", *args, check=check).stdout


def _text(data: bytes) -> str:
    return data.decode("utf-8", errors="strict").strip()


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _require_sha40(name: str, value: str) -> str:
    if not SHA40_RE.fullmatch(value):
        raise ProtocolError(f"{name} must be a full 40-character SHA")
    return value.lower()


def _require_safe_branch(branch: str) -> None:
    if not TARGET_BRANCH_RE.fullmatch(branch) or ".." in branch or "//" in branch:
        raise ProtocolError("target_branch must be a safe chatgpt/gameengine-* branch")


def _validate_product_path(path: str) -> None:
    if path.startswith(FORBIDDEN_PREFIXES):
        raise ProtocolError(f"trusted automation or transport path is forbidden: {path}")
    if path in ALLOWED_TOP_LEVEL_FILES or path.startswith(ALLOWED_PREFIXES):
        return
    raise ProtocolError(f"path is outside the public GameEngine allow-list: {path}")


def validate_manifest(manifest: dict[str, Any], request_id: str | None = None) -> None:
    required_base = {
        "schema_version": int,
        "request_id": str,
        "target_branch": str,
        "expected_head_sha": str,
        "patch_parts": list,
        "commit_message": str,
        "pr_title": str,
        "pr_body": str,
    }
    for key, expected_type in required_base.items():
        if key not in manifest or not isinstance(manifest[key], expected_type):
            raise ProtocolError(f"ready.json field {key!r} has an invalid type")

    schema = manifest["schema_version"]
    if schema not in (1, 2):
        raise ProtocolError("schema_version must be 1 or 2")

    rid = manifest["request_id"]
    if not REQUEST_ID_RE.fullmatch(rid):
        raise ProtocolError("request_id contains invalid characters")
    if request_id is not None and rid != request_id:
        raise ProtocolError("request_id must match its request directory")

    _require_safe_branch(manifest["target_branch"])
    _require_sha40("expected_head_sha", manifest["expected_head_sha"])

    parts = manifest["patch_parts"]
    if not 1 <= len(parts) <= 64 or any(not isinstance(part, str) for part in parts):
        raise ProtocolError("patch_parts must contain 1 to 64 string entries")
    expected_parts = [f"part-{index:04d}.patch" for index in range(len(parts))]
    if parts != expected_parts:
        raise ProtocolError("patch_parts must be contiguous part-NNNN.patch entries")

    if not manifest["commit_message"] or len(manifest["commit_message"]) > 120:
        raise ProtocolError("commit_message is invalid")
    if "\n" in manifest["commit_message"] or "\r" in manifest["commit_message"]:
        raise ProtocolError("commit_message must be one line")
    if not manifest["pr_title"] or len(manifest["pr_title"]) > 200:
        raise ProtocolError("pr_title is invalid")
    if "\n" in manifest["pr_title"] or "\r" in manifest["pr_title"]:
        raise ProtocolError("pr_title must be one line")
    if len(manifest["pr_body"]) > 8000:
        raise ProtocolError("pr_body is too long")
    if "<!-- gameengine-chatgpt-automation -->" in manifest["pr_body"]:
        raise ProtocolError("legacy auto-merge authorization is forbidden")

    if schema == 2:
        for key, expected_type in {
            "baseline_main_sha": str,
            "patch_sha256": str,
            "patch_bytes": int,
        }.items():
            if key not in manifest or not isinstance(manifest[key], expected_type):
                raise ProtocolError(f"schema v2 field {key!r} has an invalid type")
        _require_sha40("baseline_main_sha", manifest["baseline_main_sha"])
        if not SHA256_RE.fullmatch(manifest["patch_sha256"]):
            raise ProtocolError("patch_sha256 must be a 64-character SHA-256")
        if not 1 <= manifest["patch_bytes"] <= MAX_PATCH_BYTES:
            raise ProtocolError("patch_bytes is outside the allowed range")


def split_patch(patch: bytes, limit: int = MAX_PART_BYTES) -> list[bytes]:
    if not patch:
        raise ProtocolError("patch is empty")
    if len(patch) > MAX_PATCH_BYTES:
        raise ProtocolError("patch exceeds 4 MiB")
    parts: list[bytes] = []
    current = bytearray()
    for line in patch.splitlines(keepends=True):
        if len(line) > limit:
            raise ProtocolError("a patch line exceeds the per-part transport limit")
        if current and len(current) + len(line) > limit:
            parts.append(bytes(current))
            current.clear()
        current.extend(line)
    if current:
        parts.append(bytes(current))
    if not 1 <= len(parts) <= 64:
        raise ProtocolError("patch requires more than 64 transport parts")
    return parts


def _check_changed_paths(repo: Path) -> None:
    raw = _git(repo, "diff", "--cached", "--name-only", "--no-renames", "-z")
    paths = [item.decode("utf-8") for item in raw.split(b"\0") if item]
    if not paths:
        raise ProtocolError("patch produced no staged changes")
    for path in paths:
        _validate_product_path(path)

    raw_modes = _text(_git(repo, "diff", "--cached", "--raw", "--no-renames"))
    for line in raw_modes.splitlines():
        match = re.match(r"^:([0-9]{6}) ([0-9]{6}) ", line)
        if not match:
            continue
        old_mode, new_mode = match.groups()
        if "120000" in (old_mode, new_mode):
            raise ProtocolError("symlink changes are forbidden")
        if "160000" in (old_mode, new_mode):
            raise ProtocolError("submodule changes are forbidden")


def preflight_patch(repo: Path, expected_head_sha: str, patch: bytes) -> None:
    expected = _require_sha40("expected_head_sha", expected_head_sha)
    if not patch.startswith(b"diff --git ") and b"\ndiff --git " not in patch:
        raise ProtocolError("request is not a unified Git patch")

    with tempfile.TemporaryDirectory(prefix="gameengine-chatgpt-preflight-") as temp_dir:
        worktree = Path(temp_dir) / "target"
        _git(repo, "worktree", "add", "--detach", "--force", str(worktree), expected)
        try:
            patch_path = Path(temp_dir) / "request.patch"
            patch_path.write_bytes(patch)
            strict = _run(
                worktree,
                "git",
                "apply",
                "--check",
                "--whitespace=error-all",
                str(patch_path),
                check=False,
            )
            if strict.returncode != 0:
                detail = strict.stderr.decode("utf-8", errors="replace").strip()
                raise ProtocolError(f"git apply --check --whitespace=error-all rejected patch: {detail}")
            _run(worktree, "git", "apply", "--index", "--whitespace=nowarn", str(patch_path))
            diff_check = _run(worktree, "git", "diff", "--cached", "--check", check=False)
            if diff_check.returncode != 0:
                detail = diff_check.stdout.decode("utf-8", errors="replace").strip()
                raise ProtocolError(f"git diff --cached --check rejected patch: {detail}")
            _check_changed_paths(worktree)
        finally:
            _git(repo, "worktree", "remove", "--force", str(worktree), check=False)


def _ls_remote(repo: Path, remote: str, ref: str) -> str:
    output = _text(_git(repo, "ls-remote", remote, ref))
    if not output:
        raise ProtocolError(f"remote ref does not exist: {ref}")
    return output.split()[0].lower()


def _load_stage_request(request_repo: Path, request_commit: str, request_id: str) -> tuple[dict[str, Any], bytes]:
    commit = _require_sha40("request_commit", request_commit)
    if not REQUEST_ID_RE.fullmatch(request_id):
        raise ProtocolError("request_id contains invalid characters")
    request_dir = f".chatgpt-requests/{request_id}"
    ready_path = f"{request_dir}/ready.json"
    manifest_bytes = _git(request_repo, "show", f"{commit}:{ready_path}")
    try:
        manifest = json.loads(manifest_bytes.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ProtocolError(f"ready.json is invalid: {exc}") from exc
    validate_manifest(manifest, request_id=request_id)

    listed = _text(_git(request_repo, "ls-tree", "-r", "--name-only", commit, "--", request_dir)).splitlines()
    relative = sorted(path[len(request_dir) + 1 :] for path in listed if path.startswith(request_dir + "/"))
    expected_files = sorted([*manifest["patch_parts"], "ready.json"])
    if relative != expected_files:
        raise ProtocolError("request directory contains unexpected files")

    chunks: list[bytes] = []
    for part_name in manifest["patch_parts"]:
        path = f"{request_dir}/{part_name}"
        mode_line = _text(_git(request_repo, "ls-tree", commit, "--", path))
        if not mode_line.startswith("100644 "):
            raise ProtocolError(f"{part_name} must be a normal non-executable file")
        blob = _git(request_repo, "show", f"{commit}:{path}")
        if not 1 <= len(blob) <= MAX_PART_BYTES:
            raise ProtocolError(f"{part_name} is outside the allowed size range")
        chunks.append(blob)
    patch = b"".join(chunks)
    if not 1 <= len(patch) <= MAX_PATCH_BYTES:
        raise ProtocolError("reconstructed patch is outside the allowed size range")
    return manifest, patch


def preflight_stage(request_repo: Path, request_commit: str, request_id: str, remote: str = "origin") -> dict[str, Any]:
    manifest, patch = _load_stage_request(request_repo, request_commit, request_id)
    target_branch = manifest["target_branch"]
    expected = manifest["expected_head_sha"].lower()

    target_head = _ls_remote(request_repo, remote, f"refs/heads/{target_branch}")
    if target_head != expected:
        raise ProtocolError(f"target branch moved: observed {target_head}, expected {expected}")

    _git(request_repo, "fetch", "--no-tags", remote, expected)

    if manifest["schema_version"] == 2:
        if len(patch) != manifest["patch_bytes"]:
            raise ProtocolError("schema v2 patch_bytes does not match reconstructed patch")
        if _sha256(patch) != manifest["patch_sha256"].lower():
            raise ProtocolError("schema v2 patch_sha256 does not match reconstructed patch")
        main_head = _ls_remote(request_repo, remote, "refs/heads/main")
        baseline = manifest["baseline_main_sha"].lower()
        if main_head != baseline:
            raise ProtocolError(f"main advanced: observed {main_head}, request baseline is {baseline}")
        _git(request_repo, "fetch", "--no-tags", remote, baseline)
        ancestry = _run(request_repo, "git", "merge-base", "--is-ancestor", baseline, expected, check=False)
        if ancestry.returncode != 0:
            raise ProtocolError("target branch does not contain the declared current-main baseline")

    preflight_patch(request_repo, expected, patch)
    return {
        "schema_version": manifest["schema_version"],
        "request_id": request_id,
        "target_branch": target_branch,
        "expected_head_sha": expected,
        "patch_sha256": _sha256(patch),
        "patch_bytes": len(patch),
    }


def _require_staged_only(workspace: Path) -> None:
    staged = _run(workspace, "git", "diff", "--cached", "--quiet", "HEAD", check=False)
    if staged.returncode == 0:
        raise ProtocolError("no staged product changes were found")
    if staged.returncode not in (0, 1):
        raise ProtocolError("could not inspect staged changes")
    unstaged = _run(workspace, "git", "diff", "--quiet", check=False)
    if unstaged.returncode != 0:
        raise ProtocolError("unstaged tracked changes exist; stage the exact intended product changes first")
    untracked = _text(_git(workspace, "ls-files", "--others", "--exclude-standard"))
    if untracked:
        raise ProtocolError("untracked files exist; stage or remove them before building a request")


def build_request(args: argparse.Namespace) -> dict[str, Any]:
    workspace = Path(args.workspace).resolve()
    output_dir = Path(args.output_dir).resolve()
    _require_safe_branch(args.target_branch)
    if not REQUEST_ID_RE.fullmatch(args.request_id):
        raise ProtocolError("request_id contains invalid characters")
    expected = _require_sha40("expected_head_sha", args.expected_head_sha)
    baseline = _require_sha40("baseline_main_sha", args.baseline_main_sha)

    actual = _text(_git(workspace, "rev-parse", "HEAD")).lower()
    if actual != expected:
        raise ProtocolError(f"workspace HEAD is {actual}, expected {expected}")
    _git(workspace, "cat-file", "-e", f"{baseline}^{{commit}}")
    ancestry = _run(workspace, "git", "merge-base", "--is-ancestor", baseline, expected, check=False)
    if ancestry.returncode != 0:
        raise ProtocolError("expected_head_sha does not contain baseline_main_sha")

    _require_staged_only(workspace)
    patch = _git(workspace, "diff", "--cached", "--binary", "--full-index", "--no-ext-diff", "HEAD", "--")
    preflight_patch(workspace, expected, patch)

    if not args.skip_remote_recheck:
        remote_target = _ls_remote(workspace, args.remote, f"refs/heads/{args.target_branch}")
        if remote_target != expected:
            raise ProtocolError(f"remote target moved: observed {remote_target}, expected {expected}")
        remote_main = _ls_remote(workspace, args.remote, "refs/heads/main")
        if remote_main != baseline:
            raise ProtocolError(f"remote main moved: observed {remote_main}, expected baseline {baseline}")

    parts = split_patch(patch)
    if output_dir.exists():
        if any(output_dir.iterdir()):
            raise ProtocolError("output directory must be absent or empty")
    else:
        output_dir.mkdir(parents=True)

    part_names: list[str] = []
    for index, blob in enumerate(parts):
        name = f"part-{index:04d}.patch"
        (output_dir / name).write_bytes(blob)
        part_names.append(name)

    pr_body = args.pr_body
    if args.pr_body_file:
        pr_body = Path(args.pr_body_file).read_text(encoding="utf-8")

    manifest = {
        "schema_version": 2,
        "request_id": args.request_id,
        "target_branch": args.target_branch,
        "expected_head_sha": expected,
        "baseline_main_sha": baseline,
        "patch_sha256": _sha256(patch),
        "patch_bytes": len(patch),
        "patch_parts": part_names,
        "commit_message": args.commit_message,
        "pr_title": args.pr_title,
        "pr_body": pr_body,
    }
    validate_manifest(manifest, request_id=args.request_id)
    (output_dir / "ready.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n")
    return manifest


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    build = sub.add_parser("build", help="build a schema v2 request from staged product changes")
    build.add_argument("--workspace", default=".")
    build.add_argument("--target-branch", required=True)
    build.add_argument("--expected-head-sha", required=True)
    build.add_argument("--baseline-main-sha", required=True)
    build.add_argument("--request-id", required=True)
    build.add_argument("--commit-message", required=True)
    build.add_argument("--pr-title", required=True)
    body = build.add_mutually_exclusive_group(required=True)
    body.add_argument("--pr-body")
    body.add_argument("--pr-body-file")
    build.add_argument("--output-dir", required=True)
    build.add_argument("--remote", default="origin")
    build.add_argument("--skip-remote-recheck", action="store_true", help=argparse.SUPPRESS)

    preflight = sub.add_parser("preflight-stage", help="preflight an immutable staged request before transport publication")
    preflight.add_argument("--request-repo", required=True)
    preflight.add_argument("--request-commit", required=True)
    preflight.add_argument("--request-id", required=True)
    preflight.add_argument("--remote", default="origin")
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.command == "build":
            result = build_request(args)
        else:
            result = preflight_stage(Path(args.request_repo).resolve(), args.request_commit, args.request_id, args.remote)
        print(json.dumps(result, sort_keys=True))
        return 0
    except (ProtocolError, OSError, subprocess.SubprocessError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
