#!/usr/bin/env bash
set -euo pipefail

# Verify (or, with --update, regenerate) checked-in CLI/MCP, CI log, sandbox
# profile, and SQLite ledger schema goldens without running the full workspace
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
#   - store-ledger and task-registry-ledger sqlite_schema prefixes under
#     orbit-store migration/tests/ and task_registry/tests/
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

# A filtered cargo test exits 0 when its filter matches zero tests, so a
# renamed or deleted golden target would pass vacuously. Capture each run,
# relay it, and fail when the harness reports zero executed tests.
run_golden_test() {
  local output status count
  set +e
  output="$("$@" 2>&1)"
  status=$?
  set -e
  printf '%s\n' "$output"
  if [[ "$status" -ne 0 ]]; then
    return "$status"
  fi
  # Sum every "N passed" harness summary; a filter matching nothing reports
  # a summary with 0 passed. No summary means no harness evidence to judge.
  count="$(printf '%s\n' "$output" | awk '/test result:/ { for (i=1; i<=NF; i++) if ($i == "passed;" && i>1) total += $(i-1); summaries++ } END { print (summaries ? total+0 : -1) }')"
  if [[ "$count" -eq 0 ]]; then
    echo "check-goldens: cargo test matched zero tests: $*" >&2
    return 1
  fi
}

# Only the golden fixture: the binary's behavioural tests belong to the
# affected-test gate, and beside it they multiply the store writes that slow
# the fixture on a loaded host (ORB-15293).
# Every invocation selects `-p orbit-cli` as the feature anchor so they all
# resolve one workspace feature set (resolver v2 unifies over the selected
# packages). The explicit `--test` / `--lib` selector keeps each run to its
# golden target: Cargo only errors when no selected package has it.
run_golden_test "$cargo" test -p orbit-core -p orbit-cli --test ci_failure_goldens \
  ci_failure_fixture_goldens -- --exact
run_golden_test "$cargo" test -p orbit-tools -p orbit-cli --test tools -- \
  public_tool_surface::github_log_goldens:: mcp_definitions::
run_golden_test "$cargo" test -p orbit-cli --test output -- help_goldens:: output_goldens::
run_golden_test "$cargo" test -p orbit-cli --test mcp \
  mcp_roundtrip::mcp_serve_tools_list_matches_production_snapshot -- --exact
run_golden_test "$cargo" test -p orbit-exec -p orbit-core -p orbit-cli --test sandbox_profile_goldens

run_golden_test "$cargo" test -p orbit-store -p orbit-cli --lib \
  driver::sqlite::migration::tests::ledger::registry::store_schema_prefix_goldens -- --exact \
  driver::sqlite::migration::tests::ledger::registry::task_registry_schema_prefix_goldens
