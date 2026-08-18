#!/usr/bin/env python3
"""Regression coverage for trusted visual-validation result discovery."""

from __future__ import annotations

from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_PATH = ROOT / ".github" / "workflows" / "gameengine-editor-visual-validation.yml"
SPEC_PATH = ROOT / "docs" / "EDITOR_VISUAL_VALIDATION.md"


class VisualValidationProtocolTests(unittest.TestCase):
    def test_report_bridge_preserves_trusted_execution_boundary(self) -> None:
        source = WORKFLOW_PATH.read_text(encoding="utf-8")
        self.assertIn("pull_request_target:", source)
        capture = source.split("\n  capture:\n", 1)[1].split("\n  report:\n", 1)[0]
        report = source.split("\n  report:\n", 1)[1]
        self.assertIn("permissions:\n      contents: read", capture)
        self.assertNotIn("issues: write", capture)
        self.assertIn("issues: write", report)
        self.assertIn("pull-requests: read", report)
        self.assertNotIn("actions/checkout", report)

    def test_report_bridge_publishes_run_and_artifact_lookup_metadata(self) -> None:
        source = WORKFLOW_PATH.read_text(encoding="utf-8")
        self.assertIn("<!-- gameengine-visual-validation-result -->", source)
        self.assertIn("RUN_ID: ${{ github.run_id }}", source)
        self.assertIn("RUN_ATTEMPT: ${{ github.run_attempt }}", source)
        self.assertIn("Artifact lookup key:", source)
        self.assertIn("gameengine-editor-visual-validation-${runId}-${runAttempt}", source)
        self.assertIn("github.rest.issues.updateComment", source)
        self.assertIn("github.rest.issues.createComment", source)

    def test_report_bridge_rejects_stale_overwrites(self) -> None:
        source = WORKFLOW_PATH.read_text(encoding="utf-8")
        self.assertIn("currentHead !== headSha", source)
        self.assertIn("existingRunId > runId", source)
        self.assertIn("existingRunId === runId && existingAttempt > runAttempt", source)

    def test_visual_validation_spec_defines_comment_discovery_contract(self) -> None:
        spec = SPEC_PATH.read_text(encoding="utf-8")
        self.assertIn("Version: 1.3.0", spec)
        self.assertIn("<!-- gameengine-visual-validation-result -->", spec)
        self.assertIn("Artifact lookup key", spec)
        self.assertIn("workflow run ID", spec)
        self.assertIn("actually reviewed", spec)


if __name__ == "__main__":
    unittest.main(verbosity=2)
