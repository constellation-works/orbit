#!/usr/bin/env bash
# Regression checks for the manual MCP Registry publication security boundary.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
workflow="$repo_root/.github/workflows/publish-mcp-registry.yml"

require() {
  local description="$1"
  local text="$2"

  if ! grep --fixed-strings --quiet -- "$text" "$workflow"; then
    echo "missing $description" >&2
    exit 1
  fi
}

forbid() {
  local description="$1"
  local text="$2"

  if grep --fixed-strings --quiet -- "$text" "$workflow"; then
    echo "forbidden $description" >&2
    exit 1
  fi
}

require "manual trigger" "workflow_dispatch:"
forbid "automatic push publication" "  push:"
require "strict release tag validation" "tag must be an explicit vMAJOR.MINOR.PATCH release tag"
require "published-release check" "published, non-draft GitHub release"
require "commit-pinned checkout" 'ref: ${{ needs.verify-release.outputs.commit_sha }}'
forbid "unchecked input checkout" 'ref: ${{ inputs.tag }}'
require "exact active-membership endpoint" "/user/memberships/orgs?state=active&per_page=100&page=\$page"
require "owner-role check" '"$state" != "active" || "$role" != "owner"'
require "pinned publisher version" "releases/download/v1.8.1/mcp-publisher_linux_amd64.tar.gz"
require "publisher checksum" "a06c9096dcb9727c13555b6be26c7effa707b01f06a4c561ba7a3635443cf2cc"
require "official token login" './mcp-publisher login github --token "$MCP_REGISTRY_PAT_TOKEN"'
require "sanitized publisher authentication failure" "MCP Registry authentication failed; verify the dedicated token's owner grant"
require "public record verification" "registry.modelcontextprotocol.io/v0.1/servers/\$server_path/versions/\$VERSION"
require "public registry response handling" ".server.name == \$name"
forbid "secret echo" 'echo "$MCP_REGISTRY_PAT_TOKEN"'

echo "MCP Registry publication workflow checks passed"
