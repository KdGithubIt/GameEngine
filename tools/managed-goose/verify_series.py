#!/usr/bin/env python3
"""Validate the pinned managed-Goose patch series without network access."""

from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST_PATH = ROOT / "tools" / "managed-goose" / "series.json"
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
DIFF_HEADER = re.compile(r"^diff --git a/(.+) b/(.+)$", re.MULTILINE)


def fail(message: str) -> "NoReturn":
    raise SystemExit(f"managed-goose verification failed: {message}")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main() -> int:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    if manifest.get("schema_version") != 1:
        fail("unsupported schema_version")

    upstream = manifest.get("upstream", {})
    upstream_sha = upstream.get("sha", "")
    if not FULL_SHA.fullmatch(upstream_sha):
        fail("upstream.sha must be a full lowercase 40-character SHA")

    patches = manifest.get("patches")
    if not isinstance(patches, list) or not patches:
        fail("patches must be a non-empty list")

    seen_paths: set[str] = set()
    observed_paths: list[str] = []
    for index, entry in enumerate(patches, start=1):
        rel = entry.get("path", "")
        expected_hash = entry.get("sha256", "")
        if not rel or rel in seen_paths:
            fail(f"patch #{index} has a missing or duplicate path")
        seen_paths.add(rel)
        observed_paths.append(rel)

        patch_path = ROOT / rel
        if not patch_path.is_file():
            fail(f"missing patch: {rel}")
        observed_hash = sha256(patch_path)
        if observed_hash != expected_hash:
            fail(f"{rel}: sha256 {observed_hash} != manifest {expected_hash}")

        text = patch_path.read_text(encoding="utf-8")
        pairs = DIFF_HEADER.findall(text)
        if not pairs:
            fail(f"{rel}: no diff --git headers found")
        changed = []
        for left, right in pairs:
            if left != right:
                fail(f"{rel}: rename-style diff is not allowed ({left} -> {right})")
            changed.append(left)

        allowed = entry.get("allowed_paths")
        if sorted(set(changed)) != sorted(allowed or []):
            fail(
                f"{rel}: changed paths {sorted(set(changed))} do not match "
                f"allowed_paths {sorted(allowed or [])}"
            )
        if any(path.startswith(".github/") or path.startswith(".chatgpt-requests/") for path in changed):
            fail(f"{rel}: patch may not modify GameEngine automation transport paths")

        origin = entry.get("origin", {})
        origin_type = origin.get("type")
        if origin_type and origin_type.startswith("upstream"):
            commit = origin.get("commit", "")
            if not FULL_SHA.fullmatch(commit):
                fail(f"{rel}: upstream origin commit must be a full SHA")
            if commit not in text:
                fail(f"{rel}: patch header does not name origin commit {commit}")

        adapted_marker = f"Adapted-To: {upstream.get('version')} ({upstream_sha})"
        if adapted_marker not in text:
            fail(f"{rel}: missing exact upstream adaptation marker")

    if observed_paths != sorted(observed_paths):
        fail("patches must be ordered lexicographically by path")

    for excluded in manifest.get("excluded_upstream_commits", []):
        commit = excluded.get("commit", "")
        if not FULL_SHA.fullmatch(commit):
            fail("excluded upstream commit must be a full SHA")
        if not excluded.get("reason"):
            fail(f"excluded upstream commit {commit} is missing a reason")

    print(
        f"managed-goose series OK: {manifest['distribution_id']} "
        f"upstream={upstream['version']}@{upstream_sha} patches={len(patches)}"
    )
    for entry in patches:
        print(f"  {entry['sha256']}  {entry['path']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
