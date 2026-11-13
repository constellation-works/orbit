#!/usr/bin/env python3
"""Bind cached Cargo workspace outputs to the bytes and modes of their inputs.

Cargo uses input mtimes as well as its compiler/feature/environment fingerprints.
A fresh checkout otherwise makes every workspace unit in a restored target dirty.
Only byte-identical regular files regain their original mtimes. Changed and new
inputs get a fresh mtime even when their checkout preserved an older timestamp;
Cargo still decides which units need rebuilding.
"""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import time
import os


MANIFEST = ".orbit-source-inputs.json"
MAX_SOURCE_TIMESTAMP_NS = (1 << 63) - 1
# The changed-input refresh preserves the stale-build guard from F2026-09-081.


def workspace_input_files(workspace):
    """Include tracked and non-ignored untracked inputs, without following links out."""
    listing = subprocess.check_output(
        ["git", "-C", str(workspace), "ls-files", "--cached", "--others",
         "--exclude-standard", "-z"],
    )
    for name in sorted(set(os.fsdecode(listing).split("\0")) - {""}):
        path = workspace / name
        if (path.is_file() and not path.is_symlink()
                and path.resolve().is_relative_to(workspace)):
            yield name, path


def input_identity(path):
    info = path.stat()
    return {"sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "mtime_ns": info.st_mtime_ns, "mode": stat.S_IMODE(info.st_mode),
            "size": info.st_size}


def validate_source_manifest(manifest, workspace):
    """Validate the entire record before changing any input's timestamp."""
    if not isinstance(manifest, dict):
        raise ValueError("incompatible workspace source manifest")
    if (manifest.get("schema_version") != 1 or manifest.get("workspace") != str(workspace)
            or not re.fullmatch(r"[0-9a-f]{40}", manifest.get("head", ""))
            or type(manifest.get("captured_at_ns")) is not int
            or not 0 <= manifest["captured_at_ns"] < MAX_SOURCE_TIMESTAMP_NS
            or not isinstance(manifest.get("files"), dict)):
        raise ValueError("incompatible workspace source manifest")
    for name, record in manifest["files"].items():
        path = PurePosixPath(name)
        if (not name or path.is_absolute() or ".." in path.parts
                or path.as_posix() != name or not isinstance(record, dict)
                or not re.fullmatch(r"[0-9a-f]{64}", record.get("sha256", ""))
                or any(type(record.get(key)) is not int for key in ("mtime_ns", "mode", "size"))
                or not 0 <= record["mtime_ns"] <= manifest["captured_at_ns"]
                or not 0 <= record["mode"] <= 0o7777 or record["size"] < 0):
            raise ValueError(f"invalid workspace source record: {name!r}")


def capture_workspace_inputs(workspace, manifest_path):
    manifest = {
        "schema_version": 1, "workspace": str(workspace),
        "head": subprocess.check_output(
            ["git", "-C", str(workspace), "rev-parse", "HEAD"], text=True).strip(),
        "files": {name: input_identity(path)
                  for name, path in workspace_input_files(workspace)},
        "captured_at_ns": time.time_ns(),
    }
    validate_source_manifest(manifest, workspace)
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    temporary = manifest_path.with_suffix(".pending")
    temporary.write_text(json.dumps(manifest, sort_keys=True) + "\n")
    temporary.replace(manifest_path)
    return {"captured": len(manifest["files"]), "head": manifest["head"]}


def restore_workspace_inputs(workspace, manifest_path):
    if not manifest_path.exists():
        return {"cache": "miss", "reused_inputs": 0}
    manifest = json.loads(manifest_path.read_text())
    validate_source_manifest(manifest, workspace)
    changed_ns = max(time.time_ns(), manifest["captured_at_ns"] + 1)
    reused = changed = 0
    for name, path in workspace_input_files(workspace):
        current = input_identity(path)
        previous = manifest["files"].get(name)
        same = previous is not None and all(
            current[key] == previous[key] for key in ("sha256", "mode", "size"))
        modified_ns = previous["mtime_ns"] if same else changed_ns
        os.utime(path, ns=(path.stat().st_atime_ns, modified_ns))
        reused += same
        changed += not same
    return {"cache": "restored", "reused_inputs": reused, "changed_inputs": changed,
            "source_head": manifest["head"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("capture", "restore"))
    parser.add_argument("--workspace", type=Path, required=True)
    arguments = parser.parse_args()
    workspace = arguments.workspace.resolve(strict=True)
    target = workspace / "target"
    if target.is_symlink():
        parser.error("the workspace target directory must not be a symlink")
    manifest_path = target / MANIFEST
    try:
        operation = capture_workspace_inputs if arguments.action == "capture" else restore_workspace_inputs
        print(json.dumps(operation(workspace, manifest_path), sort_keys=True))
    except (OSError, ValueError, TypeError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"ci-workspace-cache: {error}\n")


if __name__ == "__main__":
    main()
