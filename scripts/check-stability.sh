#!/usr/bin/env bash
set -euo pipefail

# Validates that every orbit-* workspace crate declares its stability tier via
# `[package.metadata.orbit] stability = "<tier>"` in its Cargo.toml. Prints a
# `crate \t tier` table on success; fails closed (non-zero, named offenders)
# when the marker is missing or invalid.

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

cargo metadata --format-version 1 --no-deps --manifest-path "$repo_root/Cargo.toml" |
  python3 -c '
import json
import sys

ALLOWED = {"stable", "experimental", "internal"}

metadata = json.load(sys.stdin)
workspace_members = set(metadata["workspace_members"])

orbit_packages = sorted(
    (
        (pkg["name"], pkg.get("metadata"))
        for pkg in metadata["packages"]
        if pkg["id"] in workspace_members and pkg["name"].startswith("orbit-")
    ),
    key=lambda package: package[0],
)

errors = []
rows = []
for name, pkg_metadata in orbit_packages:
    orbit_meta = (pkg_metadata or {}).get("orbit") if isinstance(pkg_metadata, dict) else None
    tier = orbit_meta.get("stability") if isinstance(orbit_meta, dict) else None
    if tier is None:
        errors.append(f"{name}: missing [package.metadata.orbit] stability = \"<tier>\"")
        continue
    # An array or table is unhashable, so test the type before the set.
    if not isinstance(tier, str) or tier not in ALLOWED:
        errors.append(
            f"{name}: invalid stability {json.dumps(tier)} (allowed: {sorted(ALLOWED)})"
        )
        continue
    rows.append((name, tier))

if errors:
    for err in errors:
        print(f"stability check failed: {err}", file=sys.stderr)
    sys.exit(1)

if not rows:
    print("no orbit-* workspace crates discovered from cargo metadata", file=sys.stderr)
    sys.exit(1)

width = max(len(name) for name, _ in rows)
header = "crate".ljust(width)
print(f"{header}\ttier")
for name, tier in rows:
    print(f"{name.ljust(width)}\t{tier}")
'
