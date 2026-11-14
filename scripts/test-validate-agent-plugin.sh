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

python3 - "$validator" <<'PY'
import ast
import sys
from pathlib import Path

validator = Path(sys.argv[1])
text = validator.read_text(encoding="utf-8")
marker = "<<'PY'\n"
start = text.find(marker)
if start < 0:
    print("test-validate-agent-plugin: validator embedded Python not found", file=sys.stderr)
    raise SystemExit(1)
start += len(marker)
end = text.find("\nPY", start)
if end < 0:
    print("test-validate-agent-plugin: validator embedded Python not terminated", file=sys.stderr)
    raise SystemExit(1)
source = text[start:end]
try:
    tree = ast.parse(source, filename=str(validator), feature_version=(3, 9))
except SyntaxError as exc:
    print(f"test-validate-agent-plugin: validator is not Python 3.9 compatible: {exc}", file=sys.stderr)
    raise SystemExit(1)
first = tree.body[0] if tree.body else None
has_future = (
    isinstance(first, ast.ImportFrom)
    and first.module == "__future__"
    and any(alias.name == "annotations" for alias in first.names)
)
if not has_future:
    print("test-validate-agent-plugin: validator must defer annotations for Python 3.9", file=sys.stderr)
    raise SystemExit(1)
PY

fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/orbit-agent-plugin.XXXXXX")"
trap 'rm -rf -- "$fixture_root"' EXIT

# Every fixture, including the per-case clones below, lives under fixture_root
# so concurrent runs sharing one TMPDIR never collide and the trap removes them.
base_fixture="$fixture_root/base"
"$repo_root/scripts/sync-plugin-skills.sh" --check >/dev/null
mkdir -p "$base_fixture/npm" "$base_fixture/.claude-plugin"
cp -R "$repo_root/plugin" "$base_fixture/plugin"
cp "$repo_root/npm/package.json" "$base_fixture/npm/package.json"
cp "$repo_root/.claude-plugin/marketplace.json" "$base_fixture/.claude-plugin/marketplace.json"

"$validator" "$base_fixture"

python3 - "$repo_root" "$validator" "$base_fixture" "$fixture_root" <<'PY'
from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path

repo_root = Path(sys.argv[1]).resolve()
validator = Path(sys.argv[2])
base_fixture = Path(sys.argv[3])
fixture_root = Path(sys.argv[4])

errors: list[str] = []


def run_validator(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(validator), str(root)],
        check=False,
        capture_output=True,
        text=True,
    )


def clone_fixture(name: str) -> Path:
    dest = fixture_root / name
    shutil.copytree(base_fixture, dest, symlinks=True)
    return dest


def expect_failure(root: Path, needle: str, label: str) -> None:
    result = run_validator(root)
    if result.returncode == 0:
        errors.append(f"{label}: expected validator failure")
        return
    combined = result.stdout + result.stderr
    if "Traceback" in combined:
        errors.append(f"{label}: validator leaked a traceback\n{combined}")
        return
    if needle not in combined:
        errors.append(f"{label}: stderr/stdout missing {needle!r}\n{combined}")


def expect_success(root: Path, label: str) -> None:
    result = run_validator(root)
    if result.returncode != 0:
        errors.append(f"{label}: expected validator success, got {result.returncode}\n{result.stdout + result.stderr}")


def set_orbit_cwd(root: Path, cwd: str | None) -> None:
    path = root / "plugin" / "mcp.json"
    payload = json.loads(path.read_text(encoding="utf-8"))
    if cwd is None:
        payload["mcpServers"]["orbit"].pop("cwd", None)
    else:
        payload["mcpServers"]["orbit"]["cwd"] = cwd
    path.write_text(json.dumps(payload), encoding="utf-8")


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

# The Orbit launch is required on every packaged manifest, not only when args
# are already a list and the server key is named orbit.
def write_json(path: Path, payload: dict) -> None:
    path.write_text(json.dumps(payload), encoding="utf-8")


def load(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


broken = clone_fixture("absent-orbit-server")
payload = load(broken / "plugin" / "mcp.json")
payload["mcpServers"] = {"helper": {"type": "stdio", "command": "true", "args": []}}
write_json(broken / "plugin" / "mcp.json", payload)
expect_failure(broken, "mcpServers.orbit", "absent orbit server")

broken = clone_fixture("absent-orbit-args")
payload = load(broken / "plugin" / "mcp.json")
del payload["mcpServers"]["orbit"]["args"]
write_json(broken / "plugin" / "mcp.json", payload)
expect_failure(broken, "mcpServers.orbit.args", "absent orbit args")

broken = clone_fixture("malformed-orbit-args")
payload = load(broken / "plugin" / "mcp.json")
payload["mcpServers"]["orbit"]["args"] = {"cmd": "serve"}
write_json(broken / "plugin" / "mcp.json", payload)
expect_failure(broken, "mcpServers.orbit.args", "malformed orbit args")

broken = clone_fixture("empty-orbit-args")
payload = load(broken / "plugin" / "mcp.json")
payload["mcpServers"]["orbit"]["args"] = []
write_json(broken / "plugin" / "mcp.json", payload)
expect_failure(broken, "MCP launch must be", "empty orbit args")

for label, relative in (
    ("claude", "plugin/.mcp.json"),
    ("codex", "plugin/.codex-plugin/plugin.json"),
):
    broken = clone_fixture(f"absent-orbit-{label}")
    payload = load(broken / relative)
    payload["mcpServers"] = {"helper": {"command": "true", "args": []}}
    write_json(broken / relative, payload)
    expect_failure(broken, "mcpServers.orbit", f"absent orbit server in {label}")

    broken = clone_fixture(f"absent-orbit-args-{label}")
    payload = load(broken / relative)
    del payload["mcpServers"]["orbit"]["args"]
    write_json(broken / relative, payload)
    expect_failure(broken, "mcpServers.orbit.args", f"absent orbit args in {label}")

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

# Negative: non-UTF8 plugin.json must fail cleanly, not with a traceback.
broken = clone_fixture("nonutf8-plugin-manifest")
(broken / "plugin" / "plugin.json").write_bytes(b"\xff\xfe\x00bad")
expect_failure(broken, "not valid JSON", "non-UTF8 plugin.json")

# Negative: non-UTF8 SKILL.md must fail cleanly, not with a traceback.
broken = clone_fixture("nonutf8-skill")
(broken / "plugin" / "skills" / "orbit" / "SKILL.md").write_bytes(b"\xff\xfe\x00bad")
expect_failure(broken, "cannot be read", "non-UTF8 SKILL.md")

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

# Negative: placeholder cwd traversal escapes its named root.
for label, cwd in (
    ("root-traversal", "${PLUGIN_ROOT}/../../outside"),
    ("data-traversal", "${PLUGIN_DATA}/../../outside"),
    ("nested-traversal", "${PLUGIN_ROOT}/safe/../../outside"),
    ("dotdot-suffix", "${PLUGIN_ROOT}/.."),
):
    broken = clone_fixture(f"cwd-{label}")
    set_orbit_cwd(broken, cwd)
    expect_failure(broken, "must be plugin-relative", f"placeholder cwd traversal {label}")

# Negative: malformed placeholder suffixes and absolute suffixes.
for label, cwd in (
    ("missing-slash", "${PLUGIN_ROOT}extra"),
    ("data-missing-slash", "${PLUGIN_DATA}suffix"),
    ("empty-descendant", "${PLUGIN_ROOT}/"),
    ("double-slash", "${PLUGIN_ROOT}//etc/passwd"),
    ("data-double-slash", "${PLUGIN_DATA}//etc"),
    ("drive-suffix", "${PLUGIN_ROOT}/C:/Windows"),
    ("backslash-suffix", "${PLUGIN_ROOT}/safe\\evil"),
):
    broken = clone_fixture(f"cwd-{label}")
    set_orbit_cwd(broken, cwd)
    expect_failure(broken, "must be plugin-relative", f"placeholder cwd suffix {label}")

# Negative: a second server entry with traversal cwd is also rejected.
broken = clone_fixture("cwd-second-server")
payload = json.loads((broken / "plugin" / "mcp.json").read_text(encoding="utf-8"))
payload["mcpServers"]["helper"] = {
    "type": "stdio",
    "command": "npx",
    "args": ["-y", "helper", "mcp", "serve"],
    "cwd": "${PLUGIN_ROOT}/../../outside",
}
(broken / "plugin" / "mcp.json").write_text(json.dumps(payload), encoding="utf-8")
expect_failure(broken, "mcpServers.helper.cwd must be plugin-relative", "second server cwd traversal")

# Every additional server must have a usable stdio launch.
for label, launch in (
    ("missing-command", {}),
    ("blank-command", {"command": "  "}),
    ("non-string-command", {"command": []}),
    ("bad-args", {"command": "helper", "args": "not-an-array"}),
    ("bad-arg-item", {"command": "helper", "args": [42]}),
):
    broken = clone_fixture(f"helper-{label}")
    path = broken / "plugin" / "mcp.json"
    payload = json.loads(path.read_text(encoding="utf-8"))
    payload["mcpServers"]["helper"] = {"type": "stdio", **launch}
    path.write_text(json.dumps(payload), encoding="utf-8")
    expect_failure(broken, "mcpServers.helper.", f"helper {label}")

good = clone_fixture("helper-valid")
path = good / "plugin" / "mcp.json"
payload = json.loads(path.read_text(encoding="utf-8"))
payload["mcpServers"]["helper"] = {"type": "stdio", "command": "helper", "args": ["serve"]}
path.write_text(json.dumps(payload), encoding="utf-8")
expect_success(good, "valid helper with independent launch")

# Positive: bare placeholders and safe descendants are accepted.
for label, cwd in (
    ("bare-root", "${PLUGIN_ROOT}"),
    ("bare-data", "${PLUGIN_DATA}"),
    ("root-descendant", "${PLUGIN_ROOT}/work"),
    ("data-descendant", "${PLUGIN_DATA}/cache/v1"),
    ("dot-relative", "./work"),
):
    good = clone_fixture(f"cwd-ok-{label}")
    set_orbit_cwd(good, cwd)
    expect_success(good, f"legitimate cwd {label}")

if errors:
    print("test-validate-agent-plugin: failed", file=sys.stderr)
    for error in errors:
        print(f"- {error}", file=sys.stderr)
    raise SystemExit(1)

print("test-validate-agent-plugin: local Cursor plugin fixture and validator cases passed")
PY
