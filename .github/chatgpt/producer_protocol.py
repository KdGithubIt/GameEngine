#!/usr/bin/env python3
"""Build Dispatcher requests from immutable connector-authored edit plans."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tempfile
from typing import Any, Iterable

import request_protocol

PRODUCER_BRANCH_RE = re.compile(r"^chatgpt-producer-stage-([A-Za-z0-9][A-Za-z0-9._-]{0,63})$")
PRODUCER_SCHEMA_VERSION = 1
PRODUCER_ROOT = ".chatgpt-producer"
MAX_EDIT_BYTES = 256_000
MAX_TOTAL_EDIT_BYTES = 4 * 1024 * 1024


class ProducerProtocolError(RuntimeError):
    pass


def _text(data: bytes) -> str:
    return data.decode("utf-8", errors="strict").strip()


def _git(repo: Path, *args: str, check: bool = True) -> bytes:
    return request_protocol._git(repo, *args, check=check)


def _require_sha40(name: str, value: str) -> str:
    try:
        return request_protocol._require_sha40(name, value)
    except request_protocol.ProtocolError as exc:
        raise ProducerProtocolError(str(exc)) from exc


def _request_dir(request_id: str) -> str:
    return f"{PRODUCER_ROOT}/{request_id}"


def _ready_path(request_id: str) -> str:
    return f"{_request_dir(request_id)}/ready.json"


def _validate_product_path(path: str) -> str:
    if not path or "\\" in path:
        raise ProducerProtocolError("edit path must be a repository-relative POSIX path")
    pure = PurePosixPath(path)
    if pure.is_absolute() or any(part in {"", ".", ".."} for part in pure.parts):
        raise ProducerProtocolError("edit path must not be absolute or contain dot traversal")
    normalized = pure.as_posix()
    if normalized != path:
        raise ProducerProtocolError("edit path must already be normalized")
    try:
        request_protocol._validate_product_path(path)
    except request_protocol.ProtocolError as exc:
        raise ProducerProtocolError(str(exc)) from exc
    return path


def _validate_text_fields(manifest: dict[str, Any]) -> None:
    commit_message = manifest["commit_message"]
    pr_title = manifest["pr_title"]
    pr_body = manifest["pr_body"]
    if not commit_message or len(commit_message) > 120 or "\n" in commit_message or "\r" in commit_message:
        raise ProducerProtocolError("commit_message must be a non-empty single line of at most 120 characters")
    if not pr_title or len(pr_title) > 200 or "\n" in pr_title or "\r" in pr_title:
        raise ProducerProtocolError("pr_title must be a non-empty single line of at most 200 characters")
    if len(pr_body) > 8000:
        raise ProducerProtocolError("pr_body is too long")
    if "<!-- gameengine-chatgpt-automation -->" in pr_body:
        raise ProducerProtocolError("legacy auto-merge authorization is forbidden")


def validate_manifest(manifest: dict[str, Any], request_id: str) -> None:
    required = {
        "schema_version": int,
        "request_id": str,
        "target_branch": str,
        "expected_head_sha": str,
        "baseline_main_sha": str,
        "edit_parts": list,
        "commit_message": str,
        "pr_title": str,
        "pr_body": str,
    }
    if set(manifest) != set(required):
        raise ProducerProtocolError("producer ready.json contains unexpected or missing fields")
    for key, expected_type in required.items():
        if not isinstance(manifest[key], expected_type):
            raise ProducerProtocolError(f"producer ready.json field {key!r} has an invalid type")
    if manifest["schema_version"] != PRODUCER_SCHEMA_VERSION:
        raise ProducerProtocolError(f"producer schema_version must be {PRODUCER_SCHEMA_VERSION}")
    if manifest["request_id"] != request_id or not request_protocol.REQUEST_ID_RE.fullmatch(request_id):
        raise ProducerProtocolError("request_id must match the producer branch and request directory")
    try:
        request_protocol._require_safe_branch(manifest["target_branch"])
    except request_protocol.ProtocolError as exc:
        raise ProducerProtocolError(str(exc)) from exc
    _require_sha40("expected_head_sha", manifest["expected_head_sha"])
    _require_sha40("baseline_main_sha", manifest["baseline_main_sha"])

    parts = manifest["edit_parts"]
    if not 1 <= len(parts) <= 64 or any(not isinstance(part, str) for part in parts):
        raise ProducerProtocolError("edit_parts must contain 1 to 64 string entries")
    expected_parts = [f"edit-{index:04d}.json" for index in range(len(parts))]
    if parts != expected_parts:
        raise ProducerProtocolError("edit_parts must be contiguous edit-NNNN.json entries")
    _validate_text_fields(manifest)


def _changed_entries(repo: Path, commit: str) -> list[tuple[str, str]]:
    raw = _git(repo, "diff-tree", "--no-commit-id", "--name-status", "-r", "--no-renames", "-z", commit)
    tokens = [token.decode("utf-8") for token in raw.split(b"\0") if token]
    if len(tokens) % 2 != 0:
        raise ProducerProtocolError("could not parse producer commit changes")
    return list(zip(tokens[0::2], tokens[1::2]))


def _single_parent(repo: Path, commit: str) -> str:
    line = _text(_git(repo, "rev-list", "--parents", "-n", "1", commit)).split()
    if len(line) != 2:
        raise ProducerProtocolError("producer commits must be linear non-merge commits")
    return line[1]


def _load_manifest(repo: Path, producer_commit: str, request_id: str) -> dict[str, Any]:
    ready_path = _ready_path(request_id)
    try:
        raw = _git(repo, "show", f"{producer_commit}:{ready_path}")
        manifest = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError, request_protocol.ProtocolError) as exc:
        raise ProducerProtocolError(f"producer ready.json is invalid: {exc}") from exc
    if not isinstance(manifest, dict):
        raise ProducerProtocolError("producer ready.json must be a JSON object")
    validate_manifest(manifest, request_id)
    return manifest


def _validate_history(repo: Path, baseline: str, producer_commit: str, manifest: dict[str, Any]) -> None:
    request_id = manifest["request_id"]
    request_dir = _request_dir(request_id)
    ready_path = _ready_path(request_id)
    ancestry = request_protocol._run(repo, "git", "merge-base", "--is-ancestor", baseline, producer_commit, check=False)
    if ancestry.returncode != 0:
        raise ProducerProtocolError("producer branch is not based on baseline_main_sha")

    commits = _text(_git(repo, "rev-list", "--reverse", f"{baseline}..{producer_commit}")).splitlines()
    if len(commits) < 2 or commits[-1] != producer_commit:
        raise ProducerProtocolError("producer branch must contain edit payload commits followed by one ready commit")

    for index, commit in enumerate(commits):
        _single_parent(repo, commit)
        entries = _changed_entries(repo, commit)
        if not entries:
            raise ProducerProtocolError("producer commits may not be empty")
        if index == len(commits) - 1:
            if entries != [("A", ready_path)]:
                raise ProducerProtocolError(f"final producer commit must add only {ready_path}")
            continue
        for status, path in entries:
            if status != "A" or not re.fullmatch(re.escape(request_dir) + r"/edit-[0-9]{4}\.json", path):
                raise ProducerProtocolError("producer payload commits may only add edit-NNNN.json files for this request")

    listed = _text(_git(repo, "ls-tree", "-r", "--name-only", producer_commit, "--", request_dir)).splitlines()
    relative = sorted(path[len(request_dir) + 1 :] for path in listed if path.startswith(request_dir + "/"))
    expected_files = sorted([*manifest["edit_parts"], "ready.json"])
    if relative != expected_files:
        raise ProducerProtocolError("producer request directory contains unexpected files")

    ready_mode = _text(_git(repo, "ls-tree", producer_commit, "--", ready_path)).split()[0]
    if ready_mode != "100644":
        raise ProducerProtocolError("producer ready.json must be a normal non-executable file")


def _load_edits(repo: Path, producer_commit: str, manifest: dict[str, Any]) -> list[dict[str, Any]]:
    request_dir = _request_dir(manifest["request_id"])
    edits: list[dict[str, Any]] = []
    total_bytes = 0
    for part in manifest["edit_parts"]:
        path = f"{request_dir}/{part}"
        mode = _text(_git(repo, "ls-tree", producer_commit, "--", path)).split()[0]
        if mode != "100644":
            raise ProducerProtocolError(f"{part} must be a normal non-executable file")
        raw = _git(repo, "show", f"{producer_commit}:{path}")
        if not 1 <= len(raw) <= MAX_EDIT_BYTES:
            raise ProducerProtocolError(f"{part} is outside the allowed size range")
        total_bytes += len(raw)
        if total_bytes > MAX_TOTAL_EDIT_BYTES:
            raise ProducerProtocolError("producer edit payload exceeds 4 MiB")
        try:
            edit = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ProducerProtocolError(f"{part} is invalid JSON: {exc}") from exc
        if not isinstance(edit, dict):
            raise ProducerProtocolError(f"{part} must contain one JSON object")
        edits.append(edit)
    return edits


def _apply_edit(workspace: Path, edit: dict[str, Any], part_name: str) -> str:
    operation = edit.get("operation")
    path_value = edit.get("path")
    if not isinstance(operation, str) or not isinstance(path_value, str):
        raise ProducerProtocolError(f"{part_name} requires string operation and path fields")
    path = _validate_product_path(path_value)
    target = workspace.joinpath(*PurePosixPath(path).parts)

    if operation == "replace_text":
        if set(edit) != {"operation", "path", "old", "new"} or not isinstance(edit.get("old"), str) or not isinstance(edit.get("new"), str):
            raise ProducerProtocolError(f"{part_name} has invalid replace_text fields")
        old = edit["old"]
        if not old:
            raise ProducerProtocolError(f"{part_name} replace_text old value must not be empty")
        if not target.is_file():
            raise ProducerProtocolError(f"{part_name} replace_text target does not exist: {path}")
        try:
            current = target.read_bytes().decode("utf-8")
        except UnicodeDecodeError as exc:
            raise ProducerProtocolError(f"{part_name} target is not UTF-8 text: {path}") from exc
        count = current.count(old)
        if count != 1:
            raise ProducerProtocolError(f"{part_name} replace_text expected exactly one match in {path}, found {count}")
        target.write_bytes(current.replace(old, edit["new"], 1).encode("utf-8"))
        return path

    if operation == "create_text":
        if set(edit) != {"operation", "path", "content"} or not isinstance(edit.get("content"), str):
            raise ProducerProtocolError(f"{part_name} has invalid create_text fields")
        if target.exists():
            raise ProducerProtocolError(f"{part_name} create_text target already exists: {path}")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(edit["content"].encode("utf-8"))
        return path

    if operation == "delete_file":
        if set(edit) != {"operation", "path", "expected_blob_sha"} or not isinstance(edit.get("expected_blob_sha"), str):
            raise ProducerProtocolError(f"{part_name} has invalid delete_file fields")
        expected_blob = _require_sha40("expected_blob_sha", edit["expected_blob_sha"])
        if not target.is_file():
            raise ProducerProtocolError(f"{part_name} delete_file target does not exist: {path}")
        actual_blob = _text(_git(workspace, "hash-object", "--", path)).lower()
        if actual_blob != expected_blob:
            raise ProducerProtocolError(
                f"{part_name} delete_file blob mismatch for {path}: observed {actual_blob}, expected {expected_blob}"
            )
        target.unlink()
        return path

    raise ProducerProtocolError(f"{part_name} has unsupported operation {operation!r}")


def build_producer_request(args: argparse.Namespace) -> dict[str, Any]:
    repo = Path(args.request_repo).resolve()
    output_dir = Path(args.output_dir).resolve()
    match = PRODUCER_BRANCH_RE.fullmatch(args.producer_branch)
    if not match:
        raise ProducerProtocolError("producer_branch must be chatgpt-producer-stage-<request-id>")
    request_id = match.group(1)
    if args.request_id != request_id:
        raise ProducerProtocolError("request_id must match producer_branch")
    producer_commit = _require_sha40("producer_commit", args.producer_commit)

    remote_producer = request_protocol._ls_remote(repo, args.remote, f"refs/heads/{args.producer_branch}")
    if remote_producer != producer_commit:
        raise ProducerProtocolError(
            f"producer branch moved: observed {remote_producer}, expected ready commit {producer_commit}"
        )
    _git(
        repo,
        "fetch",
        "--no-tags",
        args.remote,
        f"refs/heads/{args.producer_branch}:refs/remotes/{args.remote}/{args.producer_branch}",
    )

    manifest = _load_manifest(repo, producer_commit, request_id)
    expected = manifest["expected_head_sha"].lower()
    baseline = manifest["baseline_main_sha"].lower()

    remote_target = request_protocol._ls_remote(repo, args.remote, f"refs/heads/{manifest['target_branch']}")
    if remote_target != expected:
        raise ProducerProtocolError(f"target branch moved: observed {remote_target}, expected {expected}")
    remote_main = request_protocol._ls_remote(repo, args.remote, "refs/heads/main")
    if remote_main != baseline:
        raise ProducerProtocolError(f"main advanced: observed {remote_main}, producer baseline is {baseline}")

    _git(repo, "fetch", "--no-tags", args.remote, expected, baseline)
    ancestry = request_protocol._run(repo, "git", "merge-base", "--is-ancestor", baseline, expected, check=False)
    if ancestry.returncode != 0:
        raise ProducerProtocolError("target branch does not contain the declared current-main baseline")

    _validate_history(repo, baseline, producer_commit, manifest)
    edits = _load_edits(repo, producer_commit, manifest)

    with tempfile.TemporaryDirectory(prefix="gameengine-chatgpt-producer-") as temp_dir:
        workspace = Path(temp_dir) / "target"
        _git(repo, "worktree", "add", "--detach", "--force", str(workspace), expected)
        try:
            changed_paths: list[str] = []
            for part_name, edit in zip(manifest["edit_parts"], edits, strict=True):
                changed_paths.append(_apply_edit(workspace, edit, part_name))
            unique_paths = list(dict.fromkeys(changed_paths))
            _git(workspace, "add", "-A", "--", *unique_paths)

            build_args = argparse.Namespace(
                workspace=str(workspace),
                target_branch=manifest["target_branch"],
                expected_head_sha=expected,
                baseline_main_sha=baseline,
                request_id=request_id,
                commit_message=manifest["commit_message"],
                pr_title=manifest["pr_title"],
                pr_body=manifest["pr_body"],
                pr_body_file=None,
                output_dir=str(output_dir),
                remote=args.remote,
                skip_remote_recheck=False,
            )
            try:
                request_manifest = request_protocol.build_request(build_args)
            except request_protocol.ProtocolError as exc:
                raise ProducerProtocolError(str(exc)) from exc
        finally:
            _git(repo, "worktree", "remove", "--force", str(workspace), check=False)

    return {
        "producer_branch": args.producer_branch,
        "producer_commit": producer_commit,
        "request": request_manifest,
        "edit_payload_sha256": hashlib.sha256(
            b"".join(_git(repo, "show", f"{producer_commit}:{_request_dir(request_id)}/{part}") for part in manifest["edit_parts"])
        ).hexdigest(),
    }


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    build = sub.add_parser("build", help="build a Dispatcher request from an immutable connector edit plan")
    build.add_argument("--request-repo", required=True)
    build.add_argument("--producer-branch", required=True)
    build.add_argument("--producer-commit", required=True)
    build.add_argument("--request-id", required=True)
    build.add_argument("--output-dir", required=True)
    build.add_argument("--remote", default="origin")
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        result = build_producer_request(args)
        print(json.dumps(result, sort_keys=True))
        return 0
    except (ProducerProtocolError, OSError, subprocess.SubprocessError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
