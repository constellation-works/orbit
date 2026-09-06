#!/usr/bin/env bash
# Exercise the Cursor marketplace follow-up reminder without network or writes
# to Cursor, npm, or GitHub.
set -euo pipefail

repo_root="${1:-${ORBIT_REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}}"
script="$repo_root/scripts/cursor-marketplace-followup.sh"
workflow="$repo_root/.github/workflows/release.yml"

if [[ ! -x "$script" && ! -f "$script" ]]; then
  echo "test-cursor-marketplace-followup: missing $script" >&2
  exit 1
fi

require() {
  local description="$1"
  local text="$2"
  if ! grep --fixed-strings --quiet -- "$text" "$workflow"; then
    echo "test-cursor-marketplace-followup: workflow missing $description" >&2
    exit 1
  fi
}

forbid() {
  local description="$1"
  local text="$2"
  if grep --fixed-strings --quiet -- "$text" "$workflow"; then
    echo "test-cursor-marketplace-followup: workflow has forbidden $description" >&2
    exit 1
  fi
}

require "follow-up job name" "cursor-marketplace-followup:"
require "follow-up after publish" "needs: publish-release"
require "non-blocking follow-up" "continue-on-error: true"
require "follow-up script invocation" "./scripts/cursor-marketplace-followup.sh"
require "catalog-not-published comment" "does not publish the Cursor catalog"

# publish-release must not wait on the reminder, and sibling post-publish jobs
# must keep running if the reminder is unacknowledged.
if grep -n --fixed-strings -- 'needs: cursor-marketplace-followup' "$workflow"; then
  echo "test-cursor-marketplace-followup: a release job depends on the reminder" >&2
  exit 1
fi
if grep --fixed-strings --quiet -- 'cursor.com/marketplace' "$workflow"; then
  echo "test-cursor-marketplace-followup: workflow must not submit marketplace forms" >&2
  exit 1
fi
forbid "npm publish from release workflow reminder" "npm publish"
forbid "mail from release workflow" "marketplace-publishing@cursor.com"

python3 - "$repo_root" "$script" <<'PY'
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

repo_root = Path(sys.argv[1]).resolve()
script = Path(sys.argv[2])
errors: list[str] = []


def run(
    ack_root: Path,
    version: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.pop("GITHUB_ACTIONS", None)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [
            str(script),
            "--repo-root",
            str(repo_root),
            "--ack-root",
            str(ack_root),
            "--version",
            version,
        ],
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )


def expect_missing(result: subprocess.CompletedProcess[str], label: str) -> None:
    combined = result.stdout + result.stderr
    if result.returncode == 0:
        errors.append(f"{label}: expected failure for missing acknowledgement")
        return
    for needle in (
        "no acknowledgement",
        "https://cursor.com/marketplace/publish",
        "https://github.com/constellation-works/orbit",
        "plugin/",
        "2280865",
        "0.5.1",
        "Cursor plugin search",
        "marketplace-publishing@cursor.com",
        "not that the catalog is live",
        "Do not retag",
    ):
        if needle not in combined:
            errors.append(f"{label}: missing checklist text {needle!r}\n{combined}")


scratch = Path(tempfile.mkdtemp(prefix="orbit-cursor-followup."))
try:
    missing_root = scratch / "missing"
    missing_root.mkdir()
    missing = run(missing_root, "0.19.0")
    expect_missing(missing, "missing ack directory")

    empty_root = scratch / "empty"
    empty_root.mkdir()
    empty = run(empty_root, "0.19.0")
    expect_missing(empty, "missing ack file")

    wrong_root = scratch / "wrong"
    wrong_root.mkdir()
    (wrong_root / "0.19.0.ack").write_text("version=0.5.1\n", encoding="utf-8")
    wrong = run(wrong_root, "0.19.0")
    if wrong.returncode == 0:
        errors.append("wrong-version ack: expected failure")
    elif "not for v0.19.0" not in (wrong.stdout + wrong.stderr):
        errors.append(f"wrong-version ack: missing mismatch text\n{wrong.stdout}{wrong.stderr}")

    ack_root = scratch / "acked"
    ack_root.mkdir()
    (ack_root / "0.19.0.ack").write_text(
        "# human follow-up only; catalog publication is separate\nversion=0.19.0\n",
        encoding="utf-8",
    )
    acked = run(ack_root, "0.19.0")
    if acked.returncode != 0:
        errors.append(f"matching ack: expected success\n{acked.stdout}{acked.stderr}")
    elif "acknowledgement is present" not in (acked.stdout + acked.stderr):
        errors.append("matching ack: missing success text")
    if "does not mean the Cursor catalog is published" not in (acked.stdout + acked.stderr):
        errors.append("matching ack: must not imply catalog publication")

    # The reminder must not create files or contact external services.
    before = {path.relative_to(scratch) for path in scratch.rglob("*")}
    run(missing_root, "0.20.0")
    after = {path.relative_to(scratch) for path in scratch.rglob("*")}
    if after != before:
        errors.append(f"script wrote files: {sorted(after - before)}")

    script_text = script.read_text(encoding="utf-8")
    for forbidden in ("curl ", "npm publish", "gh release", "mailx", "sendmail"):
        if forbidden in script_text:
            errors.append(f"follow-up script contains forbidden operation {forbidden!r}")

    # Current repository tree is unacknowledged for the live version.
    live_version = json.loads((repo_root / "plugin" / "plugin.json").read_text(encoding="utf-8"))[
        "version"
    ]
    live_ack = repo_root / ".github" / "cursor-marketplace-followup" / f"{live_version}.ack"
    if live_ack.is_file():
        errors.append(
            f"{live_ack.relative_to(repo_root)} must not exist while listing 2280865 is stale"
        )
finally:
    shutil.rmtree(scratch, ignore_errors=True)

if errors:
    print("test-cursor-marketplace-followup: failed", file=sys.stderr)
    for error in errors:
        print(f"- {error}", file=sys.stderr)
    raise SystemExit(1)

print("test-cursor-marketplace-followup: missing and acknowledged states passed")
print("test-cursor-marketplace-followup: release workflow reminder is non-blocking")
PY
