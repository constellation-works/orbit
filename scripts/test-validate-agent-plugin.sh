#!/usr/bin/env bash
# Exercise the repository-owned Agent Plugin validator and the Cursor local
# plugins-directory discovery shape.
set -euo pipefail

repo_root="${1:-${ORBIT_REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}}"
validator="$repo_root/scripts/validate-agent-plugin.sh"

if ! command -v python3 >/dev/null 2>&1; then
  echo "test-validate-agent-plugin: required binary 'python3' not on PATH" >&2
  exit 2
fi

if ! grep -Fqx 'from __future__ import annotations' "$validator"; then
  echo "test-validate-agent-plugin: validator must defer annotations for Python 3.9" >&2
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

fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/orbit-agent-plugin.XXXXXX")"
trap 'rm -rf -- "$fixture_root"' EXIT

"$repo_root/scripts/sync-plugin-skills.sh" --check >/dev/null
cp -R "$repo_root/plugin" "$fixture_root/plugin"
mkdir -p "$fixture_root/npm" "$fixture_root/.claude-plugin"
cp "$repo_root/npm/package.json" "$fixture_root/npm/package.json"
cp "$repo_root/.claude-plugin/marketplace.json" "$fixture_root/.claude-plugin/marketplace.json"

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


# Cursor discovers Agent Plugins from ~/.cursor/plugins/local/<name>.
home = base_fixture / "fake-home"
cursor_plugin = home / ".cursor" / "plugins" / "local" / "orbit"
if cursor_plugin.exists():
    shutil.rmtree(cursor_plugin)
shutil.copytree(base_fixture / "plugin", cursor_plugin, symlinks=True)

plugin_manifest = json.loads((cursor_plugin / "plugin.json").read_text(encoding="utf-8"))
mcp = json.loads((cursor_plugin / "mcp.json").read_text(encoding="utf-8"))
skill = cursor_plugin / "skills" / "orbit" / "SKILL.md"
if plugin_manifest.get("name") != "orbit":
    errors.append("local Cursor plugin manifest name is not orbit")
if "$schema" not in plugin_manifest:
    errors.append("local Cursor plugin.json is missing $schema")
if (cursor_plugin / ".cursor-plugin").exists():
    errors.append("local Cursor plugin must not grow a .cursor-plugin surface")
if not skill.is_file():
    errors.append("local Cursor plugin does not expose plugin/skills/orbit/SKILL.md")
npm_version = json.loads((base_fixture / "npm" / "package.json").read_text(encoding="utf-8"))["version"]
expected_args = ["-y", f"@orbit-tools/cli@{npm_version}", "mcp", "serve"]
orbit_mcp = mcp.get("mcpServers", {}).get("orbit", {})
if orbit_mcp.get("command") != "npx" or orbit_mcp.get("args") != expected_args:
    errors.append("local Cursor plugin MCP launch drifted from the release-version npx pin")
if plugin_manifest.get("author", {}).get("name") != "constellation-works":
    errors.append("local Cursor plugin author is not constellation-works")

plugin_blob = (cursor_plugin / "plugin.json").read_text(encoding="utf-8")
mcp_blob = (cursor_plugin / "mcp.json").read_text(encoding="utf-8")
for blob, label in ((plugin_blob, "plugin.json"), (mcp_blob, "mcp.json")):
    if str(repo_root) in blob or str(Path.home()) in blob:
        errors.append(f"{label} embeds a repository- or user-specific absolute path")

# Negative: missing official schema.
broken = clone_fixture("missing-schema")
payload = json.loads((broken / "plugin" / "plugin.json").read_text(encoding="utf-8"))
del payload["$schema"]
(broken / "plugin" / "plugin.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "$schema", "missing $schema")

# Negative: version drift against Claude/Codex/npm.
broken = clone_fixture("version-drift")
payload = json.loads((broken / "plugin" / "plugin.json").read_text(encoding="utf-8"))
payload["version"] = "0.0.0"
(broken / "plugin" / "plugin.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "drifted", "version drift")

# Negative: MCP command no longer matches Claude/Codex.
broken = clone_fixture("mcp-drift")
payload = json.loads((broken / "plugin" / "mcp.json").read_text(encoding="utf-8"))
payload["mcpServers"]["orbit"]["command"] = "node"
(broken / "plugin" / "mcp.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "MCP launch", "mcp command drift")

# Negative: stale @latest launch pin.
broken = clone_fixture("stale-latest-pin")
payload = json.loads((broken / "plugin" / "mcp.json").read_text(encoding="utf-8"))
payload["mcpServers"]["orbit"]["args"] = ["-y", "@orbit-tools/cli@latest", "mcp", "serve"]
(broken / "plugin" / "mcp.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "stale launch pin", "stale @latest pin")

# Negative: historical 0.5.1 launch pin.
broken = clone_fixture("stale-051-pin")
payload = json.loads((broken / "plugin" / "mcp.json").read_text(encoding="utf-8"))
payload["mcpServers"]["orbit"]["args"] = ["-y", "@orbit-tools/cli@0.5.1", "mcp", "serve"]
(broken / "plugin" / "mcp.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "stale launch pin", "stale 0.5.1 pin")

# Negative: missing required mcp.json.
broken = clone_fixture("missing-mcp")
(broken / "plugin" / "mcp.json").unlink()
expect_failure(broken, "plugin/mcp.json is missing", "missing mcp.json")

# Negative: missing required plugin.json.
broken = clone_fixture("missing-plugin-manifest")
(broken / "plugin" / "plugin.json").unlink()
expect_failure(broken, "plugin/plugin.json is missing", "missing plugin.json")

# Negative: malformed plugin.json.
broken = clone_fixture("malformed-plugin-manifest")
(broken / "plugin" / "plugin.json").write_text("{not-json", encoding="utf-8")
expect_failure(broken, "not valid JSON", "malformed plugin.json")

# Negative: ownership drift away from constellation-works.
broken = clone_fixture("owner-drift")
payload = json.loads((broken / "plugin" / "plugin.json").read_text(encoding="utf-8"))
payload["author"] = {"name": "danieljhkim", "url": "https://github.com/danieljhkim"}
(broken / "plugin" / "plugin.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "constellation-works", "owner drift")

# Negative: absolute path in the Agent Plugin MCP config.
broken = clone_fixture("absolute-path")
payload = json.loads((broken / "plugin" / "mcp.json").read_text(encoding="utf-8"))
payload["mcpServers"]["orbit"]["command"] = "/usr/bin/npx"
(broken / "plugin" / "mcp.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "absolute filesystem path", "absolute mcp path")

# Negative: missing shared Orbit skill.
broken = clone_fixture("missing-skill")
skill_path = broken / "plugin" / "skills" / "orbit" / "SKILL.md"
skill_path.unlink()
expect_failure(broken, "SKILL.md", "missing orbit skill")

# Negative: Cursor-specific surface that the task forbids.
broken = clone_fixture("cursor-plugin-dir")
(broken / "plugin" / ".cursor-plugin").mkdir()
(broken / "plugin" / ".cursor-plugin" / "plugin.json").write_text("{}", encoding="utf-8")
expect_failure(broken, ".cursor-plugin", "unexpected .cursor-plugin surface")

if errors:
    print("test-validate-agent-plugin: failed", file=sys.stderr)
    for error in errors:
        print(f"- {error}", file=sys.stderr)
    raise SystemExit(1)

print("test-validate-agent-plugin: local Cursor plugin fixture and validator cases passed")
PY
