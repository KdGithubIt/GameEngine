#!/usr/bin/env python3
"""Verify an already-applied Dispatcher request before post-push reconciliation.

This helper never writes the product target. It proves that the current target
HEAD is exactly the one-parent commit produced by a published schema-v2 request,
so trusted automation can resume Draft PR and Windows Validation reconciliation
after the normal Dispatcher already pushed the product commit.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys
from typing import Any

from request_protocol import (
    ProtocolError,
    _git,
    _load_stage_request,
    _ls_remote,
    _require_sha40,
    _sha256,
    preflight_patch,
)

READY_PATH_RE = re.compile(
    r"^\.chatgpt-requests/([A-Za-z0-9][A-Za-z0-9._-]{0,63})/ready\.json$"
)


def _text(data: bytes) -> str:
    return data.decode("utf-8", errors="strict").strip()


def inspect_published_request(
    request_repo: Path,
    request_commit: str,
) -> tuple[dict[str, Any], bytes, str]:
    """Validate one published ready commit and reconstruct its exact patch."""
    commit = _require_sha40("request_commit", request_commit)
    _git(request_repo, "cat-file", "-e", f"{commit}^{{commit}}")
    reachable = _git(
        request_repo,
        "merge-base",
        "--is-ancestor",
        commit,
        "HEAD",
        check=False,
    )
    # `_git(..., check=False)` returns bytes only, so use rev-list for a stable
    # ancestry assertion that raises on failure through the normal helper.
    if not _text(_git(request_repo, "rev-list", "HEAD")).splitlines().count(commit):
        raise ProtocolError("request_commit is not reachable from chatgpt-dispatch")

    changed = [
        item
        for item in _text(
            _git(request_repo, "diff-tree", "--no-commit-id", "--name-only", "-r", commit)
        ).splitlines()
        if item
    ]
    if len(changed) != 1:
        raise ProtocolError("published request commit must change exactly one file")
    ready_path = changed[0]
    match = READY_PATH_RE.fullmatch(ready_path)
    if match is None:
        raise ProtocolError("published request commit must add one request ready.json")
    request_id = match.group(1)

    status = _text(
        _git(request_repo, "diff-tree", "--no-commit-id", "--name-status", "-r", commit)
    )
    if status != f"A\t{ready_path}":
        raise ProtocolError("published ready.json must be newly added by request_commit")

    mode_line = _text(_git(request_repo, "ls-tree", commit, "--", ready_path))
    if not mode_line.startswith("100644 "):
        raise ProtocolError("published ready.json must be a normal non-executable file")

    manifest, patch = _load_stage_request(request_repo, commit, request_id)
    if manifest["schema_version"] != 2:
        raise ProtocolError("post-push recovery requires a schema-v2 request")
    if len(patch) != manifest["patch_bytes"]:
        raise ProtocolError("schema v2 patch_bytes does not match reconstructed patch")
    if _sha256(patch) != manifest["patch_sha256"].lower():
        raise ProtocolError("schema v2 patch_sha256 does not match reconstructed patch")
    return manifest, patch, ready_path


def verify_already_applied_commit(
    target_repo: Path,
    manifest: dict[str, Any],
    patch: bytes,
    *,
    remote: str = "origin",
) -> str:
    """Return the exact already-applied target HEAD or raise [`ProtocolError`]."""
    expected = manifest["expected_head_sha"].lower()
    baseline = manifest["baseline_main_sha"].lower()
    target_branch = manifest["target_branch"]
    candidate = _text(_git(target_repo, "rev-parse", "HEAD")).lower()
    if candidate == expected:
        raise ProtocolError("target still equals expected_head_sha; normal Dispatcher apply is required")

    parent_line = _text(_git(target_repo, "rev-list", "--parents", "-n", "1", candidate))
    parent_fields = parent_line.split()
    if len(parent_fields) != 2 or parent_fields[1].lower() != expected:
        raise ProtocolError(
            "current target is not a one-parent child of the request expected_head_sha"
        )

    commit_message = _text(_git(target_repo, "show", "-s", "--format=%B", candidate))
    if commit_message != manifest["commit_message"]:
        raise ProtocolError("already-applied candidate commit message does not match request")

    preflight_patch(target_repo, expected, patch)
    candidate_patch = _git(
        target_repo,
        "diff",
        "--binary",
        "--full-index",
        "--no-ext-diff",
        expected,
        candidate,
        "--",
    )
    if candidate_patch != patch:
        raise ProtocolError("already-applied candidate diff bytes do not match published request")

    _git(target_repo, "cat-file", "-e", f"{baseline}^{{commit}}")
    if _git(
        target_repo,
        "merge-base",
        "--is-ancestor",
        baseline,
        expected,
        check=False,
    ) != b"":
        # `merge-base --is-ancestor` has no stdout; a nonzero exit is otherwise
        # hidden by the byte-only helper when `check=False`. Verify explicitly
        # with `merge-base` equality below instead.
        pass
    merge_base = _text(_git(target_repo, "merge-base", baseline, expected)).lower()
    if merge_base != baseline:
        raise ProtocolError("expected_head_sha does not contain baseline_main_sha")

    main_head = _ls_remote(target_repo, remote, "refs/heads/main")
    _git(target_repo, "fetch", "--no-tags", remote, main_head)
    main_merge_base = _text(_git(target_repo, "merge-base", baseline, main_head)).lower()
    if main_merge_base != baseline:
        raise ProtocolError("current main no longer contains the request baseline")

    remote_target = _ls_remote(target_repo, remote, f"refs/heads/{target_branch}")
    if remote_target != candidate:
        raise ProtocolError(
            f"target moved during recovery verification: observed {remote_target}, expected {candidate}"
        )
    return candidate


def inspect_command(args: argparse.Namespace) -> dict[str, Any]:
    request_repo = Path(args.request_repo).resolve()
    manifest, patch, ready_path = inspect_published_request(request_repo, args.request_commit)
    return {
        "request_commit": args.request_commit.lower(),
        "request_id": manifest["request_id"],
        "ready_path": ready_path,
        "target_branch": manifest["target_branch"],
        "expected_head_sha": manifest["expected_head_sha"].lower(),
        "baseline_main_sha": manifest["baseline_main_sha"].lower(),
        "patch_sha256": _sha256(patch),
        "patch_bytes": len(patch),
    }


def verify_command(args: argparse.Namespace) -> dict[str, Any]:
    request_repo = Path(args.request_repo).resolve()
    target_repo = Path(args.target_repo).resolve()
    manifest, patch, ready_path = inspect_published_request(request_repo, args.request_commit)
    candidate = verify_already_applied_commit(target_repo, manifest, patch, remote=args.remote)
    return {
        "request_commit": args.request_commit.lower(),
        "request_id": manifest["request_id"],
        "ready_path": ready_path,
        "target_branch": manifest["target_branch"],
        "expected_head_sha": manifest["expected_head_sha"].lower(),
        "already_applied_head_sha": candidate,
        "patch_sha256": _sha256(patch),
        "patch_bytes": len(patch),
    }


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    inspect = sub.add_parser("inspect", help="validate and inspect a published recovery request")
    inspect.add_argument("--request-repo", required=True)
    inspect.add_argument("--request-commit", required=True)

    verify = sub.add_parser(
        "verify-applied",
        help="prove that current target HEAD exactly equals the already-applied request",
    )
    verify.add_argument("--request-repo", required=True)
    verify.add_argument("--request-commit", required=True)
    verify.add_argument("--target-repo", required=True)
    verify.add_argument("--remote", default="origin")
    return parser


def main() -> int:
    args = _parser().parse_args()
    try:
        if args.command == "inspect":
            result = inspect_command(args)
        else:
            result = verify_command(args)
    except ProtocolError as exc:
        print(f"dispatcher recovery rejected: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
