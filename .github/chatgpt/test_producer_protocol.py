#!/usr/bin/env python3
"""Regression coverage for the connector-only trusted producer path."""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

MODULE_PATH = Path(__file__).with_name("producer_protocol.py")
spec = importlib.util.spec_from_file_location("producer_protocol", MODULE_PATH)
assert spec and spec.loader
producer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(producer)


class ProducerFixture:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.repo = root / "repo"
        self.remote = root / "remote.git"
        self.repo.mkdir()
        self.run("git", "init", "-b", "main")
        self.run("git", "config", "user.name", "Producer Test")
        self.run("git", "config", "user.email", "producer@example.invalid")
        (self.repo / "docs").mkdir()
        (self.repo / "docs" / "sample.md").write_text("base\n", encoding="utf-8")
        self.run("git", "add", "docs/sample.md")
        self.run("git", "commit", "-m", "base")
        self.base = self.text("git", "rev-parse", "HEAD")
        subprocess.run(
            ["git", "init", "--bare", str(self.remote)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.run("git", "remote", "add", "origin", str(self.remote))
        self.run("git", "push", "origin", "main")
        self.run("git", "branch", "chatgpt/gameengine-test")
        self.run("git", "push", "origin", "chatgpt/gameengine-test")

    def run(self, *args: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run(args, cwd=self.repo, check=check, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    def text(self, *args: str) -> str:
        return self.run(*args).stdout.decode().strip()

    def create_producer(self, request_id: str, edits: list[dict[str, object]]) -> tuple[str, str]:
        branch = f"chatgpt-producer-stage-{request_id}"
        self.run("git", "checkout", "-B", branch, self.base)
        request_dir = self.repo / ".chatgpt-producer" / request_id
        request_dir.mkdir(parents=True)
        edit_parts: list[str] = []
        for index, edit in enumerate(edits):
            name = f"edit-{index:04d}.json"
            (request_dir / name).write_text(json.dumps(edit, indent=2) + "\n", encoding="utf-8")
            edit_parts.append(name)
        self.run("git", "add", str(request_dir))
        self.run("git", "commit", "-m", "Add connector edit plan")

        ready = {
            "schema_version": 1,
            "request_id": request_id,
            "target_branch": "chatgpt/gameengine-test",
            "expected_head_sha": self.base,
            "baseline_main_sha": self.base,
            "edit_parts": edit_parts,
            "commit_message": "Apply connector product edit",
            "pr_title": "Apply connector product edit",
            "pr_body": "Built by the trusted connector producer path.",
        }
        (request_dir / "ready.json").write_text(json.dumps(ready, indent=2) + "\n", encoding="utf-8")
        self.run("git", "add", str(request_dir / "ready.json"))
        self.run("git", "commit", "-m", "Mark connector producer ready")
        commit = self.text("git", "rev-parse", "HEAD")
        self.run("git", "push", "origin", branch)
        return branch, commit


class ProducerProtocolTests(unittest.TestCase):
    def build(self, fixture: ProducerFixture, request_id: str, branch: str, commit: str, output: Path):
        return producer.build_producer_request(
            argparse.Namespace(
                request_repo=str(fixture.repo),
                producer_branch=branch,
                producer_commit=commit,
                request_id=request_id,
                output_dir=str(output),
                remote="origin",
            )
        )

    def test_connector_edit_plan_builds_schema_v2_request(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            branch, commit = fixture.create_producer(
                "connector-build",
                [{"operation": "replace_text", "path": "docs/sample.md", "old": "base\n", "new": "connector edit\n"}],
            )
            output = Path(temp) / "request"
            result = self.build(fixture, "connector-build", branch, commit, output)
            manifest = result["request"]
            self.assertEqual(manifest["schema_version"], 2)
            self.assertEqual(manifest["expected_head_sha"], fixture.base)
            self.assertEqual(manifest["baseline_main_sha"], fixture.base)
            patch = b"".join((output / name).read_bytes() for name in manifest["patch_parts"])
            self.assertEqual(manifest["patch_bytes"], len(patch))
            self.assertEqual(manifest["patch_sha256"], producer.request_protocol._sha256(patch))
            self.assertIn(b"docs/sample.md", patch)
            self.assertIn(b"+connector edit", patch)

    def test_connector_edit_plan_supports_create_and_delete(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source_blob = fixture.text("git", "hash-object", "docs/sample.md")
            branch, commit = fixture.create_producer(
                "connector-create-delete",
                [
                    {"operation": "create_text", "path": "docs/new.md", "content": "new\n"},
                    {"operation": "delete_file", "path": "docs/sample.md", "expected_blob_sha": source_blob},
                ],
            )
            output = Path(temp) / "request"
            result = self.build(fixture, "connector-create-delete", branch, commit, output)
            patch = b"".join((output / name).read_bytes() for name in result["request"]["patch_parts"])
            self.assertIn(b"docs/new.md", patch)
            self.assertIn(b"docs/sample.md", patch)

    def test_connector_edit_plan_rejects_trust_boundary_path(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            branch, commit = fixture.create_producer(
                "connector-trust",
                [{"operation": "create_text", "path": ".github/blocked.yml", "content": "blocked\n"}],
            )
            with self.assertRaisesRegex(producer.ProducerProtocolError, "forbidden"):
                self.build(fixture, "connector-trust", branch, commit, Path(temp) / "request")

    def test_connector_edit_plan_rejects_path_traversal(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            branch, commit = fixture.create_producer(
                "connector-traversal",
                [{"operation": "create_text", "path": "docs/../../.github/blocked.yml", "content": "blocked\n"}],
            )
            with self.assertRaisesRegex(producer.ProducerProtocolError, "traversal"):
                self.build(fixture, "connector-traversal", branch, commit, Path(temp) / "request")

    def test_replace_text_requires_one_exact_match(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            branch, commit = fixture.create_producer(
                "connector-context",
                [{"operation": "replace_text", "path": "docs/sample.md", "old": "missing", "new": "changed"}],
            )
            with self.assertRaisesRegex(producer.ProducerProtocolError, "exactly one match"):
                self.build(fixture, "connector-context", branch, commit, Path(temp) / "request")

    def test_connector_branch_must_stay_at_signaled_ready_commit(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            branch, commit = fixture.create_producer(
                "connector-moved",
                [{"operation": "replace_text", "path": "docs/sample.md", "old": "base\n", "new": "changed\n"}],
            )
            (fixture.repo / "later.txt").write_text("later\n", encoding="utf-8")
            fixture.run("git", "add", "later.txt")
            fixture.run("git", "commit", "-m", "Move producer after ready")
            fixture.run("git", "push", "origin", branch)
            with self.assertRaisesRegex(producer.ProducerProtocolError, "producer branch moved"):
                self.build(fixture, "connector-moved", branch, commit, Path(temp) / "request")

    def test_trusted_producer_uses_default_branch_issue_signal(self) -> None:
        root = Path(__file__).resolve().parents[2]
        trusted = (root / ".github" / "workflows" / "gameengine-chatgpt-trusted-producer.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("issues:\n    types:\n      - opened", trusted)
        self.assertIn("GameEngine ChatGPT Producer: ", trusted)
        self.assertIn("ISSUE_ASSOCIATION", trusted)
        self.assertIn("OWNER|MEMBER|COLLABORATOR", trusted)
        self.assertIn("gameengine-chatgpt-producer-v1", trusted)
        self.assertIn("gh workflow run gameengine-chatgpt-transport-publisher.yml", trusted)
        self.assertNotIn("HEAD:refs/heads/chatgpt-dispatch\n", trusted)


if __name__ == "__main__":
    unittest.main(verbosity=2)
