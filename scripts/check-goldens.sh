#!/usr/bin/env bash
set -euo pipefail

# Verify (or, with --update, regenerate) checked-in CLI/MCP, CI log, sandbox
# profile, and store-ledger schema goldens without running the full workspace
# test suite.
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
#   - store-ledger sqlite_schema prefixes under orbit-store migration/tests/
#
# Regeneration env vars (also printed by the failing tests):
#   ORBIT_UPDATE_STORE_SCHEMA_GOLDENS=1
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
  export ORBIT_UPDATE_STORE_SCHEMA_GOLDENS=1
  export ORBIT_UPDATE_LOG_GOLDENS=1
  export ORBIT_UPDATE_HELP_GOLDENS=1
  export ORBIT_UPDATE_OUTPUT_GOLDENS=1
  export ORBIT_MCP_UPDATE_SNAPSHOT=1
  export ORBIT_UPDATE_SANDBOX_GOLDENS=1
fi

# Gates always cover the full corpus, even after an individual fixture replay.
unset ORBIT_LOG_GOLDEN_CASE

cargo="${CARGO:-cargo}"

# Only the golden fixture: the binary's behavioural tests belong to the
# affected-test gate, and beside it they multiply the store writes that slow
# the fixture on a loaded host (ORB-15293).
"$cargo" test -p orbit-core --test ci_failure_goldens \
  ci_failure_fixture_goldens -- --exact
"$cargo" test -p orbit-tools --test tools -- \
  public_tool_surface::github_log_goldens:: mcp_definitions::
"$cargo" test -p orbit-cli --test output -- help_goldens:: output_goldens::
"$cargo" test -p orbit-cli --test mcp \
  mcp_roundtrip::mcp_serve_tools_list_matches_production_snapshot -- --exact
"$cargo" test -p orbit-exec --test sandbox_profile_goldens
"$cargo" test -p orbit-core --test sandbox_profile_goldens

"$cargo" test -p orbit-store --lib \
  driver::sqlite::migration::tests::ledger::store_schema_prefix_goldens -- --exact
