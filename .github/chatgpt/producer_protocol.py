#!/usr/bin/env python3
"""Validate connector-authored producer branches and build Dispatcher requests."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from typing import Any, Iterable

import request_protocol

PRODUCER_BRANCH_RE = re.compile(r"^chatgpt-producer-stage-([A-Za-z0-9][A-Za-z0-9._-]{0,63})$")
PRODUCER_SCHEMA_VERSION = 1
PRODUCER_ROOT = ".chatgpt-producer"


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


def _ready_path(request_id: str) -> str:
    return f"{PRODUCER_ROOT}/{request_id}/ready.json"


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


def _validate_product_history(repo: Path, expected: str, producer_commit: str, request_id: str) -> str:
    ancestry = request_protocol._run(repo, "git", "merge-base", "--is-ancestor", expected, producer_commit, check=False)
    if ancestry.returncode != 0:
        raise ProducerProtocolError("producer branch is not based on expected_head_sha")

    commits = _text(_git(repo, "rev-list", "--reverse", f"{expected}..{producer_commit}")).splitlines()
    if len(commits) < 2:
        raise ProducerProtocolError("producer branch must contain product edits followed by one ready commit")
    if commits[-1] != producer_commit:
        raise ProducerProtocolError("producer ready commit must be the branch head")

    ready_path = _ready_path(request_id)
    for index, commit in enumerate(commits):
        parent = _single_parent(repo, commit)
        entries = _changed_entries(repo, commit)
        if not entries:
            raise ProducerProtocolError("producer commits may not be empty")
        if index == len(commits) - 1:
            if entries != [("A", ready_path)]:
                raise ProducerProtocolError(f"final producer commit must add only {ready_path}")
            mode = _text(_git(repo, "ls-tree", producer_commit, "--", ready_path)).split()[0]
            if mode != "100644":
                raise ProducerProtocolError("producer ready.json must be a normal non-executable file")
            return parent

        for status, path in entries:
            if status not in {"A", "M", "D"}:
                raise ProducerProtocolError(f"unsupported producer change status {status!r} for {path}")
            try:
                request_protocol._validate_product_path(path)
            except request_protocol.ProtocolError as exc:
                raise ProducerProtocolError(str(exc)) from exc

    raise ProducerProtocolError("producer ready commit was not found")


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

    product_head = _validate_product_history(repo, expected, producer_commit, request_id)
    patch = _git(repo, "diff", "--binary", "--full-index", "--no-ext-diff", expected, product_head, "--")
    if not patch:
        raise ProducerProtocolError("producer branch contains no final product changes")

    with tempfile.TemporaryDirectory(prefix="gameengine-chatgpt-producer-") as temp_dir:
        workspace = Path(temp_dir) / "target"
        _git(repo, "worktree", "add", "--detach", "--force", str(workspace), expected)
        try:
            apply_result = request_protocol._run(
                workspace,
                "git",
                "apply",
                "--index",
                "--whitespace=nowarn",
                "-",
                input_bytes=patch,
                check=False,
            )
            if apply_result.returncode != 0:
                detail = apply_result.stderr.decode("utf-8", errors="replace").strip()
                raise ProducerProtocolError(f"could not stage producer product state on exact target: {detail}")

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
        "product_head_sha": product_head,
        "request": request_manifest,
    }


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    build = sub.add_parser("build", help="build a Dispatcher request from an immutable connector producer branch")
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
