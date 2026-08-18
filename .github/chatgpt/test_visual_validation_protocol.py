#!/usr/bin/env python3
"""Regression coverage for trusted visual-validation result discovery."""

from __future__ import annotations

from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_PATH = ROOT / ".github" / "workflows" / "gameengine-editor-visual-validation.yml"
SPEC_PATH = ROOT / "docs" / "EDITOR_VISUAL_VALIDATION.md"


class VisualValidationProtocolTests(unittest.TestCase):
    def _workflow_sections(self) -> tuple[str, str, str]:
        source = WORKFLOW_PATH.read_text(encoding="utf-8")
        discovery = source.split("\n  discovery:\n", 1)[1].split("\n  capture:\n", 1)[0]
        capture = source.split("\n  capture:\n", 1)[1].split("\n  report:\n", 1)[0]
        report = source.split("\n  report:\n", 1)[1]
        return discovery, capture, report

    def test_run_discovery_preserves_trusted_execution_boundary(self) -> None:
        source = WORKFLOW_PATH.read_text(encoding="utf-8")
        discovery, capture, report = self._workflow_sections()
        self.assertIn("pull_request_target:", source)
        self.assertIn("needs: context", discovery)
        self.assertNotIn("needs: [context, capture]", discovery)
        self.assertIn("issues: write", discovery)
        self.assertIn("pull-requests: read", discovery)
        self.assertNotIn("actions/checkout", discovery)
        self.assertIn("permissions:\n      contents: read", capture)
        self.assertNotIn("issues: write", capture)
        self.assertIn("issues: write", report)
        self.assertIn("pull-requests: read", report)
        self.assertNotIn("actions/checkout", report)

    def test_discovery_publishes_run_identity_before_capture_completion(self) -> None:
        discovery, _, _ = self._workflow_sections()
        self.assertIn('"- State: **queued**"', discovery)
        self.assertIn('"- Result: **pending**"', discovery)
        self.assertIn("RUN_ID: ${{ github.run_id }}", discovery)
        self.assertIn("RUN_ATTEMPT: ${{ github.run_attempt }}", discovery)
        self.assertIn("Artifact lookup key:", discovery)
        self.assertIn("gameengine-editor-visual-validation-${runId}-${runAttempt}", discovery)
        self.assertIn("github.rest.issues.updateComment", discovery)
        self.assertIn("github.rest.issues.createComment", discovery)

    def test_report_finalizes_same_discovery_comment_after_capture(self) -> None:
        _, _, report = self._workflow_sections()
        self.assertIn("needs: [context, discovery, capture]", report)
        self.assertIn('"- State: **completed**"', report)
        self.assertIn("CAPTURE_RESULT: ${{ needs.capture.result }}", report)
        self.assertIn("RESOLVED_TARGET: ${{ needs.capture.outputs.target }}", report)
        self.assertIn("gameengine-editor-visual-validation-${runId}-${runAttempt}", report)
        self.assertIn("github.rest.issues.updateComment", report)
        self.assertIn("github.rest.issues.createComment", report)

    def test_discovery_and_report_reject_stale_overwrites(self) -> None:
        discovery, _, report = self._workflow_sections()
        for section in (discovery, report):
            self.assertIn("currentHead !== headSha", section)
            self.assertIn("existingRunId > runId", section)
            self.assertIn("existingRunId === runId && existingAttempt > runAttempt", section)

    def test_visual_validation_spec_defines_early_discovery_contract(self) -> None:
        spec = SPEC_PATH.read_text(encoding="utf-8")
        self.assertIn("Version: 1.4.0", spec)
        self.assertIn("<!-- gameengine-visual-validation-result -->", spec)
        self.assertIn("publishes trusted workflow-run identity before Windows capture completion", spec)
        self.assertIn("State: **queued**", spec)
        self.assertIn("Result: **pending**", spec)
        self.assertIn("Artifact lookup key", spec)
        self.assertIn("workflow run ID", spec)
        self.assertIn("actually reviewed", spec)


if __name__ == "__main__":
    unittest.main(verbosity=2)
