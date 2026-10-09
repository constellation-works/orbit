#!/usr/bin/env python3
"""Backfill bundled auto-task body fingerprints from landed asset history.

Requires PyYAML. Run from the checkout; the output is compiled into Orbit.
Existing records are retained so shallow checkouts cannot erase old history.
"""

import hashlib
import json
from pathlib import Path
import subprocess

import yaml


ROOT = Path(__file__).resolve().parent.parent
ASSETS = Path("crates/orbit-core/assets/auto_tasks")
OUTPUT = ROOT / ASSETS / "body-history.json"
PLACEHOLDER = "__ORBIT_BASE_BRANCH__"


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True)


def record(raw, revision):
    # Match the v1 definition's serde defaults/omissions before fingerprinting.
    definition = yaml.safe_load(raw)
    template = definition["template"]
    settings = {
        "enabled": definition.get("enabled", True),
        "schedule": definition["schedule"],
        "dedupe": definition.get("dedupe", "skip_if_open"),
        "priority": template.get("priority", "medium"),
        "tags": template.get("tags", []),
    }
    for field in ("crew", "complexity"):
        if template.get(field) is not None:
            settings[field] = template[field]
    for field in (
        "enabled", "schedule", "dedupe", "created_by", "created_at",
        "updated_by", "updated_at",
    ):
        definition.pop(field, None)
    for field in ("crew", "priority", "complexity", "tags"):
        template.pop(field, None)
    definition.setdefault("description", "")
    template.setdefault("description", "")
    template.setdefault("acceptance_criteria", [])
    template.setdefault("task_type", "chore")
    template.setdefault("status", "backlog")
    for field in ("required_tools", "context_files"):
        if not template.get(field):
            template.pop(field, None)
    probe = definition.get("skip_if_unchanged")
    if probe:
        # Shipped probes used agent-main before branch rendering was introduced.
        if probe["ref"] == "agent-main":
            probe["ref"] = PLACEHOLDER
        if not probe["cursor"].get("legacy_tags"):
            probe["cursor"].pop("legacy_tags", None)
    else:
        definition.pop("skip_if_unchanged", None)
    canonical = json.dumps(definition, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    return {
        "digest": hashlib.sha256(canonical.encode()).hexdigest(),
        "revision": revision,
        "settings": settings,
        "comments": sorted({line.strip() for line in raw.splitlines() if line.strip().startswith("#")}),
    }


def main():
    history = json.loads(OUTPUT.read_text()) if OUTPUT.exists() else {}
    for asset in sorted((ROOT / ASSETS).glob("*.yaml")):
        path = (ASSETS / asset.name).as_posix()
        records = history.setdefault(asset.stem, [])
        # Only integration commits are trusted; uncommitted edits never gain provenance.
        revisions = git("log", "--first-parent", "--format=%H", "HEAD", "--", path).splitlines()
        for revision in revisions:
            raw = git("show", f"{revision}:{path}")
            candidate = record(raw, revision)
            if not any(all(old[key] == candidate[key] for key in ("digest", "settings", "comments")) for old in records):
                records.append(candidate)
        records.sort(key=lambda item: (item["digest"], item["revision"]))
    OUTPUT.write_text(json.dumps(history, indent=2, ensure_ascii=False, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
