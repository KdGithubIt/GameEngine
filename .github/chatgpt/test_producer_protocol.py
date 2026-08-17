#!/usr/bin/env python3
"""Regression coverage for the connector trusted producer path."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

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
        self.run("git", "config", "core.fileMode", "true")
        (self.repo / "docs").mkdir()
        (self.repo / "docs" / "update.md").write_text("old update\n", encoding="utf-8")
        (self.repo / "docs" / "delete.md").write_text("delete me\n", encoding="utf-8")
        self.run("git", "add", "docs")
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
        return subprocess.run(
            args,
            cwd=self.repo,
            check=check,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def text(self, *args: str) -> str:
        return self.run(*args).stdout.decode().strip()

    @staticmethod
    def entry(
        path: str,
        operation: str,
        *,
        source: bytes | None = None,
        base_mode: str | None = None,
        mode: str | None = None,
    ) -> dict[str, object]:
        if operation == "add":
            base_mode = None
            mode = mode or "100644"
        elif operation == "update":
            base_mode = base_mode or "100644"
            mode = mode or "100644"
        elif operation == "delete":
            base_mode = base_mode or "100644"
            mode = None
            source = None
        return {
            "path": path,
            "operation": operation,
            "base_mode": base_mode,
            "mode": mode,
            "source_sha256": hashlib.sha256(source).hexdigest() if source is not None else None,
            "source_bytes": len(source) if source is not None else 0,
        }

    def create_input(
        self,
        request_id: str,
        entries: list[dict[str, object]],
        *,
        sources: dict[int, bytes] | None = None,
        manifest_mutator=None,
        ready_mutator=None,
        omit_source_indexes: set[int] | None = None,
        extra_source: bool = False,
        ready_extra: bool = False,
    ) -> tuple[str, str]:
        branch = f"chatgpt-producer-stage-{request_id}"
        self.run("git", "checkout", "-B", branch, self.base)
        request_dir = self.repo / ".chatgpt-producer" / request_id
        files_dir = request_dir / "files"
        files_dir.mkdir(parents=True)
        ordered = sorted(entries, key=lambda entry: str(entry["path"]))
        manifest = {
            "schema_version": 1,
            "request_id": request_id,
            "target_branch": "chatgpt/gameengine-test",
            "expected_head_sha": self.base,
            "baseline_main_sha": self.base,
            "source_format": "utf8-lf",
            "commit_message": "Apply connector source state",
            "pr_title": "Apply connector source state",
            "pr_body": "Built by the trusted connector producer path.",
            "files": ordered,
        }
        if manifest_mutator:
            manifest_mutator(manifest)
        manifest_raw = (json.dumps(manifest, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
        (request_dir / "manifest.json").write_bytes(manifest_raw)

        source_map = sources or {}
        omitted = omit_source_indexes or set()
        for index, entry in enumerate(ordered):
            if entry.get("operation") == "delete" or index in omitted:
                continue
            source = source_map.get(index)
            if source is None:
                for candidate in source_map.values():
                    if hashlib.sha256(candidate).hexdigest() == entry.get("source_sha256"):
                        source = candidate
                        break
            if source is None:
                source = b""
            (files_dir / f"{index:04d}.source").write_bytes(source)
        if extra_source:
            (files_dir / "9999.source").write_text("extra\n", encoding="utf-8")

        self.run("git", "add", str(request_dir / "manifest.json"), str(files_dir))
        self.run("git", "commit", "-m", "Add connector producer source state")

        ready = {
            "schema_version": 1,
            "request_id": request_id,
            "manifest_sha256": hashlib.sha256(manifest_raw).hexdigest(),
            "manifest_bytes": len(manifest_raw),
        }
        if ready_mutator:
            ready_mutator(ready)
        (request_dir / "ready.json").write_text(
            json.dumps(ready, indent=2) + "\n", encoding="utf-8", newline="\n"
        )
        self.run("git", "add", str(request_dir / "ready.json"))
        if ready_extra:
            (request_dir / "unexpected.txt").write_text("unexpected\n", encoding="utf-8")
            self.run("git", "add", str(request_dir / "unexpected.txt"))
        self.run("git", "commit", "-m", "Mark connector producer ready")
        commit = self.text("git", "rev-parse", "HEAD")
        self.run("git", "push", "origin", branch)
        return branch, commit

    def build(self, request_id: str, branch: str, commit: str, output_name: str = "request"):
        output = self.root / output_name
        args = argparse.Namespace(
            request_repo=str(self.repo),
            producer_branch=branch,
            producer_commit=commit,
            request_id=request_id,
            output_dir=str(output),
            remote="origin",
        )
        return producer.build_producer_request(args), output

    def advance_remote(self, branch: str, path: str, content: str) -> str:
        self.run("git", "checkout", branch)
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
        self.run("git", "add", path)
        self.run("git", "commit", "-m", f"advance {branch}")
        sha = self.text("git", "rev-parse", "HEAD")
        self.run("git", "push", "origin", branch)
        return sha


class ProducerProtocolTests(unittest.TestCase):
    def test_normal_source_input_builds_schema_v2_request_for_add_update_delete(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            add = b"added\n"
            update = b"updated\n"
            entries = [
                fixture.entry("docs/add.md", "add", source=add),
                fixture.entry("docs/delete.md", "delete"),
                fixture.entry("docs/update.md", "update", source=update),
            ]
            branch, commit = fixture.create_input(
                "source-state", entries, sources={0: add, 2: update}
            )
            result, output = fixture.build("source-state", branch, commit)
            manifest = result["request"]
            self.assertEqual(manifest["schema_version"], 2)
            self.assertEqual(manifest["expected_head_sha"], fixture.base)
            self.assertEqual(manifest["baseline_main_sha"], fixture.base)
            patch = b"".join((output / part).read_bytes() for part in manifest["patch_parts"])
            self.assertEqual(manifest["patch_bytes"], len(patch))
            self.assertEqual(manifest["patch_sha256"], hashlib.sha256(patch).hexdigest())
            self.assertIn(b"new file mode 100644", patch)
            self.assertIn(b"deleted file mode 100644", patch)
            self.assertIn(b"+updated", patch)

    def test_stale_target_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"updated\n"
            entries = [fixture.entry("docs/update.md", "update", source=source)]
            branch, commit = fixture.create_input("stale-target", entries, sources={0: source})
            fixture.advance_remote("chatgpt/gameengine-test", "docs/other.md", "moved\n")
            with self.assertRaisesRegex(producer.ProducerProtocolError, "target branch moved"):
                fixture.build("stale-target", branch, commit)

    def test_stale_main_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"updated\n"
            entries = [fixture.entry("docs/update.md", "update", source=source)]
            branch, commit = fixture.create_input("stale-main", entries, sources={0: source})
            fixture.advance_remote("main", "docs/main.md", "advanced\n")
            with self.assertRaisesRegex(producer.ProducerProtocolError, "main advanced"):
                fixture.build("stale-main", branch, commit)

    def test_signal_commit_rejects_later_producer_branch_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"updated\n"
            entries = [fixture.entry("docs/update.md", "update", source=source)]
            branch, commit = fixture.create_input("mutated", entries, sources={0: source})
            extra = fixture.repo / ".chatgpt-producer" / "mutated" / "after-ready.txt"
            extra.write_text("changed later\n", encoding="utf-8")
            fixture.run("git", "add", str(extra))
            fixture.run("git", "commit", "-m", "mutate producer after signal")
            fixture.run("git", "push", "origin", branch)
            with self.assertRaisesRegex(producer.ProducerProtocolError, "producer branch moved"):
                fixture.build("mutated", branch, commit)

    def test_path_traversal_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"bad\n"
            entries = [fixture.entry("docs/../README.md", "add", source=source)]
            branch, commit = fixture.create_input("traversal", entries, sources={0: source})
            with self.assertRaisesRegex(producer.ProducerProtocolError, "traversal"):
                fixture.build("traversal", branch, commit)

    def test_github_path_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"bad\n"
            entries = [fixture.entry(".github/workflows/blocked.yml", "add", source=source)]
            branch, commit = fixture.create_input("github-path", entries, sources={0: source})
            with self.assertRaisesRegex(producer.ProducerProtocolError, "forbidden"):
                fixture.build("github-path", branch, commit)

    def test_chatgpt_requests_path_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"bad\n"
            entries = [fixture.entry(".chatgpt-requests/x/part-0000.patch", "add", source=source)]
            branch, commit = fixture.create_input("transport-path", entries, sources={0: source})
            with self.assertRaisesRegex(producer.ProducerProtocolError, "forbidden"):
                fixture.build("transport-path", branch, commit)

    def test_duplicate_path_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            one = b"one\n"
            two = b"two\n"
            entries = [
                fixture.entry("docs/same.md", "add", source=one),
                fixture.entry("docs/same.md", "add", source=two),
            ]
            branch, commit = fixture.create_input("duplicate", entries, sources={0: one, 1: two})
            with self.assertRaisesRegex(producer.ProducerProtocolError, "duplicate"):
                fixture.build("duplicate", branch, commit)

    def test_case_collision_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            one = b"one\n"
            two = b"two\n"
            entries = [
                fixture.entry("docs/Case.md", "add", source=one),
                fixture.entry("docs/case.md", "add", source=two),
            ]
            branch, commit = fixture.create_input("case-collision", entries, sources={0: one, 1: two})
            with self.assertRaisesRegex(producer.ProducerProtocolError, "case-colliding"):
                fixture.build("case-collision", branch, commit)

    def test_malformed_manifest_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"updated\n"
            entries = [fixture.entry("docs/update.md", "update", source=source)]

            def mutate(manifest):
                manifest["unexpected"] = True

            branch, commit = fixture.create_input(
                "bad-manifest", entries, sources={0: source}, manifest_mutator=mutate
            )
            with self.assertRaisesRegex(producer.ProducerProtocolError, "unexpected or missing fields"):
                fixture.build("bad-manifest", branch, commit)

    def test_manifest_hash_mismatch_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"updated\n"
            entries = [fixture.entry("docs/update.md", "update", source=source)]

            def mutate(ready):
                ready["manifest_sha256"] = "0" * 64

            branch, commit = fixture.create_input(
                "manifest-hash", entries, sources={0: source}, ready_mutator=mutate
            )
            with self.assertRaisesRegex(producer.ProducerProtocolError, "manifest_sha256"):
                fixture.build("manifest-hash", branch, commit)

    def test_source_hash_mismatch_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            declared = b"declared\n"
            actual = b"different\n"
            entries = [fixture.entry("docs/update.md", "update", source=declared)]
            branch, commit = fixture.create_input("source-hash", entries, sources={0: actual})
            with self.assertRaisesRegex(producer.ProducerProtocolError, "source_(bytes|sha256)"):
                fixture.build("source-hash", branch, commit)

    def test_missing_source_file_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"updated\n"
            entries = [fixture.entry("docs/update.md", "update", source=source)]
            branch, commit = fixture.create_input(
                "missing-source", entries, sources={0: source}, omit_source_indexes={0}
            )
            with self.assertRaisesRegex(producer.ProducerProtocolError, "missing producer source file"):
                fixture.build("missing-source", branch, commit)

    def test_extra_source_file_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"updated\n"
            entries = [fixture.entry("docs/update.md", "update", source=source)]
            branch, commit = fixture.create_input(
                "extra-source", entries, sources={0: source}, extra_source=True
            )
            with self.assertRaisesRegex(producer.ProducerProtocolError, "missing or extra files"):
                fixture.build("extra-source", branch, commit)

    def test_whitespace_error_is_rejected_by_request_builder(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"trailing whitespace \n"
            entries = [fixture.entry("docs/update.md", "update", source=source)]
            branch, commit = fixture.create_input("whitespace", entries, sources={0: source})
            with self.assertRaisesRegex(producer.ProducerProtocolError, "whitespace"):
                fixture.build("whitespace", branch, commit)

    def test_invalid_request_id_or_branch_mismatch_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"updated\n"
            entries = [fixture.entry("docs/update.md", "update", source=source)]
            branch, commit = fixture.create_input("valid-id", entries, sources={0: source})
            with self.assertRaisesRegex(producer.ProducerProtocolError, "request_id must match"):
                fixture.build("different-id", branch, commit)

    def test_ready_commit_with_extra_change_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"updated\n"
            entries = [fixture.entry("docs/update.md", "update", source=source)]
            branch, commit = fixture.create_input(
                "ready-extra", entries, sources={0: source}, ready_extra=True
            )
            with self.assertRaises(producer.ProducerProtocolError):
                fixture.build("ready-extra", branch, commit)

    def test_crlf_source_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"updated\r\n"
            entries = [fixture.entry("docs/update.md", "update", source=source)]
            branch, commit = fixture.create_input("crlf", entries, sources={0: source})
            with self.assertRaisesRegex(producer.ProducerProtocolError, "LF line endings"):
                fixture.build("crlf", branch, commit)

    def test_binary_source_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"text\x00binary"
            entries = [fixture.entry("docs/update.md", "update", source=source)]
            branch, commit = fixture.create_input("binary", entries, sources={0: source})
            with self.assertRaisesRegex(producer.ProducerProtocolError, "binary/NUL"):
                fixture.build("binary", branch, commit)

    def test_builder_real_remote_recheck_rejects_race_after_producer_validation(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            fixture = ProducerFixture(Path(temp))
            source = b"updated\n"
            entries = [fixture.entry("docs/update.md", "update", source=source)]
            branch, commit = fixture.create_input("remote-race", entries, sources={0: source})
            advanced = fixture.advance_remote(
                "chatgpt/gameengine-test", "docs/race.md", "advanced during build\n"
            )
            fixture.run("git", "push", "--force", "origin", f"{fixture.base}:chatgpt/gameengine-test")
            original = producer.request_protocol.build_request

            def advance_then_build(args):
                fixture.run(
                    "git",
                    "push",
                    "--force",
                    "origin",
                    f"{advanced}:chatgpt/gameengine-test",
                )
                return original(args)

            with mock.patch.object(producer.request_protocol, "build_request", side_effect=advance_then_build):
                with self.assertRaisesRegex(producer.ProducerProtocolError, "remote target moved"):
                    fixture.build("remote-race", branch, commit)


class TrustedProducerWorkflowContractTests(unittest.TestCase):
    def test_signal_is_default_branch_read_only(self) -> None:
        root = Path(__file__).resolve().parents[2]
        signal = (root / ".github" / "workflows" / "gameengine-chatgpt-producer-signal.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("issues:\n    types:\n      - opened", signal)
        self.assertIn("permissions:\n  contents: read\n  issues: read", signal)
        self.assertNotIn("contents: write", signal)
        self.assertIn("ref: main", signal)
        self.assertIn("gameengine-chatgpt-producer-v1", signal)

    def test_trusted_producer_uses_signal_and_never_writes_transport(self) -> None:
        root = Path(__file__).resolve().parents[2]
        trusted = (root / ".github" / "workflows" / "gameengine-chatgpt-trusted-producer.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("GameEngine ChatGPT Producer Signal", trusted)
        self.assertIn("ref: main", trusted)
        self.assertIn("request_protocol.py preflight-stage", trusted)
        self.assertIn("gh workflow run gameengine-chatgpt-stage-signal.yml", trusted)
        self.assertNotIn("gh workflow run gameengine-chatgpt-transport-publisher.yml", trusted)
        self.assertNotIn("HEAD:refs/heads/chatgpt-dispatch\n", trusted)

    def test_stage_signal_rejects_branch_mismatch_and_extra_ready_changes(self) -> None:
        root = Path(__file__).resolve().parents[2]
        stage = (root / ".github" / "workflows" / "gameengine-chatgpt-stage-signal.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("workflow_dispatch:", stage)
        self.assertIn("permissions:\n  contents: read", stage)
        self.assertIn("^chatgpt-dispatch-stage-", stage)
        self.assertIn("[[ ${#changed_paths[@]} -eq 1 ]]", stage)
        self.assertIn('expected_ready_path=".chatgpt-requests/$request_id/ready.json"', stage)
        self.assertIn('[[ "$remote_head" == "$REQUEST_COMMIT" ]]', stage)

    def test_publisher_remains_single_writer_with_exact_lease(self) -> None:
        root = Path(__file__).resolve().parents[2]
        publisher = (root / ".github" / "workflows" / "gameengine-chatgpt-transport-publisher.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("cancel-in-progress: false", publisher)
        self.assertIn("queue: max", publisher)
        self.assertIn("git rev-parse --verify", publisher)
        self.assertIn('--force-with-lease="refs/heads/chatgpt-dispatch:$transport_head"', publisher)
        self.assertIn("The final staged commit must change only", publisher)
        self.assertIn("request_protocol.py", publisher)


if __name__ == "__main__":
    unittest.main(verbosity=2)
