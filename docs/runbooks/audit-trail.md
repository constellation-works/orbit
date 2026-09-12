---
type: runbook
summary: Query and interpret Orbit invocation, run, step, and activity audit history.
tags: [operations, audit, observability, debugging]
paths: ["crates/orbit-core/src/runtime/run_audit.rs", "crates/orbit-types/src/telemetry/audit_event.rs"]
related_features: [auditability, activity-job]
related_artifacts: [ORB-10014, ORB-10227, ORB-10228]
last_validated: 2026-09-12
---

# Inspect the Audit Trail

Use this runbook to determine which Orbit invocation or pipeline step ran, failed, or was
denied and to correlate that event with a task or job run.

## Storage and durability

Audit events are stored in SQLite, not the JSONL process log. The global store
`~/.orbit/orbit.db` contains:

- `audit_events`: one row per CLI or MCP invocation, written by an RAII guard so even crashes
  record a failure row; and
- `v2_audit_events`: the run → step → activity envelope tree, keyed by `run_id`.

Audit-write handling depends on the boundary. A CLI RAII guard that fails while unwinding
warns without masking the command result. A tool implementation that succeeds but cannot
persist its runtime audit row fails the tool call closed and does not surface a successful
result ([ORB-10227]). A tool that already failed keeps its implementation error authoritative.

## Query audit events

```sh
orbit audit list --since 1h --status failure     # recent failures
orbit audit list --transport local --capability agent
orbit audit list --origin-session <id> --mcp-call <id>
orbit audit list --workspace <id> --caller-machine <id> --process-machine <id>
orbit audit list --run <jrun-id> --lease <lease-id>
orbit audit list --json --limit 100              # full event objects
orbit audit show <id>
orbit audit stats --since 7d
orbit audit export --output audit.json           # JSON or --format csv
orbit audit prune --older-than 90d --confirm
```

Per-invocation fields include `id`, `execution_id`, `timestamp`, `command`, `subcommand`,
`tool_name`, `target_type`, `target_id`, `role`, `status` (`success|failure|denied`),
`exit_code`, `duration_ms`, `working_directory`, `arguments_json`, `stdout_truncated`,
`stderr_truncated`, `error_message`, `host`, `pid`, `session_id`, `task_id`, `job_run_id`,
`activity_id`, and `step_index`. MCP rows add optional `workspace_id`, caller/process
`machine_id` and display `host_id`, `transport`, the complete `effective_capabilities` set,
`origin_session_id`, `mcp_call_id`, and `lease_id`.

Compatibility matters when interpreting those fields: legacy `host` is always the hostname of
the executing process, not the caller; `session_id` is unchanged; and `job_run_id` remains the
canonical run correlation. `origin_session_id` groups MCP calls while `mcp_call_id` identifies
one call. Standalone MCP rows have role `unverified`, local transport, and exactly the `agent`
capability. Trusted managed-envelope identity may replace `unverified`; client JSON may not.

## Find recent failures and causes

```sh
orbit audit export --output /tmp/audit.json
jq -r '.[] | select(.status=="failure")
       | "\(.timestamp[0:19]) \(.command) \(.subcommand // "-") \(.error_message // "-")"' /tmp/audit.json
```

For a single run's tree and event stream, prefer:

```sh
orbit run events <run_id>
orbit run trace <run_id>
```

## Pruning safety

`orbit audit prune` permanently removes audit history older than the requested window. Export
or back up `~/.orbit/orbit.db` first when that history may be needed for incident review.

Related: [Recover stuck job runs](./stuck-job-runs.md) ·
[Inventory and protect Orbit state](./state-and-backup.md) ·
[Inspect and retain logs](./logging.md).
