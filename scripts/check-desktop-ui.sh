#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
# Execute the shipped MCP Apps script; missing Node or a failing test is a failed gate.
node --test "$repo_root/crates/orbit-mcp/src/adapter/tests/task-panel.mjs"
