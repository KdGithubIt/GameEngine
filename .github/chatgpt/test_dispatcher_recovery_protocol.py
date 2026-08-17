from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))

from dispatcher_recovery import ProtocolError, verify_already_applied_commit


class DispatcherRecoveryProtocolTests(unittest.TestCase):
    def run_git(self, cwd: Path, *args: str) -> str:
        proc = subprocess.run(
            ["git", *args],
            cwd=cwd,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if proc.returncode != 0:
            self.fail(f"git {' '.join(args)} failed: {proc.stderr}")
        return proc.stdout.strip()

    def make_fixture(self) -> tuple[tempfile.TemporaryDirectory[str], Path, dict, bytes, str]:
        temp = tempfile.TemporaryDirectory(prefix="dispatcher-recovery-test-")
        root = Path(temp.name)
        remote = root / "remote.git"
        self.run_git(root, "init", "--bare", str(remote))
        repo = root / "repo"
        self.run_git(root, "clone", str(remote), str(repo))
        self.run_git(repo, "config", "user.name", "ChatGPT GameEngine Dispatcher")
        self.run_git(
            repo,
            "config",
            "user.email",
            "41898282+github-actions[bot]@users.noreply.github.com",
        )

        (repo / "README.md").write_text("baseline\n", encoding="utf-8", newline="\n")
        self.run_git(repo, "add", "README.md")
        self.run_git(repo, "commit", "-m", "baseline")
        baseline = self.run_git(repo, "rev-parse", "HEAD")
        self.run_git(repo, "branch", "-M", "main")
        self.run_git(repo, "push", "-u", "origin", "main")

        target_branch = "chatgpt/gameengine-recovery-test"
        self.run_git(repo, "checkout", "-b", target_branch)
        (repo / "README.md").write_text("baseline\nrequested\n", encoding="utf-8", newline="\n")
        patch = subprocess.run(
            ["git", "diff", "--binary", "--full-index", "--no-ext-diff", baseline, "--"],
            cwd=repo,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        ).stdout
        self.run_git(repo, "add", "README.md")
        self.run_git(repo, "commit", "-m", "Apply requested change")
        candidate = self.run_git(repo, "rev-parse", "HEAD")
        self.run_git(repo, "push", "-u", "origin", target_branch)

        manifest = {
            "schema_version": 2,
            "request_id": "recovery-test",
            "target_branch": target_branch,
            "expected_head_sha": baseline,
            "baseline_main_sha": baseline,
            "commit_message": "Apply requested change",
        }
        return temp, repo, manifest, patch, candidate

    def test_exact_one_parent_applied_request_is_accepted(self) -> None:
        temp, repo, manifest, patch, candidate = self.make_fixture()
        self.addCleanup(temp.cleanup)

        verified = verify_already_applied_commit(repo, manifest, patch)

        self.assertEqual(verified, candidate)

    def test_different_tree_with_same_parent_and_message_is_rejected(self) -> None:
        temp, repo, manifest, patch, _candidate = self.make_fixture()
        self.addCleanup(temp.cleanup)
        expected = manifest["expected_head_sha"]
        target_branch = manifest["target_branch"]

        self.run_git(repo, "reset", "--hard", expected)
        (repo / "README.md").write_text("baseline\nunrelated\n", encoding="utf-8", newline="\n")
        self.run_git(repo, "add", "README.md")
        self.run_git(repo, "commit", "-m", manifest["commit_message"])
        self.run_git(repo, "push", "--force", "origin", f"HEAD:{target_branch}")

        with self.assertRaisesRegex(ProtocolError, "diff bytes do not match"):
            verify_already_applied_commit(repo, manifest, patch)

    def test_recovery_workflow_has_no_product_write_permission(self) -> None:
        workflow = (
            Path(__file__).resolve().parents[1]
            / "workflows"
            / "gameengine-chatgpt-dispatcher-recovery.yml"
        ).read_text(encoding="utf-8")

        self.assertIn("contents: read", workflow)
        self.assertNotIn("contents: write", workflow)
        self.assertIn("group: gameengine-chatgpt-dispatcher", workflow)
        self.assertIn("verify-applied", workflow)
        self.assertIn("expected_head_sha=\"$PUSHED_HEAD_SHA\"", workflow)


if __name__ == "__main__":
    unittest.main()
