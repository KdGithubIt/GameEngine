#!/usr/bin/env python3
"""Build protocol-v2 Dispatcher requests from connector-authored source-state data."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tempfile
import unicodedata
from typing import Any, Iterable

import request_protocol

PRODUCER_SCHEMA_VERSION = 1
PRODUCER_ROOT = ".chatgpt-producer"
PRODUCER_BRANCH_RE = re.compile(
    r"^chatgpt-producer-stage-([A-Za-z0-9][A-Za-z0-9._-]{0,63})$"
)
SHA256_RE = re.compile(r"^[0-9a-fA-F]{64}$")
MAX_MANIFEST_BYTES = 256 * 1024
MAX_READY_BYTES = 16 * 1024
MAX_FILES = 256
MAX_SOURCE_BYTES = 1024 * 1024
MAX_TOTAL_SOURCE_BYTES = 8 * 1024 * 1024
ALLOWED_FILE_MODES = {"100644", "100755"}
WINDOWS_RESERVED_NAMES = {
    "CON",
    "PRN",
    "AUX",
    "NUL",
    *(f"COM{index}" for index in range(1, 10)),
    *(f"LPT{index}" for index in range(1, 10)),
}


class ProducerProtocolError(RuntimeError):
    """Raised when connector producer input violates the trusted producer contract."""


def _run(
    cwd: Path,
    *args: str,
    input_bytes: bytes | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[bytes]:
    return request_protocol._run(cwd, *args, input_bytes=input_bytes, check=check)


def _git(cwd: Path, *args: str, check: bool = True) -> bytes:
    return request_protocol._git(cwd, *args, check=check)


def _text(data: bytes) -> str:
    return data.decode("utf-8", errors="strict").strip()


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _require_sha40(name: str, value: str) -> str:
    try:
        return request_protocol._require_sha40(name, value)
    except request_protocol.ProtocolError as exc:
        raise ProducerProtocolError(str(exc)) from exc


def _load_json_object(raw: bytes, label: str) -> dict[str, Any]:
    def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise ProducerProtocolError(f"{label} contains duplicate key {key!r}")
            result[key] = value
        return result

    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=reject_duplicate_keys)
    except UnicodeDecodeError as exc:
        raise ProducerProtocolError(f"{label} must be UTF-8 JSON") from exc
    except json.JSONDecodeError as exc:
        raise ProducerProtocolError(f"{label} is invalid JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise ProducerProtocolError(f"{label} must be a JSON object")
    return value


def _require_exact_keys(value: dict[str, Any], required: set[str], label: str) -> None:
    if set(value) != required:
        missing = sorted(required - set(value))
        extra = sorted(set(value) - required)
        raise ProducerProtocolError(
            f"{label} contains unexpected or missing fields (missing={missing}, extra={extra})"
        )


def _require_type(value: Any, expected: type, label: str) -> None:
    if type(value) is not expected:
        raise ProducerProtocolError(f"{label} has an invalid type")


def _request_dir(request_id: str) -> str:
    return f"{PRODUCER_ROOT}/{request_id}"


def _manifest_path(request_id: str) -> str:
    return f"{_request_dir(request_id)}/manifest.json"


def _ready_path(request_id: str) -> str:
    return f"{_request_dir(request_id)}/ready.json"


def _source_path(request_id: str, index: int) -> str:
    return f"{_request_dir(request_id)}/files/{index:04d}.source"


def _validate_product_path(path: str) -> None:
    if not path or path != unicodedata.normalize("NFC", path):
        raise ProducerProtocolError("product paths must be non-empty NFC text")
    if path.startswith("/") or path.endswith("/") or "\\" in path or "\x00" in path:
        raise ProducerProtocolError(f"unsafe product path: {path}")
    if any(ord(character) < 0x20 for character in path):
        raise ProducerProtocolError(f"product path contains a control character: {path}")

    parts = path.split("/")
    if any(part in {"", ".", ".."} for part in parts):
        raise ProducerProtocolError(f"product path traversal is forbidden: {path}")
    for part in parts:
        if ":" in part or part.endswith((" ", ".")):
            raise ProducerProtocolError(f"product path is not Windows-safe: {path}")
        stem = part.split(".", 1)[0].upper()
        if stem in WINDOWS_RESERVED_NAMES:
            raise ProducerProtocolError(f"product path uses a Windows-reserved name: {path}")

    if PurePosixPath(path).as_posix() != path:
        raise ProducerProtocolError(f"product path is not canonical: {path}")
    try:
        request_protocol._validate_product_path(path)
    except request_protocol.ProtocolError as exc:
        raise ProducerProtocolError(str(exc)) from exc


def _validate_text_fields(manifest: dict[str, Any]) -> None:
    commit_message = manifest["commit_message"]
    pr_title = manifest["pr_title"]
    pr_body = manifest["pr_body"]
    if not commit_message or len(commit_message) > 120 or "\n" in commit_message or "\r" in commit_message:
        raise ProducerProtocolError(
            "commit_message must be a non-empty single line of at most 120 characters"
        )
    if not pr_title or len(pr_title) > 200 or "\n" in pr_title or "\r" in pr_title:
        raise ProducerProtocolError(
            "pr_title must be a non-empty single line of at most 200 characters"
        )
    if len(pr_body) > 8000:
        raise ProducerProtocolError("pr_body is too long")
    if "<!-- gameengine-chatgpt-automation -->" in pr_body:
        raise ProducerProtocolError("legacy auto-merge authorization is forbidden")


def validate_ready(ready: dict[str, Any], request_id: str) -> None:
    required = {"schema_version", "request_id", "manifest_sha256", "manifest_bytes"}
    _require_exact_keys(ready, required, "producer ready.json")
    _require_type(ready["schema_version"], int, "producer ready.json schema_version")
    _require_type(ready["request_id"], str, "producer ready.json request_id")
    _require_type(ready["manifest_sha256"], str, "producer ready.json manifest_sha256")
    _require_type(ready["manifest_bytes"], int, "producer ready.json manifest_bytes")
    if ready["schema_version"] != PRODUCER_SCHEMA_VERSION:
        raise ProducerProtocolError(
            f"producer ready.json schema_version must be {PRODUCER_SCHEMA_VERSION}"
        )
    if ready["request_id"] != request_id:
        raise ProducerProtocolError("producer ready.json request_id must match its branch")
    if not SHA256_RE.fullmatch(ready["manifest_sha256"]):
        raise ProducerProtocolError("producer ready.json manifest_sha256 must be a SHA-256")
    if not 1 <= ready["manifest_bytes"] <= MAX_MANIFEST_BYTES:
        raise ProducerProtocolError("producer ready.json manifest_bytes is outside the allowed range")


def validate_manifest(manifest: dict[str, Any], request_id: str) -> list[dict[str, Any]]:
    required = {
        "schema_version",
        "request_id",
        "target_branch",
        "expected_head_sha",
        "baseline_main_sha",
        "source_format",
        "commit_message",
        "pr_title",
        "pr_body",
        "files",
    }
    _require_exact_keys(manifest, required, "producer manifest")
    scalar_types = {
        "schema_version": int,
        "request_id": str,
        "target_branch": str,
        "expected_head_sha": str,
        "baseline_main_sha": str,
        "source_format": str,
        "commit_message": str,
        "pr_title": str,
        "pr_body": str,
        "files": list,
    }
    for key, expected_type in scalar_types.items():
        _require_type(manifest[key], expected_type, f"producer manifest {key}")

    if manifest["schema_version"] != PRODUCER_SCHEMA_VERSION:
        raise ProducerProtocolError(
            f"producer manifest schema_version must be {PRODUCER_SCHEMA_VERSION}"
        )
    if manifest["request_id"] != request_id or not request_protocol.REQUEST_ID_RE.fullmatch(request_id):
        raise ProducerProtocolError("request_id must match the producer branch and manifest")
    try:
        request_protocol._require_safe_branch(manifest["target_branch"])
    except request_protocol.ProtocolError as exc:
        raise ProducerProtocolError(str(exc)) from exc
    _require_sha40("expected_head_sha", manifest["expected_head_sha"])
    _require_sha40("baseline_main_sha", manifest["baseline_main_sha"])
    if manifest["source_format"] != "utf8-lf":
        raise ProducerProtocolError("producer source_format must be 'utf8-lf'")
    _validate_text_fields(manifest)

    files = manifest["files"]
    if not 1 <= len(files) <= MAX_FILES:
        raise ProducerProtocolError(f"producer manifest files must contain 1 to {MAX_FILES} entries")

    required_file_keys = {
        "path",
        "operation",
        "base_mode",
        "mode",
        "source_sha256",
        "source_bytes",
    }
    normalized_keys: set[str] = set()
    paths: list[str] = []
    for index, entry in enumerate(files):
        if not isinstance(entry, dict):
            raise ProducerProtocolError(f"producer manifest files[{index}] must be an object")
        _require_exact_keys(entry, required_file_keys, f"producer manifest files[{index}]")
        _require_type(entry["path"], str, f"producer manifest files[{index}].path")
        _require_type(entry["operation"], str, f"producer manifest files[{index}].operation")
        _require_type(entry["source_bytes"], int, f"producer manifest files[{index}].source_bytes")
        if entry["base_mode"] is not None and type(entry["base_mode"]) is not str:
            raise ProducerProtocolError(f"producer manifest files[{index}].base_mode has an invalid type")
        if entry["mode"] is not None and type(entry["mode"]) is not str:
            raise ProducerProtocolError(f"producer manifest files[{index}].mode has an invalid type")
        if entry["source_sha256"] is not None and type(entry["source_sha256"]) is not str:
            raise ProducerProtocolError(
                f"producer manifest files[{index}].source_sha256 has an invalid type"
            )

        path = entry["path"]
        _validate_product_path(path)
        normalized_key = unicodedata.normalize("NFC", path).casefold()
        if normalized_key in normalized_keys:
            raise ProducerProtocolError(f"duplicate or case-colliding product path: {path}")
        normalized_keys.add(normalized_key)
        paths.append(path)

        operation = entry["operation"]
        if operation not in {"add", "update", "delete"}:
            raise ProducerProtocolError(f"unsupported producer operation {operation!r}")
        if operation == "add":
            if entry["base_mode"] is not None or entry["mode"] not in ALLOWED_FILE_MODES:
                raise ProducerProtocolError("add requires base_mode=null and a normal file mode")
        elif operation == "update":
            if entry["base_mode"] not in ALLOWED_FILE_MODES or entry["mode"] not in ALLOWED_FILE_MODES:
                raise ProducerProtocolError("update requires normal base_mode and mode values")
        else:
            if entry["base_mode"] not in ALLOWED_FILE_MODES or entry["mode"] is not None:
                raise ProducerProtocolError("delete requires a normal base_mode and mode=null")

        if operation == "delete":
            if entry["source_sha256"] is not None or entry["source_bytes"] != 0:
                raise ProducerProtocolError("delete entries must not declare source content")
        else:
            if not isinstance(entry["source_sha256"], str) or not SHA256_RE.fullmatch(
                entry["source_sha256"]
            ):
                raise ProducerProtocolError("add/update entries require source_sha256")
            if not 0 <= entry["source_bytes"] <= MAX_SOURCE_BYTES:
                raise ProducerProtocolError("source_bytes is outside the per-file limit")

    if paths != sorted(paths):
        raise ProducerProtocolError("producer manifest files must be sorted by path")
    for previous, current in zip(paths, paths[1:]):
        if current.startswith(previous + "/"):
            raise ProducerProtocolError(
                f"product paths may not overlap as file and directory: {previous}, {current}"
            )
    return files


def _changed_entries(repo: Path, commit: str) -> list[tuple[str, str]]:
    raw = _git(
        repo,
        "diff-tree",
        "--no-commit-id",
        "--name-status",
        "-r",
        "--no-renames",
        "-z",
        commit,
    )
    tokens = [token.decode("utf-8") for token in raw.split(b"\0") if token]
    if len(tokens) % 2 != 0:
        raise ProducerProtocolError("could not parse producer commit changes")
    return list(zip(tokens[0::2], tokens[1::2]))


def _single_parent(repo: Path, commit: str) -> str:
    fields = _text(_git(repo, "rev-list", "--parents", "-n", "1", commit)).split()
    if len(fields) != 2:
        raise ProducerProtocolError("producer commits must be linear non-merge commits")
    return fields[1]


def _validate_history(
    repo: Path,
    baseline: str,
    producer_commit: str,
    request_id: str,
) -> None:
    ancestry = _run(repo, "git", "merge-base", "--is-ancestor", baseline, producer_commit, check=False)
    if ancestry.returncode != 0:
        raise ProducerProtocolError("producer branch is not based on baseline_main_sha")
    commits = _text(_git(repo, "rev-list", "--reverse", f"{baseline}..{producer_commit}")).splitlines()
    if len(commits) < 2 or commits[-1] != producer_commit:
        raise ProducerProtocolError(
            "producer branch must contain data additions followed by one final ready commit"
        )

    request_dir = _request_dir(request_id)
    ready_path = _ready_path(request_id)
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
            if status != "A":
                raise ProducerProtocolError("producer payload commits may only add immutable input data")
            if path == _manifest_path(request_id):
                continue
            if re.fullmatch(
                rf"{re.escape(request_dir)}/files/[0-9]{{4}}\.source",
                path,
            ):
                continue
            raise ProducerProtocolError(
                f"producer payload path is outside the request data directory: {path}"
            )


def _tree_mode_and_type(repo: Path, commit: str, path: str) -> tuple[str, str] | None:
    raw = _git(repo, "ls-tree", "-z", commit, "--", path)
    if not raw:
        return None
    records = [record for record in raw.split(b"\0") if record]
    if len(records) != 1:
        raise ProducerProtocolError(f"could not resolve unique target path: {path}")
    meta, resolved_path = records[0].split(b"\t", 1)
    if resolved_path.decode("utf-8") != path:
        raise ProducerProtocolError(f"target path resolution changed unexpectedly: {path}")
    mode, object_type, _object_id = meta.decode("ascii").split()
    return mode, object_type


def _validate_parent_chain(repo: Path, commit: str, path: str) -> None:
    parts = path.split("/")
    for end in range(1, len(parts)):
        parent = "/".join(parts[:end])
        entry = _tree_mode_and_type(repo, commit, parent)
        if entry is None:
            continue
        mode, object_type = entry
        if mode == "120000" or object_type == "blob":
            raise ProducerProtocolError(f"product path traverses a non-directory target entry: {parent}")
        if mode == "160000" or object_type == "commit":
            raise ProducerProtocolError(f"product path traverses a submodule: {parent}")


def _load_input(
    repo: Path,
    producer_commit: str,
    request_id: str,
) -> tuple[dict[str, Any], list[tuple[dict[str, Any], bytes | None]]]:
    ready_path = _ready_path(request_id)
    ready_raw = _git(repo, "show", f"{producer_commit}:{ready_path}")
    if not 1 <= len(ready_raw) <= MAX_READY_BYTES:
        raise ProducerProtocolError("producer ready.json is outside the allowed size range")
    ready = _load_json_object(ready_raw, "producer ready.json")
    validate_ready(ready, request_id)

    manifest_path = _manifest_path(request_id)
    manifest_raw = _git(repo, "show", f"{producer_commit}:{manifest_path}")
    if len(manifest_raw) != ready["manifest_bytes"]:
        raise ProducerProtocolError("producer manifest_bytes does not match manifest.json")
    if _sha256(manifest_raw) != ready["manifest_sha256"].lower():
        raise ProducerProtocolError("producer manifest_sha256 does not match manifest.json")
    manifest = _load_json_object(manifest_raw, "producer manifest")
    files = validate_manifest(manifest, request_id)

    expected_tree_files = ["manifest.json", "ready.json"]
    total_source_bytes = 0
    loaded: list[tuple[dict[str, Any], bytes | None]] = []
    for index, entry in enumerate(files):
        if entry["operation"] == "delete":
            loaded.append((entry, None))
            continue
        source_name = f"files/{index:04d}.source"
        expected_tree_files.append(source_name)
        source_path = _source_path(request_id, index)
        try:
            source = _git(repo, "show", f"{producer_commit}:{source_path}")
        except request_protocol.ProtocolError as exc:
            raise ProducerProtocolError(f"missing producer source file: {source_name}") from exc
        if len(source) != entry["source_bytes"]:
            raise ProducerProtocolError(f"source_bytes does not match {source_name}")
        if _sha256(source) != entry["source_sha256"].lower():
            raise ProducerProtocolError(f"source_sha256 does not match {source_name}")
        if b"\x00" in source:
            raise ProducerProtocolError(f"binary/NUL source content is not supported: {source_name}")
        if b"\r" in source:
            raise ProducerProtocolError(f"source content must use LF line endings: {source_name}")
        try:
            source.decode("utf-8", errors="strict")
        except UnicodeDecodeError as exc:
            raise ProducerProtocolError(f"source content must be UTF-8: {source_name}") from exc
        total_source_bytes += len(source)
        if total_source_bytes > MAX_TOTAL_SOURCE_BYTES:
            raise ProducerProtocolError("producer source content exceeds the total size limit")
        loaded.append((entry, source))

    request_dir = _request_dir(request_id)
    tree_files = _text(
        _git(repo, "ls-tree", "-r", "--name-only", producer_commit, "--", request_dir)
    ).splitlines()
    relative = sorted(
        path[len(request_dir) + 1 :] for path in tree_files if path.startswith(request_dir + "/")
    )
    if relative != sorted(expected_tree_files):
        raise ProducerProtocolError("producer request directory contains missing or extra files")

    for relative_path in expected_tree_files:
        full_path = f"{request_dir}/{relative_path}"
        mode_line = _text(_git(repo, "ls-tree", producer_commit, "--", full_path))
        if not mode_line.startswith("100644 blob "):
            raise ProducerProtocolError(f"producer input must be a normal non-executable file: {relative_path}")

    return manifest, loaded


def _materialize_product_state(
    repo: Path,
    workspace: Path,
    expected: str,
    loaded: list[tuple[dict[str, Any], bytes | None]],
) -> None:
    for entry, source in loaded:
        path = entry["path"]
        _validate_parent_chain(repo, expected, path)
        target_entry = _tree_mode_and_type(repo, expected, path)
        operation = entry["operation"]

        if operation == "add":
            if target_entry is not None:
                raise ProducerProtocolError(f"add path already exists at expected target: {path}")
        else:
            if target_entry is None:
                raise ProducerProtocolError(f"{operation} path is missing at expected target: {path}")
            actual_mode, object_type = target_entry
            if object_type != "blob" or actual_mode not in ALLOWED_FILE_MODES:
                raise ProducerProtocolError(f"{operation} path is not a normal file: {path}")
            if actual_mode != entry["base_mode"]:
                raise ProducerProtocolError(
                    f"base_mode mismatch for {path}: observed {actual_mode}, declared {entry['base_mode']}"
                )

        destination = workspace / path
        if operation == "delete":
            destination.unlink()
            _git(workspace, "add", "-u", "--", path)
            continue

        assert source is not None
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(source)
        destination.chmod(0o755 if entry["mode"] == "100755" else 0o644)
        _git(workspace, "add", "--", path)
        staged = _text(_git(workspace, "ls-files", "--stage", "--", path))
        if not staged.startswith(entry["mode"] + " "):
            raise ProducerProtocolError(f"could not stage requested file mode for {path}")

    staged_raw = _git(workspace, "diff", "--cached", "--name-only", "--no-renames", "-z")
    staged_paths = sorted(item.decode("utf-8") for item in staged_raw.split(b"\0") if item)
    declared_paths = sorted(entry["path"] for entry, _source in loaded)
    if staged_paths != declared_paths:
        raise ProducerProtocolError("staged product paths do not exactly match the producer manifest")


def build_producer_request(args: argparse.Namespace) -> dict[str, Any]:
    repo = Path(args.request_repo).resolve()
    output_dir = Path(args.output_dir).resolve()
    match = PRODUCER_BRANCH_RE.fullmatch(args.producer_branch)
    if not match:
        raise ProducerProtocolError("producer_branch must be chatgpt-producer-stage-<request-id>")
    request_id = match.group(1)
    if args.request_id != request_id or not request_protocol.REQUEST_ID_RE.fullmatch(request_id):
        raise ProducerProtocolError("request_id must match producer_branch")
    producer_commit = _require_sha40("producer_commit", args.producer_commit)

    remote_producer = request_protocol._ls_remote(
        repo, args.remote, f"refs/heads/{args.producer_branch}"
    )
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

    manifest, loaded = _load_input(repo, producer_commit, request_id)
    expected = manifest["expected_head_sha"].lower()
    baseline = manifest["baseline_main_sha"].lower()

    remote_main = request_protocol._ls_remote(repo, args.remote, "refs/heads/main")
    if remote_main != baseline:
        raise ProducerProtocolError(f"main advanced: observed {remote_main}, producer baseline is {baseline}")
    _git(repo, "fetch", "--no-tags", args.remote, expected, baseline)
    _validate_history(repo, baseline, producer_commit, request_id)

    remote_target = request_protocol._ls_remote(
        repo, args.remote, f"refs/heads/{manifest['target_branch']}"
    )
    if remote_target != expected:
        raise ProducerProtocolError(f"target branch moved: observed {remote_target}, expected {expected}")
    ancestry = _run(repo, "git", "merge-base", "--is-ancestor", baseline, expected, check=False)
    if ancestry.returncode != 0:
        raise ProducerProtocolError("target branch does not contain the declared current-main baseline")

    with tempfile.TemporaryDirectory(prefix="gameengine-chatgpt-producer-") as temp_dir:
        workspace = Path(temp_dir) / "target"
        _git(repo, "worktree", "add", "--detach", "--force", str(workspace), expected)
        try:
            _materialize_product_state(repo, workspace, expected, loaded)
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
    }


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    build = sub.add_parser(
        "build", help="build a Dispatcher request from immutable connector source-state data"
    )
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
    except (
        ProducerProtocolError,
        request_protocol.ProtocolError,
        OSError,
        subprocess.SubprocessError,
    ) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
