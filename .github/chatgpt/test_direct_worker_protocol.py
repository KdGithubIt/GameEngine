#!/usr/bin/env python3
"""Regression coverage for the direct current-protocol ChatGPT Worker."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKER_PATH = ROOT / "scripts" / "public" / "gameengine_chatgpt_worker.py"
spec = importlib.util.spec_from_file_location("gameengine_chatgpt_worker", WORKER_PATH)
assert spec and spec.loader
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class DirectWorkerProtocolTests(unittest.TestCase):
    def test_worker_is_bound_to_public_gameengine_and_current_transport(self) -> None:
        source = WORKER_PATH.read_text(encoding="utf-8")
        self.assertEqual(worker.EXPECTED_REPOSITORY, "KdGithubIt/GameEngine")
        self.assertIn("request_protocol.py", source)
        self.assertIn("chatgpt-dispatch-stage-", source)
        self.assertIn("gameengine-chatgpt-stage-signal.yml", source)
        self.assertIn("gameengine-chatgpt-transport-publisher.yml", source)
        self.assertIn("gameengine-chatgpt-dispatcher.yml", source)
        self.assertIn("gameengine-windows-validation.yml", source)
        self.assertNotIn("KdGithubIt/RustProject", source)
        self.assertNotIn("gameengine_chatgpt_apply_patch", source)
        self.assertNotIn("GameEngine ChatGPT Bridge", source)

    def test_target_branch_contract(self) -> None:
        worker.require_target_branch("chatgpt/gameengine-worker-test")
        with self.assertRaises(worker.WorkerError):
            worker.require_target_branch("main")
        with self.assertRaises(worker.WorkerError):
            worker.require_target_branch("chatgpt/gameengine-../escape")

    def test_request_id_contract(self) -> None:
        worker.require_request_id("worker-20260817-01")
        with self.assertRaises(worker.WorkerError):
            worker.require_request_id("bad/request")
        with self.assertRaises(worker.WorkerError):
            worker.require_request_id("x" * 65)

    def test_worker_keeps_visual_success_distinct_from_human_review(self) -> None:
        source = WORKER_PATH.read_text(encoding="utf-8")
        self.assertIn("workflow-success-human-review-required", source)
        self.assertNotIn('visual = "PASS"', source)

    def test_worker_uses_authoring_patch_only_before_canonical_builder(self) -> None:
        source = WORKER_PATH.read_text(encoding="utf-8")
        apply_index = source.index('"apply",\n            "--index"')
        builder_index = source.index('request_protocol.py')
        publish_index = source.index('def publish_stage')
        self.assertLess(apply_index, publish_index)
        self.assertLess(builder_index, publish_index)
        self.assertIn("canonical builder returned an unexpected request manifest", source)


if __name__ == "__main__":
    unittest.main(verbosity=2)
