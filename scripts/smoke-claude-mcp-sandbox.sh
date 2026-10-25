#!/usr/bin/env bash
# Sandboxed companion to smoke-claude-mcp.sh.
#
# Wraps `claude -p` in `sandbox-exec -f <profile>` using a profile that mirrors
# crates/orbit-exec/src/macos_sandbox.rs::compile_macos_sandbox_profile() for an
# implicit unrestricted fsProfile (read=./**, modify=./**) — the same shape an
# activity gets when it omits `fsProfile:`. This reproduces the kernel
# confinement orbit-engine applies to direct_agent steps so we can see what
# breaks when `orbit tool run ...` is invoked from inside claude under sandbox.
#
# Pass criterion: at least one new audit row in ~/.orbit/orbit.db with
# subcommand IN ('run','run-mcp') and tool_name LIKE 'orbit.%' is recorded
# during the run. (The non-sandbox script forced the CLI path with the prompt
# 'do not use mcp tools'; this one mirrors that — so we expect subcommand='run'
# rows from `orbit tool run orbit.task.list ...` invoked via Bash.)
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# --- preflight ---
if [[ ! -f .mcp.json ]]; then
  echo "FAIL: .mcp.json missing in $REPO_ROOT" >&2; exit 2
fi
for bin in claude orbit sqlite3 sandbox-exec; do
  command -v "$bin" >/dev/null 2>&1 || { echo "FAIL: $bin not on PATH" >&2; exit 2; }
done
DB="$HOME/.orbit/orbit.db"
[[ -f "$DB" ]] || { echo "FAIL: $DB missing" >&2; exit 2; }

# --- compile sandbox profile (mirrors macos_sandbox.rs) ---
PROFILE=$(mktemp -t orbit-smoke-XXXXXX.sb)
trap 'rm -f "$PROFILE"' EXIT

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

echo "--- compiled profile (${PROFILE}) ---"
cat "$PROFILE"
echo "----------------------------------------"

# --- baseline ---
before_id=$(sqlite3 "$DB" "SELECT COALESCE(MAX(id), 0) FROM audit_events;")
echo "baseline audit max id: $before_id (attributing only orbit.task.list rows after it)"

prompt='Use the orbit.task.list orbit tool with input {"limit": 3} to list orbit tasks (do not use mcp tools!). After the tool returns, reply with a one-line summary: "tasks_returned=<N>" where N is the count of items in the result.'

# --- run claude under sandbox-exec ---
echo "--- invoking claude under sandbox-exec ---"
set +e
out=$(sandbox-exec -f "$PROFILE" claude -p \
  --permission-mode bypassPermissions \
  --output-format json \
  --tools default \
  --model claude-sonnet-4-6 \
  "$prompt" 2>&1)
rc=$?
set -e

echo "--- claude exit=$rc ---"
echo "$out"

# --- verdict ---
delta=$(sqlite3 "$DB" "SELECT count(*) FROM audit_events WHERE id > $before_id AND subcommand IN ('run','run-mcp') AND tool_name='orbit.task.list';")
echo "--- audit delta: $delta new orbit.task.list rows after id $before_id ---"

if [[ "$delta" -gt 0 ]]; then
  sqlite3 -header -column "$DB" \
    "SELECT timestamp, subcommand, tool_name, status, exit_code FROM audit_events
     WHERE id > $before_id AND subcommand IN ('run','run-mcp') AND tool_name='orbit.task.list'
     ORDER BY id DESC LIMIT $delta;"
  echo "PASS: claude reached orbit (CLI or MCP) under sandbox"
  exit 0
fi

echo "FAIL: no new orbit.* audit rows under sandbox." >&2
echo "  Likely causes:" >&2
echo "  (1) PATH not propagated — claude's bash can't find 'orbit'" >&2
echo "  (2) skills not auto-loaded under sandbox so claude doesn't know the CLI form" >&2
echo "  (3) sandbox blocks something the orbit child needs (check claude-debug.log under \$HOME/.claude)" >&2
echo "  (4) Anthropic API auth failed (network ok, but ~/.claude credential read or env var)" >&2
exit 1
