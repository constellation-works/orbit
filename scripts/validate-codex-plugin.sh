#!/usr/bin/env bash
# Validate the repository-owned Codex plugin package.
#
# This intentionally lives in-repo instead of depending on a developer's
# ~/.codex skill checkout. It mirrors the supported Codex manifest contract
# used by the plugin-creator validator for the fields Orbit ships.
set -euo pipefail

repo_root="${1:-${ORBIT_REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}}"

if ! command -v python3 >/dev/null 2>&1; then
  echo "validate-codex-plugin: required binary 'python3' not on PATH" >&2
  exit 2
fi

python3 - "$repo_root" <<'PY'
from __future__ import annotations

import json
import re
import sys
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import urlparse

repo = Path(sys.argv[1]).resolve()
plugin_root = repo / "plugin"
manifest_path = plugin_root / ".codex-plugin" / "plugin.json"
marketplace_path = repo / ".agents" / "plugins" / "marketplace.json"
EXPECTED_OWNER_NAME = "constellation-works"
EXPECTED_OWNER_URL = "https://github.com/constellation-works"
EXPECTED_MCP_COMMAND = "npx"

SEMVER_RE = re.compile(
    r"^(0|[1-9]\d*)\."
    r"(0|[1-9]\d*)\."
    r"(0|[1-9]\d*)"
    r"(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\."
    r"(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)
HEX_COLOR_RE = re.compile(r"^#[0-9A-F]{6}$", re.IGNORECASE)

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


def require_string(payload: dict[str, Any], key: str, label: str) -> str | None:
    value = payload.get(key)
    if not isinstance(value, str) or not value.strip():
        errors.append(f"{label}.{key} must be a non-empty string")
        return None
    return value


def require_owner(payload: dict[str, Any] | None, label: str) -> None:
    if payload is None:
        errors.append(f"{label} must be an object")
        return
    if not isinstance(payload, dict):
        errors.append(f"{label} must be an object")
        return
    if payload.get("name") != EXPECTED_OWNER_NAME:
        errors.append(f"{label}.name must be {EXPECTED_OWNER_NAME!r}")
    if payload.get("url") != EXPECTED_OWNER_URL:
        errors.append(f"{label}.url must be {EXPECTED_OWNER_URL!r}")


def optional_https(payload: dict[str, Any], key: str, label: str) -> None:
    value = payload.get(key)
    if value is None:
        return
    parsed = urlparse(value) if isinstance(value, str) else None
    if parsed is None or parsed.scheme != "https" or not parsed.netloc:
        errors.append(f"{label}.{key} must be an absolute https:// URL")


def normalize_contract_path(raw_path: Any) -> str | None:
    if not isinstance(raw_path, str):
        return None
    path = PurePosixPath(raw_path.replace("\\", "/"))
    if path.is_absolute() or any(part in {"", ".."} for part in path.parts):
        return None
    normalized = path.as_posix().rstrip("/")
    if normalized.startswith("./"):
        normalized = normalized[2:]
    return normalized or None


def reject_user_specific_paths(value: Any, path: str) -> None:
    if isinstance(value, str):
        if "CLAUDE_PROJECT_DIR" in value:
            errors.append(f"{path} must not reference CLAUDE_PROJECT_DIR")
        if str(Path.home()) in value:
            errors.append(f"{path} must not embed a user-specific absolute home path")
        return
    if isinstance(value, list):
        for index, item in enumerate(value):
            reject_user_specific_paths(item, f"{path}[{index}]")
        return
    if isinstance(value, dict):
        for key, item in value.items():
            reject_user_specific_paths(item, f"{path}.{key}")


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
    frontmatter = text[4:end]
    fields: dict[str, str] = {}
    for line in frontmatter.splitlines():
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        fields[key.strip()] = value.strip().strip('"').strip("'")
    for key in ("name", "description"):
        if not fields.get(key):
            errors.append(f"skill {skill_dir.name} frontmatter field {key} must be non-empty")


def validate_task_pilot_assets() -> None:
    # The task-pilot contract lives in two places, and neither is a skill: the
    # `task_pilot` activity instructs the agent the pipeline actually runs, and
    # this agent profile instructs the one a plugin user delegates to. A third
    # copy as a skill was redundant and free to drift, so it was retired — these
    # markers keep the read-only bounds asserted on the copy that remains.
    agent_path = plugin_root / "agents" / "orbit-task-pilot.md"
    if not agent_path.is_file():
        errors.append("plugin is missing agents/orbit-task-pilot.md")
    else:
        agent = agent_path.read_text(encoding="utf-8")
        for required in (
            "name: orbit-task-pilot",
            "tools: Read, Grep, Glob, Bash",
            "Never edit repository files",
            "context_files_before",
            "context_files_after",
            "verified_no_diff",
        ):
            if required not in agent:
                errors.append(f"orbit-task-pilot agent is missing profile marker {required!r}")
        for forbidden in ("tools: Read, Grep, Glob, Bash, Edit", "Write", "orbit.task.update"):
            if forbidden in agent:
                errors.append(f"orbit-task-pilot agent contains forbidden write marker {forbidden!r}")


manifest = load_json(manifest_path, "plugin/.codex-plugin/plugin.json")
if manifest is not None:
    reject_todos(manifest, "plugin.json")
    allowed_keys = {
        "id",
        "name",
        "version",
        "description",
        "skills",
        "apps",
        "mcpServers",
        "interface",
        "author",
        "homepage",
        "repository",
        "license",
        "keywords",
    }
    for key in sorted(set(manifest) - allowed_keys):
        errors.append(f"plugin.json field {key} is not supported")

    name = require_string(manifest, "name", "plugin.json")
    version = require_string(manifest, "version", "plugin.json")
    require_string(manifest, "description", "plugin.json")
    if version is not None and SEMVER_RE.fullmatch(version) is None:
        errors.append("plugin.json.version must use strict semver")

    author = manifest.get("author")
    if not isinstance(author, dict):
        errors.append("plugin.json.author must be an object")
    else:
        require_string(author, "name", "plugin.json.author")
        optional_https(author, "url", "plugin.json.author")
        require_owner(author, "plugin.json.author")

    if normalize_contract_path(manifest.get("skills")) != "skills":
        errors.append("plugin.json.skills must resolve to ./skills/")

    mcp_servers = manifest.get("mcpServers")
    if not isinstance(mcp_servers, dict) or not mcp_servers:
        errors.append("plugin.json.mcpServers must be a non-empty object for Orbit")
    else:
        reject_user_specific_paths(mcp_servers, "plugin.json.mcpServers")
        for server_name, server in mcp_servers.items():
            if not isinstance(server_name, str) or not server_name.strip():
                errors.append("plugin.json.mcpServers keys must be non-empty strings")
            if not isinstance(server, dict):
                errors.append(f"plugin.json.mcpServers.{server_name} must be an object")
                continue
            command = server.get("command")
            if not isinstance(command, str) or not command.strip():
                errors.append(f"plugin.json.mcpServers.{server_name}.command must be non-empty")
            args = server.get("args")
            if args is not None and (
                not isinstance(args, list)
                or not all(isinstance(arg, str) and arg.strip() for arg in args)
            ):
                errors.append(f"plugin.json.mcpServers.{server_name}.args must be an array of strings")
            elif (
                version is not None
                and server_name == "orbit"
                and isinstance(command, str)
                and isinstance(args, list)
            ):
                expected_args = ["-y", f"@orbit-tools/cli@{version}", "mcp", "serve"]
                stale = any(
                    isinstance(arg, str) and ("@latest" in arg or "@0.5.1" in arg)
                    for arg in args
                )
                if command != EXPECTED_MCP_COMMAND or args != expected_args:
                    prefix = "plugin.json has a stale launch pin; " if stale else ""
                    errors.append(
                        f"{prefix}plugin.json.mcpServers.orbit must launch "
                        f"{EXPECTED_MCP_COMMAND} {' '.join(expected_args)}"
                    )

    interface = manifest.get("interface")
    if not isinstance(interface, dict):
        errors.append("plugin.json.interface must be an object")
    else:
        for field in (
            "displayName",
            "shortDescription",
            "longDescription",
            "developerName",
            "category",
        ):
            require_string(interface, field, "plugin.json.interface")
        if interface.get("developerName") != EXPECTED_OWNER_NAME:
            errors.append(
                "plugin.json.interface.developerName must be "
                f"{EXPECTED_OWNER_NAME!r}"
            )
        capabilities = interface.get("capabilities")
        if not isinstance(capabilities, list) or not all(
            isinstance(item, str) and item.strip() for item in capabilities
        ):
            errors.append("plugin.json.interface.capabilities must be an array of strings")
        prompts = interface.get("defaultPrompt")
        if not isinstance(prompts, list) or not 1 <= len(prompts) <= 3 or not all(
            isinstance(item, str) and item.strip() and len(item) <= 128 for item in prompts
        ):
            errors.append("plugin.json.interface.defaultPrompt must contain 1-3 short strings")
        color = interface.get("brandColor")
        if color is not None and (not isinstance(color, str) or HEX_COLOR_RE.fullmatch(color) is None):
            errors.append("plugin.json.interface.brandColor must use #RRGGBB")
        for field in ("websiteURL", "privacyPolicyURL", "termsOfServiceURL"):
            optional_https(interface, field, "plugin.json.interface")

    skills_root = plugin_root / "skills"
    if not skills_root.is_dir():
        errors.append("plugin/skills is missing")
    else:
        for skill_dir in sorted(path for path in skills_root.iterdir() if path.is_dir()):
            validate_skill_frontmatter(skill_dir)
    validate_task_pilot_assets()

    marketplace = load_json(marketplace_path, ".agents/plugins/marketplace.json")
    if marketplace is not None and name is not None:
        if not isinstance(marketplace.get("name"), str) or not marketplace["name"].strip():
            errors.append("marketplace.name must be a non-empty string")
        plugins = marketplace.get("plugins")
        if not isinstance(plugins, list):
            errors.append("marketplace.plugins must be an array")
        else:
            entries = [entry for entry in plugins if isinstance(entry, dict) and entry.get("name") == name]
            if len(entries) != 1:
                errors.append(f"marketplace must contain exactly one plugin entry named {name!r}")
            else:
                entry = entries[0]
                source = entry.get("source")
                if not isinstance(source, dict) or source.get("source") != "local":
                    errors.append("marketplace orbit entry must use a local source")
                else:
                    raw_path = source.get("path")
                    normalized = normalize_contract_path(raw_path)
                    if normalized is None:
                        errors.append("marketplace orbit source.path must be a relative path")
                    elif (repo / normalized).resolve() != plugin_root:
                        errors.append("marketplace orbit source.path must resolve to ./plugin")
                policy = entry.get("policy")
                if not isinstance(policy, dict):
                    errors.append("marketplace orbit policy must be an object")
                else:
                    if policy.get("installation") not in {"AVAILABLE", "NOT_AVAILABLE", "INSTALLED_BY_DEFAULT"}:
                        errors.append("marketplace orbit policy.installation is invalid")
                    if policy.get("authentication") not in {"ON_INSTALL", "ON_USE"}:
                        errors.append("marketplace orbit policy.authentication is invalid")
                if not isinstance(entry.get("category"), str) or not entry["category"].strip():
                    errors.append("marketplace orbit category must be a non-empty string")

if errors:
    print("validate-codex-plugin: failed", file=sys.stderr)
    for error in errors:
        print(f"- {error}", file=sys.stderr)
    raise SystemExit(1)

print(f"validate-codex-plugin: plugin manifest is valid ({manifest_path.relative_to(repo)})")
PY

