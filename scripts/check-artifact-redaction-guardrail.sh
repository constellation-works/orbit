#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

targets=(
  "crates/orbit-core/src/adapter/tool_host/task_tools.rs"
  "crates/orbit-core/src/adapter/tool_host/friction_tools.rs"
  "crates/orbit-tools/src/builtin/orbit/task"
  "crates/orbit-tools/src/builtin/orbit/friction"
  # [ORB-00417] Write-time redaction paths: task creation (dashboard + core
  # choke point) and provider CLI argv. Redaction must flow through
  # orbit_common::utility::redaction (redact_all / with_argv_secrets), never a
  # surface-local helper here.
  "crates/orbit-core/src/application/task/add.rs"
  "crates/orbit-web/src/api/tasks.rs"
  "crates/orbit-engine/src/activity_job/cli_runner/orchestrator.rs"
  "crates/orbit-engine/src/activity_job/cli_runner/response_diagnostics.rs"
  "crates/orbit-engine/src/activity_job/cli_runner/stdout_preview.rs"
  "crates/orbit-engine/src/activity_job/cli_runner/argv.rs"
)

if rg -n 'fn\s+redact_' "${targets[@]}"; then
  cat >&2 <<'MSG'
Artifact write redaction must flow through orbit_common::utility::redaction and the shared tool-host policy.
Do not add surface-local `fn redact_*` helpers for task or friction tools.
MSG
  exit 1
fi
