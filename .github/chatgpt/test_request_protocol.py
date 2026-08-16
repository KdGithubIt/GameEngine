#!/usr/bin/env python3
"""Regression coverage for confirmed ChatGPT automation incidents."""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

MODULE_PATH = Path(__file__).with_name("request_protocol.py")
spec = importlib.util.spec_from_file_location("request_protocol", MODULE_PATH)
assert spec and spec.loader
protocol = importlib.util.module_from_spec(spec)
spec.loader.exec_module(protocol)


class RepoFixture:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.repo = root / "repo"
        self.remote = root / "remote.git"
        self.repo.mkdir()
        self.run("git", "init", "-b", "main")
        self.run("git", "config", "user.name", "Automation Test")
        self.run("git", "config", "user.email", "automation@example.invalid")
        (self.repo / "docs").mkdir()
        (self.repo / "docs" / "sample.md").write_text("base\n", encoding="utf-8")
        self.run("git", "add", "docs/sample.md")
        self.run("git", "commit", "-m", "base")
        self.base = self.text("git", "rev-parse", "HEAD")
        subprocess.run(["git", "init", "--bare", str(self.remote)], check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.run("git", "remote", "add", "origin", str(self.remote))
        self.run("git", "push", "origin", "main")
        self.run("git", "branch", "chatgpt/gameengine-test")
        self.run("git", "push", "origin", "chatgpt/gameengine-test")

    def run(self, *args: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run(args, cwd=self.repo, check=check, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    def text(self, *args: str) -> str:
        return self.run(*args).stdout.decode().strip()

    def make_request(self, request_id: str, patch: bytes, *, baseline: str | None = None, patch_hash: str | None = None) -> str:
        self.run("git", "checkout", "-B", f"chatgpt-dispatch-stage-{request_id}", self.base)
        request_dir = self.repo / ".chatgpt-requests" / request_id
        request_dir.mkdir(parents=True)
        (request_dir / "part-0000.patch").write_bytes(patch)
        self.run("git", "add", str(request_dir / "part-0000.patch"))
        self.run("git", "commit", "-m", "Add request patch")
        manifest = {
            "schema_version": 2,
            "request_id": request_id,
            "target_branch": "chatgpt/gameengine-test",
            "expected_head_sha": self.base,
            "baseline_main_sha": baseline or self.base,
            "patch_sha256": patch_hash or protocol._sha256(patch),
            "patch_bytes": len(patch),
            "patch_parts": ["part-0000.patch"],
            "commit_message": "Test request",
            "pr_title": "Test request",
            "pr_body": "Regression fixture",
        }
        (request_dir / "ready.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        self.run("git", "add", str(request_dir / "ready.json"))
        self.run("git", "commit", "-m", "Mark request ready")
        return self.text("git", "rev-parse", "HEAD")


GOOD_PATCH = b"""diff --git a/docs/sample.md b/docs/sample.md
index df967b9..5ea2ed4 100644
--- a/docs/sample.md
+++ b/docs/sample.md
@@ -1 +1 @@
-base
+changed
"""


class RequestProtocolRegressionTests(unittest.TestCase):
    def test_inc001_trailing_whitespace_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = RepoFixture(Path(temp))
            patch = GOOD_PATCH.replace(b"+changed\n", b"+changed \n")
            with self.assertRaisesRegex(protocol.ProtocolError, "whitespace"):
                protocol.preflight_patch(fixture.repo, fixture.base, patch)

    def test_inc002_corrupt_hunk_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = RepoFixture(Path(temp))
            patch = GOOD_PATCH.replace(b"@@ -1 +1 @@", b"@@ -1,2 +1,2 @@")
            with self.assertRaises(protocol.ProtocolError):
                protocol.preflight_patch(fixture.repo, fixture.base, patch)

    def test_inc003_schema_v2_hash_mismatch_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = RepoFixture(Path(temp))
            commit = fixture.make_request("inc003", GOOD_PATCH, patch_hash="0" * 64)
            with self.assertRaisesRegex(protocol.ProtocolError, "patch_sha256"):
                protocol.preflight_stage(fixture.repo, commit, "inc003")

    def test_inc004_advanced_main_baseline_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = RepoFixture(Path(temp))
            commit = fixture.make_request("inc004", GOOD_PATCH, baseline="0" * 40)
            with self.assertRaisesRegex(protocol.ProtocolError, "main advanced"):
                protocol.preflight_stage(fixture.repo, commit, "inc004")

    def test_inc005_checkout_prefix_path_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = RepoFixture(Path(temp))
            patch = GOOD_PATCH.replace(b"a/docs/sample.md", b"a/GameEngine/docs/sample.md").replace(
                b"b/docs/sample.md", b"b/GameEngine/docs/sample.md"
            )
            with self.assertRaises(protocol.ProtocolError):
                protocol.preflight_patch(fixture.repo, fixture.base, patch)

    def test_builder_generates_schema_v2_from_staged_changes(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = RepoFixture(Path(temp))
            fixture.run("git", "checkout", "chatgpt/gameengine-test")
            (fixture.repo / "docs" / "sample.md").write_text("builder\n", encoding="utf-8")
            fixture.run("git", "add", "docs/sample.md")
            output = Path(temp) / "request"
            args = argparse.Namespace(
                workspace=str(fixture.repo),
                target_branch="chatgpt/gameengine-test",
                expected_head_sha=fixture.base,
                baseline_main_sha=fixture.base,
                request_id="builder-test",
                commit_message="Builder test",
                pr_title="Builder test",
                pr_body="Builder regression fixture",
                pr_body_file=None,
                output_dir=str(output),
                remote="origin",
                skip_remote_recheck=True,
            )
            manifest = protocol.build_request(args)
            self.assertEqual(manifest["schema_version"], 2)
            patch = b"".join((output / name).read_bytes() for name in manifest["patch_parts"])
            self.assertEqual(manifest["patch_bytes"], len(patch))
            self.assertEqual(manifest["patch_sha256"], protocol._sha256(patch))

    def test_inc006_editor_capture_stays_off_eframe_helper(self) -> None:
        root = Path(__file__).resolve().parents[2]
        capture = (root / "scripts" / "ci" / "Invoke-EditorVisualValidation.ps1").read_text(encoding="utf-8")
        self.assertIn("GAMEENGINE_SCREENSHOT_TO", capture)
        self.assertNotIn("EFRAME_SCREENSHOT_TO", capture)

    def test_inc007_publisher_uses_verifying_rev_parse(self) -> None:
        root = Path(__file__).resolve().parents[2]
        publisher = (root / ".github" / "workflows" / "gameengine-chatgpt-transport-publisher.yml").read_text(encoding="utf-8")
        self.assertIn('git rev-parse --verify "$transport_head:$request_dir"', publisher)


if __name__ == "__main__":
    unittest.main(verbosity=2)
