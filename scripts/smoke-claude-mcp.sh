#!/usr/bin/env bash
# Smoke test: confirm this headless claude invocation can reach the orbit MCP
# server:
#   claude -p --permission-mode bypassPermissions --output-format json
#     --tools default --model claude-sonnet-4-6
#
# Pass criterion: this invocation exits 0, and a new successful run-mcp audit
# row for orbit.task.list names an orbit process observed under it. A matching
# row from another client, an older row, or a failed call does not qualify.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if [[ ! -f .mcp.json ]]; then
  echo "FAIL: .mcp.json not found in $REPO_ROOT — run 'orbit workspace init --mcp' first" >&2
  exit 2
fi

for bin in claude orbit sqlite3 jq python3; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "FAIL: required binary '$bin' not on PATH" >&2
    exit 2
  fi
done

DB="$HOME/.orbit/orbit.db"
if [[ ! -f "$DB" ]]; then
  echo "FAIL: $DB not found" >&2
  exit 2
fi

mkdir -p "$REPO_ROOT/.orbit/tmp"
observed=$(mktemp "$REPO_ROOT/.orbit/tmp/orbit-smoke-observed-XXXXXX")
export ORBIT_SMOKE_OBSERVED="$observed"
export ORBIT_SMOKE_COMMAND_STATUS="$observed.status"
trap 'python3 -c "from pathlib import Path; import sys; [Path(name).unlink(missing_ok=True) for name in sys.argv[1:]]" "$observed" "$ORBIT_SMOKE_COMMAND_STATUS"' EXIT

before_id=$(sqlite3 "$DB" "SELECT COALESCE(MAX(id), 0) FROM audit_events;")
echo "baseline audit max id: $before_id (attributing only orbit.task.list rows after it, bound to this invocation)"

prompt='Use the Orbit MCP server to call orbit.task.list with input {"limit": 3}. Use its MCP tool, not Bash or the orbit CLI. After it returns, reply with a one-line summary: "tasks_returned=<N>" where N is the count of items in the result.'

helper="$(cd "$(dirname "$0")" && pwd)/smoke-claude-mcp-invoke.py"
echo "--- invoking claude (this will hit the API and may take ~30-60s) ---"
set +e
out=$(python3 "$helper" -- claude -p \
  --permission-mode bypassPermissions \
  --output-format json \
  --tools default \
  --model claude-sonnet-4-6 \
  "$prompt")
rc=$?
set -e

echo "--- claude raw output ---"
echo "$out"
echo "--- claude exit=$rc ---"

if [[ "$rc" -eq 2 ]]; then
  echo "FAIL: cannot attribute this claude invocation to an orbit process." >&2
  exit 2
fi
if [[ "$rc" -ne 0 ]]; then
  command_rc="$(cat "$ORBIT_SMOKE_COMMAND_STATUS" 2>/dev/null || echo "$rc")"
  echo "FAIL: claude exited $command_rc; a later audit row is not evidence for this invocation." >&2
  exit 1
fi

# An empty observation is a real answer. Do not emit SQL `IN ()`.
pids="$(grep -E '^[0-9]+$' "$observed" | sort -u | paste -sd, - || true)"
if [[ -z "$pids" ]]; then
  delta=0
else
  delta=$(sqlite3 "$DB" "SELECT count(*) FROM audit_events WHERE id > $before_id AND subcommand='run-mcp' AND tool_name='orbit.task.list' AND status='success' AND exit_code=0 AND pid IN ($pids);")
fi
echo "--- audit delta: $delta new invocation-bound orbit.task.list rows after id $before_id ---"

if [[ "$delta" -gt 0 ]]; then
  echo "--- new rows ---"
  sqlite3 -header -column "$DB" \
    "SELECT timestamp, tool_name, status, exit_code, pid FROM audit_events
     WHERE id > $before_id AND subcommand='run-mcp' AND tool_name='orbit.task.list' AND status='success' AND exit_code=0 AND pid IN ($pids)
     ORDER BY id DESC LIMIT $delta;"
  echo "PASS: claude reached orbit MCP server through the new args"
  exit 0
fi

echo "FAIL: no new successful run-mcp orbit.task.list audit rows bound to this invocation. claude did not reach orbit MCP." >&2
echo "  Likely causes: (1) claude rejected --permission-mode, --output-format, --tools, or --model;" >&2
echo "  (2) .mcp.json is malformed or the orbit binary is not spawnable from cwd." >&2
exit 1
