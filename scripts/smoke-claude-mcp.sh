#!/usr/bin/env bash
# Smoke test: confirm the headless `claude -p` invocation defined by
# crates/orbit-core/assets/executors/claude.yaml can reach the orbit MCP
# server. Mirrors that yaml's args verbatim except `--debug-file`, which the
# engine rewrites at spawn time.
#
# Pass criterion: at least one new audit row in ~/.orbit/orbit.db with
# subcommand='run-mcp' and tool_name beginning with 'orbit.' is recorded
# during the run.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if [[ ! -f .mcp.json ]]; then
  echo "FAIL: .mcp.json not found in $REPO_ROOT — run 'orbit workspace init --mcp' first" >&2
  exit 2
fi

for bin in claude orbit sqlite3 jq; do
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

before_id=$(sqlite3 "$DB" "SELECT COALESCE(MAX(id), 0) FROM audit_events;")
echo "baseline audit max id: $before_id (attributing only orbit.task.list rows after it)"

prompt='Use the orbit.task.list orbit tool with input {"limit": 3} to list orbit  (do not use mcp tools!) tasks. After the tool returns, reply with a one-line summary: "tasks_returned=<N>" where N is the count of items in the result and list of skills available to you.'

echo "--- invoking claude (this will hit the API and may take ~30-60s) ---"
out=$(claude -p \
  --permission-mode bypassPermissions \
  --output-format json \
  --tools default \
  --model claude-sonnet-4-6 \
  "$prompt")

echo "--- claude raw output ---"
echo "$out"

delta=$(sqlite3 "$DB" "SELECT count(*) FROM audit_events WHERE id > $before_id AND subcommand='run-mcp' AND tool_name='orbit.task.list';")
echo "--- audit delta: $delta new orbit.task.list rows after id $before_id ---"

if [[ "$delta" -gt 0 ]]; then
  echo "--- new rows ---"
  sqlite3 -header -column "$DB" \
    "SELECT timestamp, tool_name, status, exit_code FROM audit_events
     WHERE id > $before_id AND subcommand='run-mcp' AND tool_name='orbit.task.list'
     ORDER BY id DESC LIMIT $delta;"
  echo "PASS: claude reached orbit MCP server through the new args"
  exit 0
fi

echo "FAIL: no new run-mcp audit rows. claude did not reach orbit MCP." >&2
echo "  Likely causes: (1) --allowedTools wildcard not honored;" >&2
echo "  (2) claude version doesn't recognize --mcp-config or --tools; " >&2
echo "  (3) .mcp.json malformed or orbit binary not spawnable from cwd." >&2
exit 1
