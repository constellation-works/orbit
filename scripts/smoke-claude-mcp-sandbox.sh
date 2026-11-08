#!/usr/bin/env bash
# Sandboxed companion to smoke-claude-mcp.sh.
#
# Wraps `claude -p` in `sandbox-exec -f <profile>` using this script's own
# hand-written profile. It checks that Claude can reach Orbit MCP while running
# under that profile. The profile is not compiled from Orbit's production
# policy, and its broad HOME/.orbit write allowance and missing credential-read
# denials mean success does not establish production confinement equivalence or
# prove kernel behavior beyond this `sandbox-exec` invocation.
#
# Pass criterion: this invocation exits 0, and a new successful run-mcp audit
# row for orbit.task.list names an orbit process observed under it. A matching
# row from another client, an older row, a failed call, or a non-zero claude
# exit does not qualify. Success still does not establish production confinement.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# --- preflight ---
if [[ ! -f .mcp.json ]]; then
  echo "FAIL: .mcp.json missing in $REPO_ROOT" >&2; exit 2
fi
for bin in claude orbit sqlite3 sandbox-exec python3; do
  command -v "$bin" >/dev/null 2>&1 || { echo "FAIL: $bin not on PATH" >&2; exit 2; }
done
DB="$HOME/.orbit/orbit.db"
[[ -f "$DB" ]] || { echo "FAIL: $DB missing" >&2; exit 2; }

# --- write this diagnostic sandbox profile ---
mkdir -p "$REPO_ROOT/.orbit/tmp"
PROFILE=$(mktemp "$REPO_ROOT/.orbit/tmp/orbit-smoke-XXXXXX")
observed=$(mktemp "$REPO_ROOT/.orbit/tmp/orbit-smoke-observed-XXXXXX")
export ORBIT_SMOKE_OBSERVED="$observed"
trap 'python3 -c "from pathlib import Path; import sys; [Path(name).unlink(missing_ok=True) for name in sys.argv[1:]]" "$PROFILE" "$observed"' EXIT

esc() { printf '%s' "$1" | sed 's/"/\\"/g'; }
HOME_ESC="$(esc "$HOME")"
REPO_ESC="$(esc "$REPO_ROOT")"

cat >"$PROFILE" <<EOF
(version 1)
(deny default)
(allow file-read*)
(allow process*)
(allow signal)
(allow ipc-posix*)
(allow mach*)
(allow system-fsctl)
(allow system-socket)
(allow system-mac-syscall (mac-policy-name "vnguard"))
(allow system-mac-syscall (require-all (mac-policy-name "Sandbox") (mac-syscall-number 67)))
(allow network*)
(allow sysctl*)
(allow iokit*)
(allow file-write* (subpath "/tmp"))
(allow file-write* (subpath "/private/tmp"))
(allow file-write* (subpath "/private/var/folders"))
(allow file-write* (subpath "/dev"))
(allow file-write* (subpath "${HOME_ESC}/Library/Caches"))
(allow file-write* (subpath "${HOME_ESC}/.orbit"))
(allow file-write* (subpath "${HOME_ESC}/.codex"))
(allow file-write* (subpath "${HOME_ESC}/.claude"))
(allow file-write* (subpath "${HOME_ESC}/.gemini"))
(allow file-write* (subpath "${REPO_ESC}"))
EOF

echo "--- diagnostic profile (${PROFILE}) ---"
echo "Diagnostic only: this hand-written profile does not establish equivalence to Orbit's production confinement."
cat "$PROFILE"
echo "----------------------------------------"

# --- baseline ---
before_id=$(sqlite3 "$DB" "SELECT COALESCE(MAX(id), 0) FROM audit_events;")
echo "baseline audit max id: $before_id (attributing only orbit.task.list rows after it, bound to this invocation)"

prompt='Use the Orbit MCP server to call orbit.task.list with input {"limit": 3}. Use its MCP tool, not Bash or the orbit CLI. After it returns, reply with a one-line summary: "tasks_returned=<N>" where N is the count of items in the result.'

# --- run claude under sandbox-exec ---
helper="$(cd "$(dirname "$0")" && pwd)/smoke-claude-mcp-invoke.py"
echo "--- invoking claude under sandbox-exec ---"
set +e
out=$(python3 "$helper" --merge-stderr -- sandbox-exec -f "$PROFILE" claude -p \
  --permission-mode bypassPermissions \
  --output-format json \
  --tools default \
  --model claude-sonnet-4-6 \
  "$prompt")
rc=$?
set -e

echo "--- claude exit=$rc ---"
echo "$out"

if [[ "$rc" -eq 2 ]]; then
  echo "FAIL: cannot attribute this sandboxed claude invocation to an orbit process." >&2
  exit 2
fi
if [[ "$rc" -ne 0 ]]; then
  echo "FAIL: claude exited $rc; a later audit row is not evidence for this invocation." >&2
  exit 1
fi

# --- verdict ---
# An empty observation is a real answer. Do not emit SQL `IN ()`.
pids="$(grep -E '^[0-9]+$' "$observed" | sort -u | paste -sd, - || true)"
if [[ -z "$pids" ]]; then
  delta=0
else
  delta=$(sqlite3 "$DB" "SELECT count(*) FROM audit_events WHERE id > $before_id AND subcommand='run-mcp' AND tool_name='orbit.task.list' AND status='success' AND exit_code=0 AND pid IN ($pids);")
fi
echo "--- audit delta: $delta new invocation-bound orbit.task.list rows after id $before_id ---"

if [[ "$delta" -gt 0 ]]; then
  sqlite3 -header -column "$DB" \
    "SELECT timestamp, subcommand, tool_name, status, exit_code, pid FROM audit_events
     WHERE id > $before_id AND subcommand='run-mcp' AND tool_name='orbit.task.list' AND status='success' AND exit_code=0 AND pid IN ($pids)
     ORDER BY id DESC LIMIT $delta;"
  echo "PASS: claude reached orbit MCP under sandbox"
  exit 0
fi

echo "FAIL: no new successful run-mcp orbit.task.list audit rows under sandbox." >&2
echo "  Likely causes:" >&2
echo "  (1) PATH not propagated — claude's bash can't find 'orbit'" >&2
echo "  (2) Orbit MCP tools not loaded from .mcp.json" >&2
echo "  (3) sandbox blocks something the orbit child needs (check claude-debug.log under \$HOME/.claude)" >&2
echo "  (4) Anthropic API auth failed (network ok, but ~/.claude credential read or env var)" >&2
exit 1
