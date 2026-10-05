#!/usr/bin/env bash
# Orbit SessionStart hook: tell the session when the user's cwd is not inside
# an initialized Orbit workspace. Pure filesystem walk — no `orbit` binary
# dependency and no state mutation. Mirrors the discovery rules in
# `crates/orbit-core/src/runtime/resolve.rs` (find_orbit_dir_walk_up +
# is_initialized_orbit_root); the `plugin_hook_workspace_probe` integration test
# runs this script against a real `orbit workspace init` so the two cannot drift.
set -eu

target_dir=""
if [ -n "${CLAUDE_PROJECT_DIR:-}" ]; then
  target_dir="$CLAUDE_PROJECT_DIR"
elif [ ! -t 0 ]; then
  payload=$(cat || true)
  if [ -n "$payload" ] && command -v python3 >/dev/null 2>&1; then
    parsed=$(printf '%s' "$payload" | python3 -c \
      'import json,sys
try:
    print(json.loads(sys.stdin.read()).get("cwd",""))
except Exception:
    pass' 2>/dev/null || true)
    if [ -n "$parsed" ]; then
      target_dir="$parsed"
    fi
  fi
fi
if [ -z "$target_dir" ]; then
  target_dir="$PWD"
fi

global_orbit="${HOME:-/}/.orbit"

is_initialized_orbit_dir() {
  candidate="$1"
  [ -d "$candidate" ] || return 1
  # Skip the user's global ~/.orbit — it is not a workspace.
  if [ "$candidate" = "$global_orbit" ]; then
    return 1
  fi
  # `orbit workspace init` writes `config.yaml`; `config.toml` and the
  # `resources` + `state` layout are the other markers the runtime accepts.
  if [ -f "$candidate/config.yaml" ] || [ -f "$candidate/config.toml" ]; then
    return 0
  fi
  if [ -d "$candidate/resources" ] && [ -d "$candidate/state" ]; then
    return 0
  fi
  return 1
}

current="$target_dir"
while :; do
  if is_initialized_orbit_dir "$current/.orbit"; then
    exit 0
  fi
  parent=$(dirname -- "$current")
  if [ "$parent" = "$current" ]; then
    break
  fi
  current="$parent"
done

# Uninitialized. `systemMessage` is shown to the user only; the session model
# reads `additionalContext`, so the guidance it needs goes there.
cat <<'JSON'
{
  "continue": true,
  "suppressOutput": false,
  "systemMessage": "Orbit has no workspace here. Run `orbit init` once per machine, then `orbit workspace init --name <name>` from the repository root.",
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "additionalContext": "The Orbit plugin is installed, but no Orbit workspace was found at or above this directory. The Orbit MCP tools are connected, but orbit_workspace_list will not list this project and workspace-scoped calls for it fail until it is registered. If the user wants Orbit here, tell them to run `orbit init` once per machine (it creates ~/.orbit), then `orbit workspace init --name <name>` from the repository root. No restart is needed afterwards: call orbit_workspace_list and pass the returned ws_ ID as `workspace`. Do not initialize a workspace unless the user asks."
  }
}
JSON

exit 0
