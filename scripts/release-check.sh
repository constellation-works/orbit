#!/usr/bin/env bash
# Verify release-version lockstep across Cargo, the npm proxy, npm, and GitHub.
#
# Exits 0 when every reachable source agrees, 1 on drift, and 2 on a missing
# local prerequisite. Remote registry/release checks remain soft so the target
# is useful from a fresh checkout without credentials.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

NPM_PKG="@orbit-tools/cli"
CARGO_TOML="Cargo.toml"
NPM_PACKAGE_JSON="npm/package.json"
MCP_SERVER_JSON="server.json"
CLAUDE_PLUGIN_MANIFEST="plugin/.claude-plugin/plugin.json"
CODEX_PLUGIN_MANIFEST="plugin/.codex-plugin/plugin.json"
AGENT_PLUGIN_MANIFEST="plugin/plugin.json"
AGENT_PLUGIN_MCP="plugin/mcp.json"
CLAUDE_PLUGIN_MCP="plugin/.mcp.json"
CLAUDE_MARKETPLACE=".claude-plugin/marketplace.json"
EXPECTED_OWNER="constellation-works"
EXPECTED_OWNER_URL="https://github.com/constellation-works"

require_bin() {
  local bin="$1"
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "release-check: required binary '$bin' not on PATH" >&2
    exit 2
  fi
}

require_bin jq

for file in "$CARGO_TOML" "$NPM_PACKAGE_JSON" "$MCP_SERVER_JSON" \
  "$CLAUDE_PLUGIN_MANIFEST" "$CODEX_PLUGIN_MANIFEST" "$AGENT_PLUGIN_MANIFEST" \
  "$AGENT_PLUGIN_MCP" "$CLAUDE_PLUGIN_MCP" "$CLAUDE_MARKETPLACE"; do
  if [[ ! -f "$file" ]]; then
    echo "release-check: $file not found (run from repo root)" >&2
    exit 2
  fi
done

"$repo_root/scripts/sync-plugin-skills.sh" --check >/dev/null
"$repo_root/scripts/validate-codex-plugin.sh" "$repo_root" >/dev/null
"$repo_root/scripts/validate-agent-plugin.sh" "$repo_root" >/dev/null

cargo_package_version="$(
  awk '
    /^\[workspace\.package\]$/ { in_workspace_package = 1; next }
    /^\[/ { in_workspace_package = 0 }
    in_workspace_package && /^version[[:space:]]*=/ {
      value = $0
      sub(/^[^=]*=[[:space:]]*/, "", value)
      gsub(/"/, "", value)
      print value
      exit
    }
  ' "$CARGO_TOML"
)"
npm_package_version="$(jq -r .version "$NPM_PACKAGE_JSON")"
mcp_name="$(jq -r .mcpName "$NPM_PACKAGE_JSON")"
server_name="$(jq -r .name "$MCP_SERVER_JSON")"
server_version="$(jq -r .version "$MCP_SERVER_JSON")"
server_package="$(jq -r '.packages[0].identifier // empty' "$MCP_SERVER_JSON")"
server_package_version="$(jq -r '.packages[0].version // empty' "$MCP_SERVER_JSON")"
claude_plugin_version="$(jq -r .version "$CLAUDE_PLUGIN_MANIFEST")"
codex_plugin_version="$(jq -r .version "$CODEX_PLUGIN_MANIFEST")"
agent_plugin_version="$(jq -r .version "$AGENT_PLUGIN_MANIFEST")"
expected_mcp_package="@orbit-tools/cli@${cargo_package_version}"

if [[ -z "$cargo_package_version" ]]; then
  echo "release-check: $CARGO_TOML has no [workspace.package] version" >&2
  exit 2
fi
if [[ -z "$npm_package_version" || "$npm_package_version" == "null" ]]; then
  echo "release-check: $NPM_PACKAGE_JSON has no .version field" >&2
  exit 2
fi
if [[ "$mcp_name" != "io.github.constellation-works/orbit" || "$server_name" != "$mcp_name" ]]; then
  echo "DRIFT: npm mcpName and server.json name must be io.github.constellation-works/orbit" >&2
  exit 1
fi
if [[ "$server_package" != "$NPM_PKG" || "$server_version" != "$npm_package_version" || "$server_package_version" != "$server_version" ]]; then
  echo "DRIFT: server.json package identity/version must match npm/package.json" >&2
  exit 1
fi
if ! jq -e '
  .packages | length == 1
  and .[0].registryType == "npm"
  and .[0].transport.type == "stdio"
  and ([.[0].packageArguments[] | [.type, .value]] == [["positional", "mcp"], ["positional", "serve"]])
  and ([.. | strings | select(. == "--operator")] | length == 0)
' "$MCP_SERVER_JSON" >/dev/null; then
  echo "DRIFT: server.json must retain exactly the non-operator npm stdio launch: mcp serve" >&2
  exit 1
fi
require_owner() {
  local file="$1"
  local jq_name="$2"
  local jq_url="$3"
  local name url
  name="$(jq -r "$jq_name" "$file")"
  url="$(jq -r "$jq_url" "$file")"
  if [[ "$name" != "$EXPECTED_OWNER" || "$url" != "$EXPECTED_OWNER_URL" ]]; then
    echo "DRIFT: $file owner must be $EXPECTED_OWNER ($EXPECTED_OWNER_URL), found $name ($url)" >&2
    exit 1
  fi
}

require_mcp_launch() {
  local file="$1"
  local jq_command="$2"
  local jq_args="$3"
  if ! jq -e --arg pkg "$expected_mcp_package" \
    "$jq_command == \"npx\" and $jq_args == [\"-y\", \$pkg, \"mcp\", \"serve\"]" \
    "$file" >/dev/null; then
    echo "DRIFT: $file MCP launch must pin npx -y $expected_mcp_package mcp serve" >&2
    exit 1
  fi
}

require_owner "$AGENT_PLUGIN_MANIFEST" ".author.name" ".author.url"
require_owner "$CLAUDE_PLUGIN_MANIFEST" ".author.name" ".author.url"
require_owner "$CODEX_PLUGIN_MANIFEST" ".author.name" ".author.url"
require_owner "$CLAUDE_MARKETPLACE" ".owner.name" ".owner.url"
codex_developer="$(jq -r .interface.developerName "$CODEX_PLUGIN_MANIFEST")"
if [[ "$codex_developer" != "$EXPECTED_OWNER" ]]; then
  echo "DRIFT: $CODEX_PLUGIN_MANIFEST interface.developerName must be $EXPECTED_OWNER" >&2
  exit 1
fi
require_mcp_launch "$AGENT_PLUGIN_MCP" ".mcpServers.orbit.command" ".mcpServers.orbit.args"
require_mcp_launch "$CLAUDE_PLUGIN_MCP" ".mcpServers.orbit.command" ".mcpServers.orbit.args"
require_mcp_launch "$CODEX_PLUGIN_MANIFEST" ".mcpServers.orbit.command" ".mcpServers.orbit.args"

if command -v mcp-publisher >/dev/null 2>&1; then
  mcp-publisher validate "$MCP_SERVER_JSON"
else
  echo "release-check: mcp-publisher not on PATH; structural registry validation completed, publisher validation deferred to release CI" >&2
fi

npm_registry_version=""
if command -v npm >/dev/null 2>&1; then
  if version="$(npm view "$NPM_PKG" version 2>/dev/null)"; then
    npm_registry_version="$version"
  else
    echo "release-check: npm view $NPM_PKG version failed (registry unreachable?)" >&2
  fi
else
  echo "release-check: npm not on PATH; skipping registry check" >&2
fi

gh_tag_version=""
if command -v gh >/dev/null 2>&1; then
  if tag="$(gh release list -L 1 --json tagName -q '.[0].tagName' 2>/dev/null)"; then
    gh_tag_version="${tag#v}"
  else
    echo "release-check: gh release list failed (not authenticated or no releases?)" >&2
  fi
else
  echo "release-check: gh not on PATH; skipping GitHub Release check" >&2
fi

printf '%-32s %s\n' "$CARGO_TOML [workspace.package]" "$cargo_package_version"
printf '%-32s %s\n' "$NPM_PACKAGE_JSON" "$npm_package_version"
printf '%-32s %s\n' "$MCP_SERVER_JSON" "$server_version"
printf '%-32s %s\n' "$CLAUDE_PLUGIN_MANIFEST" "$claude_plugin_version"
printf '%-32s %s\n' "$CODEX_PLUGIN_MANIFEST" "$codex_plugin_version"
printf '%-32s %s\n' "$AGENT_PLUGIN_MANIFEST" "$agent_plugin_version"
printf '%-32s %s\n' "npm view $NPM_PKG" "${npm_registry_version:-<skipped>}"
printf '%-32s %s\n' "gh release list -L 1" "${gh_tag_version:-<skipped>}"

drift=0
compare_version() {
  local source="$1"
  local version="$2"
  if [[ -n "$version" && "$cargo_package_version" != "$version" ]]; then
    echo "DRIFT: $CARGO_TOML ($cargo_package_version) != $source ($version)" >&2
    drift=1
  fi
}

compare_version "$NPM_PACKAGE_JSON" "$npm_package_version"
compare_version "$CLAUDE_PLUGIN_MANIFEST" "$claude_plugin_version"
compare_version "$CODEX_PLUGIN_MANIFEST" "$codex_plugin_version"
compare_version "$AGENT_PLUGIN_MANIFEST" "$agent_plugin_version"
compare_version "npm view $NPM_PKG" "$npm_registry_version"
compare_version "latest gh release tag" "$gh_tag_version"

if [[ "$drift" -ne 0 ]]; then
  cat >&2 <<EOF

release-check failed. See docs/runbooks/release.md for the procedure.
Cargo, npm package, npm registry, and GitHub Release versions must agree.
EOF
  exit 1
fi

if [[ -z "$npm_registry_version" || -z "$gh_tag_version" ]]; then
  echo "release-check: local sources agree on $cargo_package_version; remote checks were skipped." >&2
  exit 0
fi

echo "release-check: all sources agree on $cargo_package_version"
