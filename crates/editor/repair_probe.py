from __future__ import annotations

import difflib
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
DIAG = Path(os.environ["RUNNER_TEMP"]) / "gameengine-validation-diagnostics"
DIAG.mkdir(parents=True, exist_ok=True)

RUST_PATHS = [
    "crates/editor/src/acp_agent_host_bridge.rs",
    "crates/editor/src/acp_agent_runtime.rs",
    "crates/editor/src/acp_agent_runtime/transport.rs",
    "crates/editor/src/acp_integration.rs",
    "crates/editor/src/agent_benchmark.rs",
    "crates/editor/src/agent_benchmark_campaign.rs",
    "crates/editor/src/ai_studio.rs",
    "crates/editor/src/ai_studio/benchmark_campaign_ui.rs",
    "crates/editor/src/ai_studio/benchmark_child.rs",
    "crates/editor/src/ai_studio/execution_routing.rs",
    "crates/editor/src/benchmark_campaign.rs",
    "crates/editor/src/benchmark_comparison.rs",
    "crates/editor/src/benchmark_process.rs",
    "crates/editor/src/claude_acp_adapter.rs",
    "crates/editor/src/codex_acp_adapter.rs",
    "crates/editor/src/external_agent_provider.rs",
    "crates/editor/src/goose_local_acp.rs",
    "crates/editor/src/lib.rs",
    "crates/editor/src/managed_local_runtime.rs",
    "crates/editor/src/model_router.rs",
]

COMPILE_OLD = "let Ok((succeeded, output)) = direct_command_output(locator, locator_args) else {"
COMPILE_NEW = "let Ok((succeeded, output)) = direct_command_output(locator, locator_args, &[]) else {"
provider = ROOT / "crates/editor/src/external_agent_provider.rs"
provider_text = provider.read_text(encoding="utf-8")
if provider_text.count(COMPILE_OLD) != 1:
    raise RuntimeError("compile repair anchor is not unique")
provider.write_text(provider_text.replace(COMPILE_OLD, COMPILE_NEW), encoding="utf-8", newline="\n")

for rel in RUST_PATHS:
    subprocess.run(["rustfmt", "--edition", "2024", str(ROOT / rel)], cwd=ROOT, check=True)

def blob_text(rel: str) -> str:
    result = subprocess.run(
        ["git", "show", f"HEAD:{rel}"], cwd=ROOT, check=True, stdout=subprocess.PIPE
    )
    return result.stdout.decode("utf-8").replace("\r\n", "\n")

def work_text(rel: str) -> str:
    return (ROOT / rel).read_bytes().decode("utf-8").replace("\r\n", "\n")

def apply_groups(path: str, old: str, new: str, context: int):
    old_lines = old.splitlines(keepends=True)
    new_lines = new.splitlines(keepends=True)
    matcher = difflib.SequenceMatcher(None, old_lines, new_lines, autojunk=False)
    current = old
    edits = []
    for group in matcher.get_grouped_opcodes(context):
        i1, i2 = group[0][1], group[-1][2]
        j1, j2 = group[0][3], group[-1][4]
        before = "".join(old_lines[i1:i2])
        after = "".join(new_lines[j1:j2])
        if not before or current.count(before) != 1:
            return None
        op = {"operation": "replace_text", "path": path, "old": before, "new": after}
        if len(json.dumps(op, ensure_ascii=False).encode("utf-8")) > 240000:
            return None
        current = current.replace(before, after, 1)
        edits.append(op)
    return edits if current == new else None

def make_edits(path: str, old: str, new: str):
    if old == new:
        return []
    whole = {"operation": "replace_text", "path": path, "old": old, "new": new}
    if len(json.dumps(whole, ensure_ascii=False).encode("utf-8")) <= 220000:
        return [whole]
    for context in (3, 5, 8, 12, 20, 32, 48):
        edits = apply_groups(path, old, new, context)
        if edits is not None:
            return edits
    raise RuntimeError(f"could not build unique bounded edits for {path}")

all_edits = []
for rel in RUST_PATHS + ["Cargo.lock"]:
    old = blob_text(rel)
    new = work_text(rel)
    all_edits.extend(make_edits(rel, old, new))

for index, edit in enumerate(all_edits):
    path = DIAG / f"edit-{index:04d}.json"
    path.write_text(json.dumps(edit, ensure_ascii=False, separators=(",", ":")) + "\n", encoding="utf-8", newline="\n")
manifest = {"edit_count": len(all_edits), "paths": sorted({edit["path"] for edit in all_edits})}
(DIAG / "repair-probe-manifest.json").write_text(
    json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n"
)
print(f"generated {len(all_edits)} repair edits")
