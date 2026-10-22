#!/usr/bin/env bash
# End-to-end smoke for the published npm install chain.
#
# Resolves the checked-in Official MCP Registry metadata, then runs its
# published npm package (not the local working tree). This catches drift
# between the registry contract, npm proxy, and GitHub Release binary.
#
# Steps:
#   1. resolve server.json and launch its versioned npm package
#        -> exercises postinstall: tarball download + sha256 verification
#   2. drive `orbit mcp serve` over stdio with a JSON-RPC handshake
#      (initialize + tools/list) and assert at least one `orbit.*` tool
#      appears in the response.
#
# Pass: exit 0. Fail: non-zero with the relevant stderr captured.
# Supported: macOS arm64 / x86_64, Linux x86_64 / arm64. Not Windows.
# --local-package-check instead builds/inspects the unpublished local candidate.
# --local-binary-check PATH runs functional checks on a built candidate without npm.
# --dry-run-version-assertion only self-tests the version comparison predicate.
set -euo pipefail
NPM_PKG="@orbit-tools/cli"

assert_version_matches_tag() {
  local installed_version="$1"
  local expected_version="$2"

  if [[ "$installed_version" != "$expected_version" ]]; then
    echo "FAIL: installed $NPM_PKG version '$installed_version' does not match tag version '$expected_version'" >&2
    return 1
  fi
  echo "versioned smoke => $NPM_PKG@$installed_version matches tag"
}

run_version_assertion_test() {
  assert_version_matches_tag "0.14.0" "0.14.0"
  if assert_version_matches_tag "0.13.0" "0.14.0"; then
    echo "FAIL: version assertion accepted a mismatched version" >&2
    return 1
  fi
  echo "PASS: version assertion rejects a mismatched tag version"
}

if [[ "$#" -eq 1 && "$1" == "--local-package-check" ]]; then
  "$(dirname "$0")/require-python.sh"
  exec python3 "$(dirname "$0")/check_npm_package.py"
fi
if [[ "$#" -eq 1 && "$1" == "--dry-run-version-assertion" ]]; then
  run_version_assertion_test
  exit 0
fi
LOCAL_BINARY=""
if [[ "$#" -eq 2 && "$1" == "--local-binary-check" ]]; then
  LOCAL_BINARY="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
  if [[ ! -x "$LOCAL_BINARY" ]]; then
    echo "FAIL: local candidate is not executable: $LOCAL_BINARY" >&2
    exit 2
  fi
elif [[ "$#" -ne 0 ]]; then
  echo "usage: $0 [--local-package-check|--dry-run-version-assertion|--local-binary-check PATH]" >&2
  exit 2
fi

require_bin() {
  local bin="$1"
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "smoke-npm-install: required binary '$bin' not on PATH" >&2
    exit 2
  fi
}

require_bin node
require_bin git
if [[ -z "$LOCAL_BINARY" ]]; then
  require_bin npx
  require_bin npm
fi

run_orbit() {
  if [[ -n "$LOCAL_BINARY" ]]; then
    "$LOCAL_BINARY" "$@"
  else
    npx -y "$NPM_SPEC" "$@"
  fi
}

REGISTRY_METADATA="server.json"
NPM_PACKAGE_JSON="npm/package.json"
if [[ ! -f "$REGISTRY_METADATA" || ! -f "$NPM_PACKAGE_JSON" ]]; then
  echo "smoke-npm-install: expected $REGISTRY_METADATA and $NPM_PACKAGE_JSON at the repository root" >&2
  exit 2
fi

registry_name="$(node -p "JSON.parse(require('fs').readFileSync('$REGISTRY_METADATA', 'utf8')).name")"
npm_mcp_name="$(node -p "JSON.parse(require('fs').readFileSync('$NPM_PACKAGE_JSON', 'utf8')).mcpName")"
NPM_PKG="$(node -p "JSON.parse(require('fs').readFileSync('$REGISTRY_METADATA', 'utf8')).packages?.[0]?.identifier ?? ''")"
npm_package_version="$(node -p "JSON.parse(require('fs').readFileSync('$NPM_PACKAGE_JSON', 'utf8')).version")"
registry_version="$(node -p "JSON.parse(require('fs').readFileSync('$REGISTRY_METADATA', 'utf8')).version")"
registry_package_version="$(node -p "JSON.parse(require('fs').readFileSync('$REGISTRY_METADATA', 'utf8')).packages?.[0]?.version ?? ''")"

if [[ "$registry_name" != "io.github.constellation-works/orbit" || "$npm_mcp_name" != "$registry_name" ]]; then
  echo "FAIL: registry name and npm mcpName must both be io.github.constellation-works/orbit" >&2
  exit 1
fi
if [[ -z "$NPM_PKG" || "$registry_version" != "$npm_package_version" || "$registry_package_version" != "$registry_version" ]]; then
  echo "FAIL: registry package identity or versions do not match npm/package.json" >&2
  exit 1
fi
if ! node -e "const s=require('./$REGISTRY_METADATA'); const p=s.packages?.[0]; process.exit(Number(p?.registryType !== 'npm' || p?.transport?.type !== 'stdio' || JSON.stringify((p?.packageArguments ?? []).map(a => [a.type, a.value])) !== JSON.stringify([['positional', 'mcp'], ['positional', 'serve']]) || JSON.stringify(s).includes('--operator')));"; then
  echo "FAIL: registry metadata must declare the fixed non-operator npm stdio launch: mcp serve" >&2
  exit 1
fi

NPM_SPEC="$NPM_PKG@$registry_package_version"
ORBIT_COMMAND=(npx -y "$NPM_SPEC")
if [[ -n "$LOCAL_BINARY" ]]; then
  ORBIT_COMMAND=("$LOCAL_BINARY")
fi
PACKAGE_ARGUMENTS=()
while IFS= read -r argument; do
  PACKAGE_ARGUMENTS+=("$argument")
done < <(node -e "for (const argument of require('./$REGISTRY_METADATA').packages[0].packageArguments) console.log(argument.value)")

case "$(uname -s)" in
  Darwin|Linux) ;;
  *)
    echo "smoke-npm-install: unsupported OS '$(uname -s)' — supported: Darwin, Linux" >&2
    exit 2
    ;;
esac

# Keep diagnostics and disposable state inside the checkout's ignored scratch.
mkdir -p .orbit/tmp
TMPDIR_ROOT="$(mktemp -d "$(pwd)/.orbit/tmp/npm-smoke.XXXXXX")"

# A tag-triggered run must prove that npm has the package built from that tag.
# workflow_dispatch supplies SMOKE_NPM_INSTALL_TAG when a releaser reruns
# this workflow from the default branch against a published release tag.
tag_name="${SMOKE_NPM_INSTALL_TAG:-}"
if [[ -z "$tag_name" && "${GITHUB_REF_TYPE:-}" == "tag" ]]; then
  tag_name="${GITHUB_REF_NAME:-}"
fi
if [[ -z "$tag_name" && "${GITHUB_REF:-}" == refs/tags/* ]]; then
  tag_name="${GITHUB_REF#refs/tags/}"
fi
expected_tag_version=""
if [[ -n "$tag_name" ]]; then
  expected_tag_version="${tag_name#v}"
fi

# Cache npx installs inside the temp dir so we exercise a clean download.
export npm_config_cache="$TMPDIR_ROOT/npm-cache"
mkdir -p "$npm_config_cache"

# Sandbox HOME so `orbit init` writes to the temp tree instead of the
# runner/user's real ~/.orbit. Without this, repeat runs locally would
# accumulate state in the developer's global Orbit root.
SMOKE_HOME="$TMPDIR_ROOT/home"
mkdir -p "$SMOKE_HOME"
# Child-only overrides preserve the caller's HOME and authority.
export HOME="$SMOKE_HOME"
export USERPROFILE="$SMOKE_HOME"
unset ORBIT_ROOT
unset ORBIT_REGISTRY_ROOT ORBIT_MANAGED_RUN_CONTEXT ORBIT_RUN_ID ORBIT_TASK_ID
unset ORBIT_AGENT_TOKEN ORBIT_CALLBACK_TOKEN ORBIT_CALLBACK_FD
unset ORBIT_WORKSPACE ORBIT_WORKSPACE_CLAIM_TOKEN ORBIT_WORKTREE_ROOT
unset ORBIT_JOB_DIR ORBIT_ACTIVITY_DIR ORBIT_WORKER_CONTEXT_REQUIRED
unset ORBIT_ACTIVE_TASK_ID ORBIT_SESSION_ID ORBIT_ACTIVITY_ID ORBIT_STEP_INDEX
unset ORBIT_AGENT_NAME ORBIT_AGENT_MODEL ORBIT_ACTOR ORBIT_OPERATOR ORBIT_TASK_ACTOR_KIND
unset ORBIT_ACTIVITY_TOOLS ORBIT_ACTIVITY_TOOL_POLICY ORBIT_ACTIVITY_TOOLS_DENY
unset ORBIT_ACTIVITY_NAME ORBIT_ACTIVITY_FS_PROFILE ORBIT_ACTIVITY_DEADLINE_UNIX_MS
unset ORBIT_PROC_ALLOWED_PROGRAMS ORBIT_PROC_PROGRAM_POLICY ORBIT_PROC_DISALLOWED_PROGRAMS
unset ORBIT_BIN ORBIT_PLUGIN_BROKER
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE
unset GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES

# Pick a timeout binary. macOS runners ship neither `timeout` nor `gtimeout`
# unless coreutils is installed; fall back to perl, which is preinstalled on
# both macOS-15 and ubuntu-22.04 GitHub runners. Perl's `alarm` survives exec,
# so the target process gets SIGALRM at the deadline and exits with rc 142.
if command -v timeout >/dev/null 2>&1; then
  TIMEOUT_KIND=gnu
elif command -v gtimeout >/dev/null 2>&1; then
  TIMEOUT_KIND=gnu_g
elif command -v perl >/dev/null 2>&1; then
  TIMEOUT_KIND=perl
else
  echo "smoke-npm-install: need timeout, gtimeout, or perl on PATH" >&2
  exit 2
fi

run_with_timeout() {
  local secs="$1"; shift
  case "$TIMEOUT_KIND" in
    gnu) timeout "$secs" "$@" ;;
    gnu_g) gtimeout "$secs" "$@" ;;
    perl) perl -e '$t=shift @ARGV; alarm $t; exec @ARGV or die "exec failed: $!\n"' "$secs" "$@" ;;
  esac
}

echo "--- step 1: $NPM_SPEC --version (local binary: ${LOCAL_BINARY:-no}) ---"
VERSION_OUT="$TMPDIR_ROOT/version.txt"
if ! run_orbit --version >"$VERSION_OUT" 2>"$TMPDIR_ROOT/version.err"; then
  echo "FAIL: $NPM_SPEC --version exited non-zero" >&2
  echo "--- stderr ---" >&2
  cat "$TMPDIR_ROOT/version.err" >&2
  exit 1
fi
binary_version="$(sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^orbit[[:space:]]*//' "$VERSION_OUT")"
if [[ -z "$binary_version" ]]; then
  echo "FAIL: orbit --version produced no output" >&2
  cat "$TMPDIR_ROOT/version.err" >&2
  exit 1
fi
echo "orbit --version => $binary_version"
assert_version_matches_tag "$binary_version" "$registry_package_version"
if [[ -n "$expected_tag_version" ]]; then
  assert_version_matches_tag "$binary_version" "$expected_tag_version"
fi

# Also confirm the npm-registry version, for the smoke report.
if [[ -z "$LOCAL_BINARY" ]]; then
  npm_version="$(npm view "$NPM_PKG" version 2>/dev/null || echo '<unknown>')"
  echo "$NPM_PKG@latest on npm => $npm_version"
fi

echo "--- step 2: orbit init + workspace init ---"
# This smoke covers the npm/release install chain, not host sandbox setup.
# Ubuntu 22.04 is outside automatic preparation support, and the runner owns
# its packages and security policy, so skip host preparation on Linux only.
init_host_args=()
if [[ "$(uname -s)" == "Linux" ]]; then
  init_host_args+=(--skip-host-prerequisites)
fi
# `orbit mcp serve` deliberately refuses to bootstrap a workspace (see
# OrbitRuntime::try_initialize_existing) — so without these two commands the
# MCP server attaches but serves an empty tool surface. Initializing first
# matches the documented binary-first installation flow.
# Fresh-host non-interactive init requires machine identity (ORB-10721): a
# machine name plus a 2-5 letter task prefix that is not ORB/ADR/L/F.
if ! run_orbit init "${init_host_args[@]}" --non-interactive \
     --machine-name smoke-npm-install --task-prefix SMK \
     >"$TMPDIR_ROOT/init.out" 2>"$TMPDIR_ROOT/init.err"; then
  echo "FAIL: orbit init exited non-zero" >&2
  echo "--- stdout ---" >&2
  cat "$TMPDIR_ROOT/init.out" >&2
  echo "--- stderr ---" >&2
  cat "$TMPDIR_ROOT/init.err" >&2
  exit 1
fi
echo "orbit init => OK ($SMOKE_HOME/.orbit)"

# No remote connection is made: these are deliberately fake URL credentials.
# Refusal must precede workspace creation or registration and redact the secret.
CREDENTIAL_DIR="$TMPDIR_ROOT/credential-workspace"
mkdir -p "$CREDENTIAL_DIR"
git -C "$CREDENTIAL_DIR" init -q
git -C "$CREDENTIAL_DIR" remote add origin 'https://smoke-user:smoke-fake-secret@example.invalid/orbit.git'
printf 'operator-file\n' >"$CREDENTIAL_DIR/.gitignore"
if ( cd "$CREDENTIAL_DIR" && run_orbit workspace init --json ) \
     >"$TMPDIR_ROOT/credential.out" 2>"$TMPDIR_ROOT/credential.err"; then
  echo "FAIL: workspace init accepted embedded source credentials" >&2
  exit 1
fi
if ! grep -q 'must not contain credentials' "$TMPDIR_ROOT/credential.err" \
   || grep -q 'smoke-fake-secret' "$TMPDIR_ROOT/credential.out" "$TMPDIR_ROOT/credential.err" \
   || [[ -e "$CREDENTIAL_DIR/.orbit" ]] \
   || [[ "$(cat "$CREDENTIAL_DIR/.gitignore")" != 'operator-file' ]]; then
  echo "FAIL: credential refusal was not redacted and free of workspace writes" >&2
  exit 1
fi
node -e '
const fs = require("fs");
const path = process.argv[1];
const registry = fs.existsSync(path) ? JSON.parse(fs.readFileSync(path, "utf8")) : {};
if ((registry.workspaces ?? []).length || (registry.checkouts ?? []).length) process.exit(1);
' "$SMOKE_HOME/.orbit/workspaces.json"
echo "workspace init => rejected fake credentials before registration"

WORKSPACE_DIR="$TMPDIR_ROOT/workspace"
mkdir -p "$WORKSPACE_DIR"
# Establish a repository boundary so discovery cannot walk into this checkout.
git -C "$WORKSPACE_DIR" init -q
if ! ( cd "$WORKSPACE_DIR" && run_orbit workspace init --json ) \
     >"$TMPDIR_ROOT/ws-init.out" 2>"$TMPDIR_ROOT/ws-init.err"; then
  echo "FAIL: orbit workspace init exited non-zero" >&2
  echo "--- stderr ---" >&2
  cat "$TMPDIR_ROOT/ws-init.err" >&2
  exit 1
fi
# Workspace init now offers a browser kit for web repositories. The empty
# install-smoke workspace must stay on the no-web-UI path without a download.
if ! node -e '
const fs = require("fs");
const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (result.browser_kit?.status !== "no_web_ui") process.exit(1);
' "$TMPDIR_ROOT/ws-init.out"; then
  echo "FAIL: empty smoke workspace did not report the no-web-UI browser-kit outcome" >&2
  cat "$TMPDIR_ROOT/ws-init.out" >&2
  exit 1
fi
echo "orbit workspace init => OK ($WORKSPACE_DIR/.orbit; no browser kit needed)"

LAUNCH_DIR="$TMPDIR_ROOT/registry-launch"
mkdir -p "$LAUNCH_DIR"
# A different, empty HOME is the ambient default. Only the explicit ORBIT_ROOT
# can locate the initialized host and its workspace from the launch directory.
export HOME="$TMPDIR_ROOT/ambient-home"
export USERPROFILE="$HOME"
mkdir -p "$HOME"
export ORBIT_ROOT="$SMOKE_HOME/.orbit"

echo "--- step 3: MCP handshake over stdio ---"
RPC_IN="$TMPDIR_ROOT/rpc-in.txt"
RPC_OUT="$TMPDIR_ROOT/rpc-out.txt"
RPC_ERR="$TMPDIR_ROOT/rpc-err.txt"

cat >"$RPC_IN" <<EOF
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke-npm-install","version":"0.0.1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"orbit_workflow_run_list","arguments":{"workspace":"$WORKSPACE_DIR"}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"orbit_task_list","arguments":{}}}
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"orbit_workspace_list","arguments":{}}}
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"orbit_task_list","arguments":{"workspace":"$WORKSPACE_DIR"}}}
EOF

# Feed the requests, then keep stdin open briefly so the server has time
# to flush its responses before EOF closes the channel. The registry launch
# runs outside any workspace: it must not infer this initialized workspace.
{
  cat "$RPC_IN"
  sleep 5
} | ( cd "$LAUNCH_DIR" && run_with_timeout 120 "${ORBIT_COMMAND[@]}" "${PACKAGE_ARGUMENTS[@]}" ) \
     >"$RPC_OUT" 2>"$RPC_ERR" || rc=$?
rc="${rc:-0}"

# Accepted post-handshake exit codes:
#   0   — server saw stdin EOF and exited cleanly (the expected happy path).
#   124 — GNU `timeout` killed it (we never send `shutdown`).
#   143 — SIGTERM (128+15), via GNU `timeout --signal` or external kill.
#   142 — SIGALRM (128+14), via the perl-based timeout fallback on macOS.
if [[ "$rc" -ne 0 && "$rc" -ne 124 && "$rc" -ne 142 && "$rc" -ne 143 ]]; then
  echo "FAIL: mcp serve exited with $rc" >&2
  echo "--- stderr ---" >&2
  cat "$RPC_ERR" >&2
  echo "--- stdout ---" >&2
  cat "$RPC_OUT" >&2
  exit 1
fi

if ! grep -q '"jsonrpc":"2.0"' "$RPC_OUT"; then
  echo "FAIL: no JSON-RPC frames in mcp serve stdout" >&2
  echo "--- stdout ---" >&2
  cat "$RPC_OUT" >&2
  echo "--- stderr ---" >&2
  cat "$RPC_ERR" >&2
  exit 1
fi

# tools/list response should advertise at least one orbit_* tool.
# Tool names are emitted with underscores on the MCP wire (orbit-mcp's
# sanitize_tool_name replaces `.` with `_` for client compatibility), so the
# canonical `orbit.task.show` selector arrives here as `orbit_task_show`.
if ! grep -q '"orbit_' "$RPC_OUT"; then
  echo "FAIL: tools/list response contained no orbit_* tools" >&2
  echo "--- stdout ---" >&2
  cat "$RPC_OUT" >&2
  echo "--- stderr ---" >&2
  cat "$RPC_ERR" >&2
  exit 1
fi

if ! node -e '
const fs = require("fs");
const frames = fs.readFileSync(process.argv[1], "utf8").trim().split(/\n+/).filter(Boolean).map(JSON.parse);
const byId = Object.fromEntries(frames.filter(frame => frame.id !== undefined).map(frame => [frame.id, frame]));
const tools = byId[2]?.result?.tools;
const operatorMessage = byId[3]?.result?.structuredContent?.message ?? "";
const workspaceMessage = byId[4]?.result?.structuredContent?.message ?? "";
const discovery = byId[5]?.result?.structuredContent;
const tasks = byId[6]?.result;
if (!byId[1]?.result?.serverInfo || !Array.isArray(tools) || !tools.some(tool => tool.name === "orbit_workspace_list")) process.exit(1);
if (!/operator/i.test(operatorMessage)) process.exit(1);
if (!workspaceMessage.includes("orbit_workspace_list") || !workspaceMessage.includes("returned `ws_*` ID") || !workspaceMessage.includes("orbit workspace init") || !workspaceMessage.includes("never infers one from the server process cwd")) process.exit(1);
if (discovery?.machine_name !== "smoke-npm-install" || discovery.workspaces?.length !== 1 || discovery.workspaces[0].id !== "ws_workspace") process.exit(1);
if (!tasks || byId[6]?.error || tasks.isError || !Array.isArray(tasks.structuredContent?.tasks)) process.exit(1);
' "$RPC_OUT"; then
  echo "FAIL: registry launch did not initialize, list tools, remain agent-only, and fail closed without a workspace" >&2
  cat "$RPC_OUT" >&2
  exit 1
fi

orbit_tool_count="$(grep -o '"orbit_[a-z_]*"' "$RPC_OUT" | sort -u | wc -l | tr -d '[:space:]')"
echo "tools/list returned $orbit_tool_count distinct orbit_* tools"

if [[ -n "$LOCAL_BINARY" ]]; then
  echo "PASS: local candidate functional smoke (orbit $binary_version); published install/signature chain remains pending"
else
  echo "PASS: npm install chain serves MCP successfully (orbit $binary_version)"
fi
echo "smoke diagnostics: $TMPDIR_ROOT"
