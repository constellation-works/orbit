#!/usr/bin/env bash
# Smoke test: confirm the headless `claude -p` invocation defined by
# crates/orbit-core/assets/executors/claude.yaml can reach the orbit MCP
# server. Mirrors that yaml's args verbatim except `--debug-file`, which the
# engine rewrites at spawn time.
#
# Pass criterion: a new successful run-mcp audit row for orbit.task.list
# after this invocation's baseline. CLI or unrelated tool rows do not qualify.
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

prompt='Use the Orbit MCP server to call orbit.task.list with input {"limit": 3}. Use its MCP tool, not Bash or the orbit CLI. After it returns, reply with a one-line summary: "tasks_returned=<N>" where N is the count of items in the result.'

echo "--- invoking claude (this will hit the API and may take ~30-60s) ---"
out=$(claude -p \
  --permission-mode bypassPermissions \
  --output-format json \
  --tools default \
  --model claude-sonnet-4-6 \
  "$prompt")

echo "--- claude raw output ---"
echo "$out"

delta=$(sqlite3 "$DB" "SELECT count(*) FROM audit_events WHERE id > $before_id AND subcommand='run-mcp' AND tool_name='orbit.task.list' AND status='success' AND exit_code=0;")
echo "--- audit delta: $delta new orbit.task.list rows after id $before_id ---"

if [[ "$delta" -gt 0 ]]; then
  echo "--- new rows ---"
  sqlite3 -header -column "$DB" \
    "SELECT timestamp, tool_name, status, exit_code FROM audit_events
     WHERE id > $before_id AND subcommand='run-mcp' AND tool_name='orbit.task.list' AND status='success' AND exit_code=0
     ORDER BY id DESC LIMIT $delta;"
  echo "PASS: claude reached orbit MCP server through the new args"
  exit 0
fi

echo "FAIL: no new successful run-mcp orbit.task.list audit rows. claude did not reach orbit MCP." >&2
echo "  Likely causes: (1) --allowedTools wildcard not honored;" >&2
echo "  (2) claude version doesn't recognize --mcp-config or --tools; " >&2
echo "  (3) .mcp.json malformed or orbit binary not spawnable from cwd." >&2
exit 1
