#!/usr/bin/env bash
# Validate the repository-owned Agent Plugins 1.0 package that Cursor loads.
#
# This lives in-repo so CI does not depend on a developer Cursor checkout.
# It checks the official 1.0.0 plugin.json / mcp.json contracts, required
# skill and MCP components, relative/portable paths, manifest version parity
# with Claude/Codex/npm, and MCP launch parity with the existing plugin
# integrations.
set -euo pipefail

repo_root="${1:-${ORBIT_REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}}"

if ! command -v python3 >/dev/null 2>&1; then
  echo "validate-agent-plugin: required binary 'python3' not on PATH" >&2
  exit 2
fi

python3 - "$repo_root" <<'PY'
from __future__ import annotations

import json
import re
import sys
from pathlib import Path, PurePosixPath
from typing import Any

repo = Path(sys.argv[1]).resolve()
plugin_root = repo / "plugin"
manifest_path = plugin_root / "plugin.json"
mcp_path = plugin_root / "mcp.json"
claude_manifest_path = plugin_root / ".claude-plugin" / "plugin.json"
codex_manifest_path = plugin_root / ".codex-plugin" / "plugin.json"
claude_mcp_path = plugin_root / ".mcp.json"
npm_package_path = repo / "npm" / "package.json"
claude_marketplace_path = repo / ".claude-plugin" / "marketplace.json"

PLUGIN_SCHEMA = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json"
MCP_SCHEMA = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json"
PLUGIN_NAME_RE = re.compile(
    r"^(?!.*(?:--|\.\.))[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$"
)
SEMVER_RE = re.compile(
    r"^(0|[1-9]\d*)\."
    r"(0|[1-9]\d*)\."
    r"(0|[1-9]\d*)"
    r"(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\."
    r"(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)
EXPECTED_MCP_COMMAND = "npx"
EXPECTED_OWNER_NAME = "constellation-works"
EXPECTED_OWNER_URL = "https://github.com/constellation-works"
PLUGIN_MANIFEST_KEYS = {
    "$schema",
    "name",
    "version",
    "description",
    "author",
    "homepage",
    "repository",
    "license",
    "keywords",
    "extensions",
}
MCP_TOP_KEYS = {"$schema", "mcpServers"}
STDIO_KEYS = {"type", "command", "args", "env", "cwd"}

errors: list[str] = []


def load_json(path: Path, label: str) -> dict[str, Any] | None:
    if not path.is_file():
        errors.append(f"{label} is missing")
        return None
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        errors.append(f"{label} is not valid JSON: {exc}")
        return None
    if not isinstance(payload, dict):
        errors.append(f"{label} must contain a JSON object")
        return None
    return payload


def require_string(payload: dict[str, Any], key: str, label: str) -> str | None:
    value = payload.get(key)
    if not isinstance(value, str) or not value.strip():
        errors.append(f"{label}.{key} must be a non-empty string")
        return None
    return value


def require_owner(payload: dict[str, Any] | None, label: str) -> None:
    if not isinstance(payload, dict):
        errors.append(f"{label} must be an object")
        return
    if payload.get("name") != EXPECTED_OWNER_NAME:
        errors.append(f"{label}.name must be {EXPECTED_OWNER_NAME!r}")
    if payload.get("url") != EXPECTED_OWNER_URL:
        errors.append(f"{label}.url must be {EXPECTED_OWNER_URL!r}")


def reject_todos(value: Any, path: str) -> None:
    if isinstance(value, str):
        if "[TODO:" in value:
            errors.append(f"{path} still contains a [TODO: ...] placeholder")
    elif isinstance(value, list):
        for index, item in enumerate(value):
            reject_todos(item, f"{path}[{index}]")
    elif isinstance(value, dict):
        for key, item in value.items():
            reject_todos(item, f"{path}.{key}")


def reject_user_specific_paths(value: Any, path: str) -> None:
    if isinstance(value, str):
        if "CLAUDE_PROJECT_DIR" in value:
            errors.append(f"{path} must not reference CLAUDE_PROJECT_DIR")
        if str(Path.home()) in value:
            errors.append(f"{path} must not embed a user-specific absolute home path")
        if value.startswith("/") or (len(value) >= 3 and value[1] == ":" and value[0].isalpha()):
            errors.append(f"{path} must not use an absolute filesystem path")
        return
    if isinstance(value, list):
        for index, item in enumerate(value):
            reject_user_specific_paths(item, f"{path}[{index}]")
        return
    if isinstance(value, dict):
        for key, item in value.items():
            if key in {"$schema", "url", "homepage", "repository"}:
                continue
            if isinstance(item, dict) and "url" in item and key != "author":
                rest = {k: v for k, v in item.items() if k != "url"}
                reject_user_specific_paths(rest, f"{path}.{key}")
                continue
            reject_user_specific_paths(item, f"{path}.{key}")


def mcp_launch(payload: dict[str, Any] | None, label: str) -> tuple[str, list[str]] | None:
    if payload is None:
        return None
    servers = payload.get("mcpServers")
    if not isinstance(servers, dict):
        errors.append(f"{label}.mcpServers must be an object")
        return None
    orbit = servers.get("orbit")
    if not isinstance(orbit, dict):
        errors.append(f"{label}.mcpServers.orbit must be an object")
        return None
    command = orbit.get("command")
    args = orbit.get("args")
    if not isinstance(command, str) or not command.strip():
        errors.append(f"{label}.mcpServers.orbit.command must be a non-empty string")
        return None
    if not isinstance(args, list) or not all(isinstance(arg, str) and arg.strip() for arg in args):
        errors.append(f"{label}.mcpServers.orbit.args must be an array of non-empty strings")
        return None
    return command, args


def validate_skill_frontmatter(skill_dir: Path) -> None:
    skill_path = skill_dir / "SKILL.md"
    if not skill_path.is_file():
        errors.append(f"skill {skill_dir.name} is missing SKILL.md")
        return
    text = skill_path.read_text(encoding="utf-8")
    if not text.startswith("---\n"):
        errors.append(f"skill {skill_dir.name} must start with YAML frontmatter")
        return
    end = text.find("\n---", 4)
    if end == -1:
        errors.append(f"skill {skill_dir.name} frontmatter is not closed")
        return
    fields: dict[str, str] = {}
    for line in text[4:end].splitlines():
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        fields[key.strip()] = value.strip().strip('"').strip("'")
    for key in ("name", "description"):
        if not fields.get(key):
            errors.append(f"skill {skill_dir.name} frontmatter field {key} must be non-empty")


def relative_plugin_cwd(raw: Any) -> bool:
    if not isinstance(raw, str) or not raw:
        return False
    if raw.startswith("${PLUGIN_ROOT}") or raw.startswith("${PLUGIN_DATA}"):
        return True
    path = PurePosixPath(raw.replace("\\", "/"))
    return not path.is_absolute() and ".." not in path.parts and raw.startswith("./")


manifest = load_json(manifest_path, "plugin/plugin.json")
mcp = load_json(mcp_path, "plugin/mcp.json")
claude_manifest = load_json(claude_manifest_path, "plugin/.claude-plugin/plugin.json")
codex_manifest = load_json(codex_manifest_path, "plugin/.codex-plugin/plugin.json")
claude_mcp = load_json(claude_mcp_path, "plugin/.mcp.json")
npm_package = load_json(npm_package_path, "npm/package.json")
claude_marketplace = load_json(claude_marketplace_path, ".claude-plugin/marketplace.json")

npm_version = None
if npm_package is not None:
    raw_npm_version = npm_package.get("version")
    if isinstance(raw_npm_version, str) and raw_npm_version.strip() and SEMVER_RE.fullmatch(raw_npm_version):
        npm_version = raw_npm_version
    else:
        errors.append("npm/package.json.version must use strict semver")

expected_mcp_args = (
    ["-y", f"@orbit-tools/cli@{npm_version}", "mcp", "serve"] if npm_version is not None else None
)

if (plugin_root / ".cursor-plugin").exists():
    errors.append(
        "plugin/.cursor-plugin must not exist; Cursor loads the Agent Plugins "
        "surface from plugin/plugin.json, plugin/mcp.json, and plugin/skills/"
    )

if manifest is not None:
    reject_todos(manifest, "plugin/plugin.json")
    reject_user_specific_paths(manifest, "plugin/plugin.json")
    for key in sorted(set(manifest) - PLUGIN_MANIFEST_KEYS):
        errors.append(f"plugin/plugin.json field {key} is not supported by Agent Plugins 1.0")
    if manifest.get("$schema") != PLUGIN_SCHEMA:
        errors.append("plugin/plugin.json.$schema must be the Agent Plugins 1.0.0 plugin schema")
    name = require_string(manifest, "name", "plugin/plugin.json")
    if name is not None:
        if name != "orbit":
            errors.append("plugin/plugin.json.name must be 'orbit'")
        if len(name) > 64 or PLUGIN_NAME_RE.fullmatch(name) is None:
            errors.append("plugin/plugin.json.name must match the Agent Plugins 1.0 name pattern")
    version = require_string(manifest, "version", "plugin/plugin.json")
    if version is not None and SEMVER_RE.fullmatch(version) is None:
        errors.append("plugin/plugin.json.version must use strict semver")
    require_string(manifest, "description", "plugin/plugin.json")
    require_string(manifest, "license", "plugin/plugin.json")
    author = manifest.get("author")
    if not isinstance(author, dict):
        errors.append("plugin/plugin.json.author must be an object")
    else:
        require_string(author, "name", "plugin/plugin.json.author")
        require_owner(author, "plugin/plugin.json.author")
        for extra in sorted(set(author) - {"name", "email", "url"}):
            errors.append(f"plugin/plugin.json.author field {extra} is not supported")
    keywords = manifest.get("keywords")
    if keywords is not None and (
        not isinstance(keywords, list) or not all(isinstance(item, str) and item.strip() for item in keywords)
    ):
        errors.append("plugin/plugin.json.keywords must be an array of non-empty strings")

    versions: list[tuple[str, str]] = []
    if version is not None:
        versions.append(("plugin/plugin.json", version))
    for path, payload, label in (
        (claude_manifest_path, claude_manifest, "plugin/.claude-plugin/plugin.json"),
        (codex_manifest_path, codex_manifest, "plugin/.codex-plugin/plugin.json"),
        (npm_package_path, npm_package, "npm/package.json"),
    ):
        if payload is None:
            continue
        other = payload.get("version")
        if not isinstance(other, str) or not other.strip():
            errors.append(f"{label}.version must be a non-empty string")
            continue
        versions.append((label, other))
    distinct = {item[1] for item in versions}
    if len(distinct) > 1:
        rendered = ", ".join(f"{label}={ver}" for label, ver in versions)
        errors.append(f"plugin manifest versions drifted: {rendered}")

if mcp is not None:
    reject_todos(mcp, "plugin/mcp.json")
    reject_user_specific_paths(mcp, "plugin/mcp.json")
    for key in sorted(set(mcp) - MCP_TOP_KEYS):
        errors.append(f"plugin/mcp.json field {key} is not supported by Agent Plugins 1.0")
    if mcp.get("$schema") != MCP_SCHEMA:
        errors.append("plugin/mcp.json.$schema must be the Agent Plugins 1.0.0 MCP schema")
    servers = mcp.get("mcpServers")
    if not isinstance(servers, dict) or not servers:
        errors.append("plugin/mcp.json.mcpServers must be a non-empty object")
    else:
        if "orbit" not in servers:
            errors.append("plugin/mcp.json.mcpServers must register the orbit server")
        for server_name, server in servers.items():
            if not isinstance(server_name, str) or not server_name.strip():
                errors.append("plugin/mcp.json.mcpServers keys must be non-empty strings")
            if not isinstance(server, dict):
                errors.append(f"plugin/mcp.json.mcpServers.{server_name} must be an object")
                continue
            server_type = server.get("type")
            if server_type != "stdio":
                errors.append(f"plugin/mcp.json.mcpServers.{server_name}.type must be 'stdio'")
            for extra in sorted(set(server) - STDIO_KEYS):
                errors.append(
                    f"plugin/mcp.json.mcpServers.{server_name} field {extra} is not supported"
                )
            cwd = server.get("cwd")
            if cwd is not None and not relative_plugin_cwd(cwd):
                errors.append(
                    f"plugin/mcp.json.mcpServers.{server_name}.cwd must be plugin-relative"
                )
            env = server.get("env")
            if env is not None:
                if not isinstance(env, dict) or not all(
                    isinstance(k, str) and isinstance(v, str) for k, v in env.items()
                ):
                    errors.append(
                        f"plugin/mcp.json.mcpServers.{server_name}.env must be a string map"
                    )
                else:
                    for reserved in ("PLUGIN_ROOT", "PLUGIN_DATA"):
                        if reserved in env:
                            errors.append(
                                f"plugin/mcp.json.mcpServers.{server_name}.env must not set {reserved}"
                            )

agent_launch = mcp_launch(mcp, "plugin/mcp.json")
claude_launch = mcp_launch(claude_mcp, "plugin/.mcp.json")
codex_launch = mcp_launch(
    codex_manifest if isinstance(codex_manifest, dict) else None,
    "plugin/.codex-plugin/plugin.json",
)
expected = (
    (EXPECTED_MCP_COMMAND, expected_mcp_args) if expected_mcp_args is not None else None
)
for label, launch in (
    ("plugin/mcp.json", agent_launch),
    ("plugin/.mcp.json", claude_launch),
    ("plugin/.codex-plugin/plugin.json", codex_launch),
):
    if launch is None or expected is None:
        continue
    args = launch[1]
    if launch == expected:
        continue
    stale = any("@latest" in arg or "@0.5.1" in arg for arg in args)
    prefix = f"{label} has a stale launch pin; " if stale else f"{label} "
    errors.append(
        f"{prefix}MCP launch must be {EXPECTED_MCP_COMMAND} {' '.join(expected_mcp_args)}"
    )

if claude_manifest is not None:
    require_owner(claude_manifest.get("author"), "plugin/.claude-plugin/plugin.json.author")
if isinstance(codex_manifest, dict):
    require_owner(codex_manifest.get("author"), "plugin/.codex-plugin/plugin.json.author")
    interface = codex_manifest.get("interface")
    if isinstance(interface, dict) and interface.get("developerName") != EXPECTED_OWNER_NAME:
        errors.append(
            "plugin/.codex-plugin/plugin.json.interface.developerName must be "
            f"{EXPECTED_OWNER_NAME!r}"
        )
if claude_marketplace is not None:
    require_owner(claude_marketplace.get("owner"), ".claude-plugin/marketplace.json.owner")

skills_root = plugin_root / "skills"
orbit_skill = skills_root / "orbit"
if not orbit_skill.is_dir():
    errors.append("plugin/skills/orbit is missing")
else:
    validate_skill_frontmatter(orbit_skill)
if skills_root.is_dir():
    for skill_dir in sorted(path for path in skills_root.iterdir() if path.is_dir()):
        validate_skill_frontmatter(skill_dir)

if errors:
    print("validate-agent-plugin: failed", file=sys.stderr)
    for error in errors:
        print(f"- {error}", file=sys.stderr)
    raise SystemExit(1)

print(f"validate-agent-plugin: plugin manifest is valid ({manifest_path.relative_to(repo)})")
PY
