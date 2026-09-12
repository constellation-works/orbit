#!/usr/bin/env bash
set -euo pipefail

# Verify (or, with --update, regenerate) checked-in orbit-cli help and
# description goldens without running the workspace test suite.
#
# Covers:
#   - CLI long-help text under crates/orbit-cli/src/command/tests/
#   - crates/orbit-cli/tests/output_goldens/ (including tool_list.json)
#   - crates/orbit-cli/tests/snapshots/mcp_tools_list.json
#
# Regeneration env vars (also printed by the failing tests):
#   ORBIT_UPDATE_HELP_GOLDENS=1
#   ORBIT_UPDATE_OUTPUT_GOLDENS=1
#   ORBIT_MCP_UPDATE_SNAPSHOT=1

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

update=false
case "${1:-}" in
  "") ;;
  --update) update=true ;;
  *)
    echo "usage: check-goldens.sh [--update]" >&2
    exit 2
    ;;
esac

if [[ "$update" == true ]]; then
  export ORBIT_UPDATE_HELP_GOLDENS=1
  export ORBIT_UPDATE_OUTPUT_GOLDENS=1
  export ORBIT_MCP_UPDATE_SNAPSHOT=1
fi

cargo="${CARGO:-cargo}"

"$cargo" test -p orbit-cli --bin orbit help_matches_the_shipped_surface
"$cargo" test -p orbit-cli --test output_goldens
"$cargo" test -p orbit-cli --test mcp_roundtrip \
  mcp_serve_tools_list_matches_production_snapshot -- --exact
