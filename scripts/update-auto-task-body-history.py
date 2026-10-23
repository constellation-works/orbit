#!/usr/bin/env python3
"""Backfill bundled auto-task body fingerprints from landed asset history.

Requires PyYAML. Run from the checkout; the output is compiled into Orbit.
Existing records and source fixtures are retained when a shallow checkout lacks
their revisions; available revisions are recomputed to repair old fingerprints.
"""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess

import yaml


ROOT = Path(__file__).resolve().parent.parent
ASSETS = Path("crates/orbit-core/assets/auto_tasks")
OUTPUT = ROOT / ASSETS / "body-history.json"
SOURCES = ROOT / "crates/orbit-cli/tests/fixtures/auto-task-body-history/sources.json"
PLACEHOLDER = "__ORBIT_BASE_BRANCH__"


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True)


def source(path, revision):
    # Missing commits in a shallow clone must not erase previously shipped bodies.
    available = subprocess.run(
        ["git", "cat-file", "-e", f"{revision}^{{commit}}"], cwd=ROOT,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    if available.returncode:
        return None
    return git("show", f"{revision}:{path}")


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
    if template.get("required_tools"):
        # deserialize_required_tools uses a BTreeSet: sort and deduplicate exact
        # names without trimming or case folding.
        template["required_tools"] = sorted(set(template["required_tools"]))
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


def same_body(left, right):
    return all(left[key] == right[key] for key in ("digest", "settings", "comments"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check", action="store_true",
        help="verify superseded bodies and source fixtures without writing (requires full history)",
    )
    args = parser.parse_args()
    if args.check and git("rev-parse", "--is-shallow-repository").strip() != "false":
        parser.exit(1, "body history check requires full Git history; fetch with --unshallow\n")
    history = json.loads(OUTPUT.read_text()) if OUTPUT.exists() else {}
    sources = json.loads(SOURCES.read_text()) if SOURCES.exists() else {}
    snapshots = {}
    names = set(history) | {asset.stem for asset in (ROOT / ASSETS).glob("*.yaml")}
    for name in sorted(names):
        path = (ASSETS / f"{name}.yaml").as_posix()
        records = []
        originals = sources.get(name, {})
        snapshots[name] = {}
        current_path = ROOT / path
        current = record(current_path.read_text(), "") if current_path.exists() else None
        # Only integration commits are trusted; uncommitted edits never gain provenance.
        revisions = git("log", "--first-parent", "--format=%H", "HEAD", "--", path).splitlines()
        candidates = [old["revision"] for old in history.get(name, [])] + revisions
        for revision in candidates:
            raw = source(path, revision)
            if raw is None:
                raw = originals.get(revision)
            if raw is None:
                raise ValueError(f"missing source fixture for {name} at {revision}")
            candidate = record(raw, revision)
            # A current body needs no entry until superseded. Compare with the
            # candidate checkout, so an uncommitted successor still requires
            # the last landed body. Retain any current entry already recorded.
            if (args.check and current is not None and same_body(current, candidate)
                    and not any(same_body(old, candidate) for old in history.get(name, []))):
                continue
            if not any(same_body(old, candidate) for old in records):
                records.append(candidate)
                snapshots[name][revision] = raw
        records.sort(key=lambda item: (item["digest"], item["revision"]))
        history[name] = records
    stale = []
    for path, value in ((OUTPUT, history), (SOURCES, snapshots)):
        rendered = json.dumps(value, indent=2, ensure_ascii=False, sort_keys=True) + "\n"
        if args.check:
            if not path.exists() or path.read_text() != rendered:
                stale.append(str(path.relative_to(ROOT)))
        else:
            path.write_text(rendered)
    if stale:
        parser.exit(1, "auto-task body history is stale: " + ", ".join(stale)
                    + "\nRun python3 scripts/update-auto-task-body-history.py and commit both outputs.\n")


if __name__ == "__main__":
    main()
