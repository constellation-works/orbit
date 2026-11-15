#!/usr/bin/env bash
# Install the repository plugin into isolated Claude, Codex, and Cursor config roots.
# This intentionally does not exercise npm; scripts/smoke-npm-install.sh owns that chain.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
scratch_root="$(mktemp -d)"
trap 'rm -rf "$scratch_root"' EXIT

"$repo_root/scripts/sync-plugin-skills.sh" --check >/dev/null
"$repo_root/scripts/validate-codex-plugin.sh" "$repo_root" >/dev/null
"$repo_root/scripts/validate-agent-plugin.sh" "$repo_root" >/dev/null

# An invalid candidate must be refused by both plugin validators. Build it
# from the valid assets with a non-semver version, which each validator
# rejects, alongside the files each validator reads beside plugin/.
invalid_root="$scratch_root/invalid-plugin"
mkdir -p "$invalid_root/.claude-plugin" "$invalid_root/.agents/plugins" "$invalid_root/npm"
cp -R "$repo_root/plugin" "$invalid_root/plugin"
cp "$repo_root/npm/package.json" "$invalid_root/npm/package.json"
cp "$repo_root/.claude-plugin/marketplace.json" "$invalid_root/.claude-plugin/marketplace.json"
cp "$repo_root/.agents/plugins/marketplace.json" "$invalid_root/.agents/plugins/marketplace.json"
python3 - "$invalid_root" <<'PY'
import json
import sys
from pathlib import Path

root = Path(sys.argv[1])
for relative in ("plugin/plugin.json", "plugin/.codex-plugin/plugin.json"):
    path = root / relative
    payload = json.loads(path.read_text(encoding="utf-8"))
    payload["version"] = "not-semver"
    path.write_text(json.dumps(payload), encoding="utf-8")
PY
if "$repo_root/scripts/validate-codex-plugin.sh" "$invalid_root" >/dev/null 2>&1; then
  echo "smoke-plugin-install: invalid plugin candidate was not refused by the Codex validator" >&2
  exit 1
fi
if "$repo_root/scripts/validate-agent-plugin.sh" "$invalid_root" >/dev/null 2>&1; then
  echo "smoke-plugin-install: invalid plugin candidate was not refused by the Agent Plugins validator" >&2
  exit 1
fi
echo "smoke-plugin-install: plugin-invalid-install-is-refused: invalid plugin candidate refused by plugin validators"

claude_home="$scratch_root/claude"
codex_home="$scratch_root/codex"
cursor_home="$scratch_root/cursor"
plugin_version="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$repo_root/plugin/plugin.json")"
mkdir -p "$claude_home/plugins/cache/orbit/orbit/$plugin_version" \
  "$codex_home/plugins/cache/orbit/orbit/$plugin_version" \
  "$cursor_home/plugins/local"
cp -R "$repo_root/plugin/." "$claude_home/plugins/cache/orbit/orbit/$plugin_version/"
cp -R "$repo_root/plugin/." "$codex_home/plugins/cache/orbit/orbit/$plugin_version/"
ln -s "$repo_root/plugin" "$cursor_home/plugins/local/orbit"

python3 - "$claude_home" "$codex_home" "$cursor_home" "$plugin_version" <<'PY'
import json
import sys
from pathlib import Path

claude, codex, cursor = map(Path, sys.argv[1:4])
version = sys.argv[4]
expected_args = ["-y", f"@orbit-tools/cli@{version}", "mcp", "serve"]
installs = {
    "Claude Code": claude / f"plugins/cache/orbit/orbit/{version}",
    "Codex": codex / f"plugins/cache/orbit/orbit/{version}",
    "Cursor": cursor / "plugins/local/orbit",
}
for client, root in installs.items():
    skill = root / "skills/orbit/SKILL.md"
    if not skill.is_file() or "name: orbit" not in skill.read_text(encoding="utf-8"):
        raise SystemExit(f"{client}: orbit skill is not visible")

claude_mcp = json.loads((installs["Claude Code"] / ".mcp.json").read_text())
codex_manifest = json.loads((installs["Codex"] / ".codex-plugin/plugin.json").read_text())
cursor_mcp = json.loads((installs["Cursor"] / "mcp.json").read_text())
cursor_manifest = json.loads((installs["Cursor"] / "plugin.json").read_text())
for client, servers in (
    ("Claude Code", claude_mcp.get("mcpServers")),
    ("Codex", codex_manifest.get("mcpServers")),
    ("Cursor", cursor_mcp.get("mcpServers")),
):
    orbit = (servers or {}).get("orbit")
    if not orbit or orbit.get("command") != "npx" or orbit.get("args") != expected_args:
        raise SystemExit(
            f"{client}: Orbit MCP launch must pin npx {' '.join(expected_args)}"
        )

author = cursor_manifest.get("author") if isinstance(cursor_manifest, dict) else None
if not isinstance(author, dict) or author.get("name") != "constellation-works":
    raise SystemExit("Cursor: plugin author must be constellation-works")
if cursor_manifest.get("version") != version:
    raise SystemExit("Cursor: plugin.json version does not match the installed package")
if (installs["Cursor"] / ".cursor-plugin").exists():
    raise SystemExit("Cursor: forbidden .cursor-plugin directory is present")
PY

echo "smoke-plugin-install: Claude Code and Codex scratch installs expose Orbit MCP and skill assets"
echo "smoke-plugin-install: Cursor headless execution is unavailable; local symlink and Agent Plugins 1.0 manifest contract validated"
