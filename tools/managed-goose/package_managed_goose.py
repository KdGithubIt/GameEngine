#!/usr/bin/env python3
"""Create deterministic managed-Goose packaging and external provenance."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import sys
import zipfile
from pathlib import Path

FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
ZIP_TIMESTAMP = (1980, 1, 1, 0, 0, 0)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def canonical_json(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")


def write_zip_entry(archive: zipfile.ZipFile, name: str, data: bytes) -> None:
    info = zipfile.ZipInfo(name, ZIP_TIMESTAMP)
    info.compress_type = zipfile.ZIP_DEFLATED
    info.external_attr = 0o644 << 16
    archive.writestr(info, data, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True, type=Path)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--gameengine-revision", required=True)
    parser.add_argument("--workflow-run-url", required=True)
    parser.add_argument("--artifact-name", required=True)
    parser.add_argument("--github-output", type=Path)
    args = parser.parse_args()

    revision = args.gameengine_revision.strip().lower()
    if not FULL_SHA.fullmatch(revision):
        raise SystemExit("gameengine revision must be a full lowercase 40-character SHA")
    if not args.exe.is_file():
        raise SystemExit(f"goose executable not found: {args.exe}")

    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    args.output_dir.mkdir(parents=True, exist_ok=True)

    distribution_id = manifest["distribution_id"]
    zip_name = f"{distribution_id}-x86_64-pc-windows-msvc.zip"
    zip_path = args.output_dir / zip_name
    provenance_path = args.output_dir / "provenance.json"
    sums_path = args.output_dir / "SHA256SUMS.txt"
    manifest_copy = args.output_dir / "series.json"

    exe_hash = sha256(args.exe)
    embedded_provenance = {
        "schema_version": 1,
        "distribution_id": distribution_id,
        "upstream": manifest["upstream"],
        "patch_series_revision": manifest["patch_series_revision"],
        "patches": [
            {
                "path": patch["path"],
                "sha256": patch["sha256"],
                "origin": patch["origin"],
            }
            for patch in manifest["patches"]
        ],
        "gameengine": {
            "repository": "https://github.com/KdGithubIt/GameEngine",
            "revision": revision,
        },
        "build": manifest["build"],
        "goose_exe_sha256": exe_hash,
    }

    with zipfile.ZipFile(zip_path, "w") as archive:
        write_zip_entry(archive, "goose.exe", args.exe.read_bytes())
        write_zip_entry(archive, "PROVENANCE.json", canonical_json(embedded_provenance))

    zip_hash = sha256(zip_path)
    external_provenance = {
        **embedded_provenance,
        "generated_zip": {
            "filename": zip_name,
            "sha256": zip_hash,
        },
        "workflow": {
            "run_url": args.workflow_run_url,
        },
        "artifact": {
            "name": args.artifact_name,
        },
        "release": {
            "repository": "https://github.com/KdGithubIt/GameEngine",
            "tag": manifest["release_tag"],
            "publication": "workflow_dispatch-only",
        },
    }
    provenance_path.write_bytes(canonical_json(external_provenance))
    shutil.copyfile(args.manifest, manifest_copy)
    sums_path.write_text(
        f"{exe_hash}  goose.exe\n{zip_hash}  {zip_name}\n",
        encoding="utf-8",
        newline="\n",
    )

    print(f"distribution_id={distribution_id}")
    print(f"goose_exe_sha256={exe_hash}")
    print(f"zip_sha256={zip_hash}")
    print(f"zip_path={zip_path}")
    if args.github_output:
        with args.github_output.open("a", encoding="utf-8", newline="\n") as output:
            output.write(f"distribution_id={distribution_id}\n")
            output.write(f"goose_exe_sha256={exe_hash}\n")
            output.write(f"zip_sha256={zip_hash}\n")
            output.write(f"zip_name={zip_name}\n")
            output.write(f"release_tag={manifest['release_tag']}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
