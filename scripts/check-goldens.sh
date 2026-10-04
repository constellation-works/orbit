#!/usr/bin/env bash
set -euo pipefail

# Verify (or, with --update, regenerate) checked-in orbit-cli help and
# description, CI log, and sandbox profile goldens without running the
# workspace test suite.
#
# Covers:
#   - sanitized CI signature and GitHub log fixture goldens
#   - builtin MCP definition and annotation conformance
#   - CLI long-help text under crates/orbit-cli/tests/help_goldens/
#   - crates/orbit-cli/tests/output_goldens/ (including tool_list.json)
#   - crates/orbit-cli/tests/snapshots/mcp_tools_list.json
#   - crates/orbit-exec/tests/sandbox_profile_goldens/ (compiled SBPL on every
#     platform, Bubblewrap/Landlock on Linux)
#   - crates/orbit-core/tests/sandbox_profile_goldens/ (resolved Linux sandbox)
#
# Regeneration env vars (also printed by the failing tests):
#   ORBIT_UPDATE_LOG_GOLDENS=1
#   ORBIT_UPDATE_HELP_GOLDENS=1
#   ORBIT_UPDATE_OUTPUT_GOLDENS=1
#   ORBIT_MCP_UPDATE_SNAPSHOT=1
#   ORBIT_UPDATE_SANDBOX_GOLDENS=1

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
  export ORBIT_UPDATE_LOG_GOLDENS=1
  export ORBIT_UPDATE_HELP_GOLDENS=1
  export ORBIT_UPDATE_OUTPUT_GOLDENS=1
  export ORBIT_MCP_UPDATE_SNAPSHOT=1
  export ORBIT_UPDATE_SANDBOX_GOLDENS=1
fi

# Gates always cover the full corpus, even after an individual fixture replay.
unset ORBIT_LOG_GOLDEN_CASE

cargo="${CARGO:-cargo}"

"$cargo" test -p orbit-core --test ci_failure_goldens
"$cargo" test -p orbit-tools --test tools -- \
  public_tool_surface::github_log_goldens:: mcp_definitions::
"$cargo" test -p orbit-cli --test output -- help_goldens:: output_goldens::
"$cargo" test -p orbit-cli --test mcp \
  mcp_roundtrip::mcp_serve_tools_list_matches_production_snapshot -- --exact
"$cargo" test -p orbit-exec --test sandbox_profile_goldens
"$cargo" test -p orbit-core --test sandbox_profile_goldens
