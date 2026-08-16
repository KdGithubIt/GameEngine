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

    def create_producer(self, request_id: str, changed_path: str = "docs/sample.md") -> tuple[str, str]:
        branch = f"chatgpt-producer-stage-{request_id}"
        self.run("git", "checkout", "-B", branch, self.base)
        path = self.repo / changed_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("connector edit\n", encoding="utf-8")
        self.run("git", "add", changed_path)
        self.run("git", "commit", "-m", "Prepare connector product edit")

        ready_dir = self.repo / ".chatgpt-producer" / request_id
        ready_dir.mkdir(parents=True)
        ready = {
            "schema_version": 1,
            "request_id": request_id,
            "target_branch": "chatgpt/gameengine-test",
            "expected_head_sha": self.base,
            "baseline_main_sha": self.base,
            "commit_message": "Apply connector product edit",
            "pr_title": "Apply connector product edit",
            "pr_body": "Built by the trusted connector producer path.",
        }
        (ready_dir / "ready.json").write_text(json.dumps(ready, indent=2) + "\n", encoding="utf-8")
        self.run("git", "add", str(ready_dir / "ready.json"))
        self.run("git", "commit", "-m", "Mark connector producer ready")
        commit = self.text("git", "rev-parse", "HEAD")
        self.run("git", "push", "origin", branch)
        return branch, commit


class ProducerProtocolTests(unittest.TestCase):
    def test_connector_branch_builds_schema_v2_request(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            branch, commit = fixture.create_producer("connector-build")
            output = Path(temp) / "request"
            result = producer.build_producer_request(
                argparse.Namespace(
                    request_repo=str(fixture.repo),
                    producer_branch=branch,
                    producer_commit=commit,
                    request_id="connector-build",
                    output_dir=str(output),
                    remote="origin",
                )
            )
            manifest = result["request"]
            self.assertEqual(manifest["schema_version"], 2)
            self.assertEqual(manifest["expected_head_sha"], fixture.base)
            self.assertEqual(manifest["baseline_main_sha"], fixture.base)
            patch = b"".join((output / name).read_bytes() for name in manifest["patch_parts"])
            self.assertEqual(manifest["patch_bytes"], len(patch))
            self.assertEqual(manifest["patch_sha256"], producer.request_protocol._sha256(patch))
            self.assertIn(b"docs/sample.md", patch)

    def test_connector_branch_rejects_trust_boundary_product_edits(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            branch, commit = fixture.create_producer("connector-trust", ".github/blocked.yml")
            with self.assertRaisesRegex(producer.ProducerProtocolError, "forbidden"):
                producer.build_producer_request(
                    argparse.Namespace(
                        request_repo=str(fixture.repo),
                        producer_branch=branch,
                        producer_commit=commit,
                        request_id="connector-trust",
                        output_dir=str(Path(temp) / "request"),
                        remote="origin",
                    )
                )

    def test_connector_branch_must_stay_at_signaled_ready_commit(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            branch, commit = fixture.create_producer("connector-moved")
            (fixture.repo / "docs" / "later.md").write_text("later\n", encoding="utf-8")
            fixture.run("git", "add", "docs/later.md")
            fixture.run("git", "commit", "-m", "Move producer after ready")
            fixture.run("git", "push", "origin", branch)
            with self.assertRaisesRegex(producer.ProducerProtocolError, "producer branch moved"):
                producer.build_producer_request(
                    argparse.Namespace(
                        request_repo=str(fixture.repo),
                        producer_branch=branch,
                        producer_commit=commit,
                        request_id="connector-moved",
                        output_dir=str(Path(temp) / "request"),
                        remote="origin",
                    )
                )

    def test_producer_signal_is_read_only_and_trusted_producer_dispatches_publisher(self) -> None:
        root = Path(__file__).resolve().parents[2]
        signal = (root / ".github" / "workflows" / "gameengine-chatgpt-producer-stage-signal.yml").read_text(
            encoding="utf-8"
        )
        trusted = (root / ".github" / "workflows" / "gameengine-chatgpt-trusted-producer.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("permissions:\n  contents: read", signal)
        self.assertNotIn("contents: write", signal)
        self.assertIn("GameEngine ChatGPT Producer Stage Signal", trusted)
        self.assertIn("gh workflow run gameengine-chatgpt-transport-publisher.yml", trusted)
        self.assertNotIn("HEAD:refs/heads/chatgpt-dispatch\n", trusted)


if __name__ == "__main__":
    unittest.main(verbosity=2)
