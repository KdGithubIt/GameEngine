#!/usr/bin/env python3
"""Regression coverage for fail-closed authoring-contract main drift."""

from __future__ import annotations

import importlib.util
import inspect
from pathlib import Path
import unittest

MODULE_PATH = Path(__file__).with_name("request_protocol.py")
spec = importlib.util.spec_from_file_location("request_protocol", MODULE_PATH)
assert spec and spec.loader
protocol = importlib.util.module_from_spec(spec)
spec.loader.exec_module(protocol)


class ContractDriftProtocolTests(unittest.TestCase):
    def test_authoring_and_automation_contract_paths_are_fail_closed(self) -> None:
        paths = [
            "AGENTS.md",
            "CLAUDE.md",
            "docs/AI_FRIENDLY_AUTHORING_SPEC.md",
            "docs/RUST_CODE_STYLE.md",
            "docs/DEVELOPMENT_WORKFLOW.md",
            "docs/CHATGPT_AUTOMATION.md",
            "docs/CHATGPT_WORKER.md",
        ]
        for path in paths:
            with self.subTest(path=path):
                self.assertEqual(protocol._drift_full_reason(path), path)

    def test_contract_risk_is_checked_before_docs_fast_path(self) -> None:
        source = inspect.getsource(protocol._validate_main_drift)
        risk_check = source.index("for path in drift_paths:")
        docs_fast_path = source.index("if not drift_paths or all(_is_drift_doc_path(path) for path in drift_paths):")
        self.assertLess(risk_check, docs_fast_path)

    def test_ordinary_documentation_can_still_use_docs_fast_path(self) -> None:
        self.assertTrue(protocol._is_drift_doc_path("README.md"))
        self.assertTrue(protocol._is_drift_doc_path("docs/adr/0127-native-2d-gameplay-and-authoring-architecture.md"))
        self.assertIsNone(protocol._drift_full_reason("README.md"))
        self.assertIsNone(protocol._drift_full_reason("docs/adr/0127-native-2d-gameplay-and-authoring-architecture.md"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
