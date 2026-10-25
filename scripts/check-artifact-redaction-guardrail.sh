#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

targets=(
  "crates/orbit-core/src/adapter/tool_host/task"
  "crates/orbit-core/src/adapter/tool_host/friction_tools.rs"
  "crates/orbit-tools/src/builtin/orbit/task"
  "crates/orbit-tools/src/builtin/orbit/operation.rs"
  # [ORB-00417] Write-time redaction paths: task creation (dashboard + core
  # choke point) and provider CLI argv. Redaction must flow through
  # orbit_common::security::redaction (redact_all / argv_redactor), never a
  # surface-local helper here.
  "crates/orbit-core/src/application/task/add.rs"
  "crates/orbit-web/src/api/tasks.rs"
  "crates/orbit-engine/src/activity_job/cli_runner/orchestrator"
  "crates/orbit-engine/src/activity_job/cli_runner/response_diagnostics.rs"
  "crates/orbit-engine/src/activity_job/cli_runner/stdout_preview.rs"
  "crates/orbit-engine/src/activity_job/cli_runner/argv.rs"
)

if ! command -v rg >/dev/null 2>&1; then
  echo "check-artifact-redaction-guardrail: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

# Fail closed: ripgrep exits 2 on a missing path even when it printed a match
# elsewhere, which would take the `if` pass branch and ship a violation.
# Assert every target exists before scanning.
missing=0
for target in "${targets[@]}"; do
  if [[ ! -e "$target" ]]; then
    echo "artifact redaction guard: missing target: $target" >&2
    missing=1
  fi
done
if [[ "$missing" -ne 0 ]]; then
  echo "artifact redaction guard: refusing to pass with missing targets" >&2
  exit 1
fi

# Capture ripgrep's status: 0 is a violation, 1 is clean, anything else is a
# scan error and must fail closed rather than pass.
set +e
rg_output="$(rg -n 'fn\s+redact_' "${targets[@]}" 2>&1)"
rg_status=$?
set -e
if [[ "$rg_status" -eq 0 ]]; then
  printf '%s\n' "$rg_output"
  cat >&2 <<'MSG'
Artifact write redaction must flow through orbit_common::utility::redaction and the shared tool-host policy.
Do not add surface-local `fn redact_*` helpers for task or friction tools.
MSG
  exit 1
elif [[ "$rg_status" -ne 1 ]]; then
  printf '%s\n' "$rg_output" >&2
  echo "artifact redaction guard: ripgrep failed (exit $rg_status); failing closed" >&2
  exit 1
fi
