#!/usr/bin/env bash
# Exercise the repository-owned Codex plugin validator at its supported
# Python interpreter boundary.
set -euo pipefail

repo_root="${1:-${ORBIT_REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}}"
validator="$repo_root/scripts/validate-codex-plugin.sh"

if ! command -v python3 >/dev/null 2>&1; then
  echo "test-validate-codex-plugin: required binary 'python3' not on PATH" >&2
  exit 2
fi

if ! grep -Fqx 'from __future__ import annotations' "$validator"; then
  echo "test-validate-codex-plugin: validator must defer annotations for Python 3.9" >&2
  exit 1
fi

python3 - <<'PY'
from __future__ import annotations

from typing import Any


def accepts_optional_mapping(value: dict[str, Any] | None) -> list[str]:
    return [] if value is None else list(value)


assert accepts_optional_mapping(None) == []
assert accepts_optional_mapping({"supported": True}) == ["supported"]
PY

fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/orbit-codex-plugin.XXXXXX")"
trap 'rm -rf -- "$fixture_root"' EXIT

"$repo_root/scripts/sync-plugin-skills.sh" --check >/dev/null
cp -R "$repo_root/plugin" "$fixture_root/plugin"
mkdir -p "$fixture_root/.agents/plugins"
cp "$repo_root/.agents/plugins/marketplace.json" "$fixture_root/.agents/plugins/marketplace.json"

"$validator" "$fixture_root"

python3 - "$repo_root" "$validator" "$fixture_root" <<'PY'
from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path

repo_root = Path(sys.argv[1]).resolve()
validator = Path(sys.argv[2])
base_fixture = Path(sys.argv[3])
errors: list[str] = []


def run_validator(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(validator), str(root)],
        check=False,
        capture_output=True,
        text=True,
    )


def clone_fixture(name: str) -> Path:
    dest = base_fixture.parent / name
    if dest.exists():
        shutil.rmtree(dest)
    shutil.copytree(base_fixture, dest, symlinks=True)
    return dest


def expect_failure(root: Path, needle: str, label: str) -> None:
    result = run_validator(root)
    if result.returncode == 0:
        errors.append(f"{label}: expected validator failure")
        return
    combined = result.stdout + result.stderr
    if needle not in combined:
        errors.append(f"{label}: stderr/stdout missing {needle!r}\n{combined}")


broken = clone_fixture("codex-stale-latest-pin")
payload = json.loads((broken / "plugin" / ".codex-plugin" / "plugin.json").read_text(encoding="utf-8"))
payload["mcpServers"]["orbit"]["args"] = ["-y", "@orbit-tools/cli@latest", "mcp", "serve"]
(broken / "plugin" / ".codex-plugin" / "plugin.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "stale launch pin", "stale @latest pin")

broken = clone_fixture("codex-missing-manifest")
(broken / "plugin" / ".codex-plugin" / "plugin.json").unlink()
expect_failure(broken, "plugin/.codex-plugin/plugin.json is missing", "missing Codex manifest")

broken = clone_fixture("codex-owner-drift")
payload = json.loads((broken / "plugin" / ".codex-plugin" / "plugin.json").read_text(encoding="utf-8"))
payload["author"] = {"name": "danieljhkim", "url": "https://github.com/danieljhkim"}
(broken / "plugin" / ".codex-plugin" / "plugin.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "constellation-works", "owner drift")

if errors:
    print("test-validate-codex-plugin: failed", file=sys.stderr)
    for error in errors:
        print(f"- {error}", file=sys.stderr)
    raise SystemExit(1)

print("test-validate-codex-plugin: current package and validator cases passed")
PY
