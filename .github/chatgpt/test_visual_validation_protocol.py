#!/usr/bin/env python3
"""Regression coverage for discoverable Editor visual validation."""

from __future__ import annotations

from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_PATH = ROOT / ".github" / "workflows" / "gameengine-editor-visual-validation.yml"
SPEC_PATH = ROOT / "docs" / "EDITOR_VISUAL_VALIDATION.md"


class VisualValidationProtocolTests(unittest.TestCase):
    def _workflow_sections(self) -> tuple[str, str]:
        source = WORKFLOW_PATH.read_text(encoding="utf-8")
        context = source.split("\n  context:\n", 1)[1].split("\n  capture:\n", 1)[0]
        capture = source.split("\n  capture:\n", 1)[1]
        return context, capture

    def test_pull_request_trigger_is_directly_discoverable(self) -> None:
        source = WORKFLOW_PATH.read_text(encoding="utf-8")
        self.assertIn("\n  pull_request:\n", source)
        self.assertNotIn("pull_request_target:", source)
        self.assertIn("types: [opened, synchronize, edited, reopened]", source)
        self.assertNotIn("gameengine-visual-validation-result", source)

    def test_visual_request_remains_same_repository_and_exact_head(self) -> None:
        context, capture = self._workflow_sections()
        self.assertIn("github.event.pull_request.head.repo.full_name == github.repository", context)
        self.assertIn("github.event.pull_request.base.ref == 'main'", context)
        self.assertIn("startsWith(github.event.pull_request.head.ref, 'chatgpt/gameengine-')", context)
        self.assertIn("contains(github.event.pull_request.body, '<!-- gameengine-visual-validation:')", context)
        self.assertIn("ref: ${{ needs.context.outputs.head_sha }}", capture)
        self.assertIn("persist-credentials: false", capture)
        self.assertIn("EXPECTED_HEAD_SHA: ${{ needs.context.outputs.head_sha }}", capture)

    def test_capture_has_no_write_token_or_reporting_job(self) -> None:
        source = WORKFLOW_PATH.read_text(encoding="utf-8")
        _, capture = self._workflow_sections()
        self.assertIn("permissions: {}", source)
        self.assertIn("permissions:\n      contents: read", capture)
        self.assertNotIn("issues: write", source)
        self.assertNotIn("pull-requests: write", source)
        self.assertNotIn("\n  discovery:\n", source)
        self.assertNotIn("\n  report:\n", source)

    def test_artifact_name_is_derived_from_directly_enumerable_run(self) -> None:
        _, capture = self._workflow_sections()
        self.assertIn(
            "name: gameengine-editor-visual-validation-${{ github.run_id }}-${{ github.run_attempt }}",
            capture,
        )
        self.assertIn("editor.png", SPEC_PATH.read_text(encoding="utf-8"))

    def test_visual_validation_spec_defines_direct_run_discovery_contract(self) -> None:
        spec = SPEC_PATH.read_text(encoding="utf-8")
        self.assertIn("Version: 1.5.0", spec)
        self.assertIn("`pull_request`", spec)
        self.assertIn("commit-filtered pull-request workflow run lookup", spec)
        self.assertIn("workflow run ID", spec)
        self.assertIn("Artifact lookup key", spec)
        self.assertIn("actually reviewed", spec)


if __name__ == "__main__":
    unittest.main(verbosity=2)
