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
# A revision counts as landed only when reachable from an integration
# branch. Task-branch commits are reachable from HEAD but, after a squash
# merge, never become ancestors of the integration branch, so recording
# them makes `--check` fail there. Prefer the remote integration branch;
# fall back to HEAD's first-parent history when no integration ref exists
# (e.g. the isolated fixtures in the generator's test).
INTEGRATION_REF_GROUPS = (
    ("origin/agent-main", "agent-main"),
    ("origin/main", "main"),
    ("origin/master", "master"),
)


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True)


def ref_available(ref):
    return not subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}"], cwd=ROOT,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    ).returncode


def integration_refs():
    for group in INTEGRATION_REF_GROUPS:
        refs = [ref for ref in group if ref_available(ref)]
        if refs:
            return refs
    return []


def commit_available(revision):
    return not subprocess.run(
        ["git", "cat-file", "-e", f"{revision}^{{commit}}"], cwd=ROOT,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    ).returncode


def source(path, revision):
    # Missing commits in a shallow clone must not erase previously shipped bodies.
    if not commit_available(revision):
        return None
    # A revision that deleted the asset has no body; the earlier ones do.
    shown = subprocess.run(
        ["git", "show", f"{revision}:{path}"], cwd=ROOT, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    )
    return shown.stdout if shown.returncode == 0 else ""


def record(raw, revision):
    # Match the v1 definition's serde defaults/omissions before fingerprinting.
    where = f"auto-task body at {revision}" if revision else "current auto-task body"
    try:
        definition = yaml.safe_load(raw)
    except yaml.YAMLError as exc:
        raise ValueError(f"{where}: invalid YAML: {exc}") from exc
    if not isinstance(definition, dict):
        raise ValueError(f"{where}: definition must be a mapping, got {type(definition).__name__}")
    template = definition.get("template")
    if not isinstance(template, dict):
        raise ValueError(f"{where}: template must be a mapping, got {type(template).__name__}")
    if "schedule" not in definition:
        raise ValueError(f"{where}: missing schedule")
    if not isinstance(definition["schedule"], dict):
        raise ValueError(
            f"{where}: schedule must be a mapping, got {type(definition['schedule']).__name__}"
        )
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
        if not isinstance(probe, dict):
            raise ValueError(f"{where}: skip_if_unchanged must be a mapping, got {type(probe).__name__}")
        cursor = probe.get("cursor")
        if not isinstance(cursor, dict):
            raise ValueError(f"{where}: skip_if_unchanged.cursor must be a mapping, got {type(cursor).__name__}")
        # Shipped probes used agent-main before branch rendering was introduced.
        if probe.get("ref") == "agent-main":
            probe["ref"] = PLACEHOLDER
        if not cursor.get("legacy_tags"):
            cursor.pop("legacy_tags", None)
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
    parents = git("rev-list", "--parents", "-n", "1", "HEAD").split()[1:]
    refs = integration_refs()
    snapshots = {}
    names = set(history) | {asset.stem for asset in (ROOT / ASSETS).glob("*.yaml")}
    for name in sorted(names):
        path = (ASSETS / f"{name}.yaml").as_posix()
        records = []
        originals = sources.get(name, {})
        snapshots[name] = {}
        current_path = ROOT / path
        current = record(current_path.read_text(), "") if current_path.exists() else None
        # Include each side's first-parent history at a merge head. In a PR
        # that merges its base, HEAD's first parent is the task branch, not
        # the integration branch. Prefer the parents to the synthetic merge
        # commit when selecting provenance for an identical body.
        starts = parents if len(parents) > 1 else []
        revisions = []
        for start in [*reversed(starts), "HEAD"]:
            revisions.extend(git("log", "--first-parent", "--format=%H", start, "--", path).splitlines())
        revisions = list(dict.fromkeys(revisions))
        if refs:
            # Keep only revisions landed on an integration branch. This
            # still includes merged-base history at a PR head that merges
            # its base: those commits are ancestors of the integration
            # branch even though HEAD's first parent is the task branch.
            landed_revisions = set()
            for ref in refs:
                landed_revisions.update(
                    git("log", "--first-parent", "--format=%H", ref, "--", path).splitlines())
            revisions = [revision for revision in revisions if revision in landed_revisions]
        landed = set(revisions)
        # Reject available draft bodies absent from these histories. Retain
        # unavailable revisions via their fixture, as in a shallow checkout.
        landed_bodies = [record(raw, revision) for revision in revisions
                         if (raw := source(path, revision))]
        candidates = []
        for old in history.get(name, []):
            revision = old["revision"]
            raw = source(path, revision)
            # An older record may name a pre-squash commit whose same body
            # did land under another SHA. Keep its stable provenance even if
            # the draft object happens to exist in this checkout.
            if (revision in landed or raw is None
                    or (raw and any(same_body(record(raw, revision), body)
                                    for body in landed_bodies))):
                candidates.append(revision)
        candidates += revisions
        for revision in candidates:
            raw = source(path, revision)
            if raw == "":
                continue
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
    import sys

    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as exc:
        print(f"update-auto-task-body-history: {exc}", file=sys.stderr)
        sys.exit(1)
