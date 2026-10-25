---
title: Investigate Failures from the Terminal
description: "Trace a bounded failure through doctor, run captures, host logs, audit events, and server health checks."
---

Use this workflow when a run fails, a tool call is denied, or the dashboard
appears unhealthy. Start with one host, one workspace, one run, and a short
time window. Keep the evidence before deciding on a repair. For resume,
replay, cancellation, and the meaning of dashboard error counts, use
[Runs and errors](../dashboard/#runs-and-errors).

## Establish the scope and check health

Run these commands on the machine that executed the work. Selecting a
workspace does not move an investigation to another host. For SSH access
and host selection, see [Run Orbit across hosts](../multi-host/).

```bash
orbit workspace list --json
orbit --workspace my-project doctor --problems
```

Replace `my-project` with your registered workspace name. Save its `ws_*`
ID for audit filtering. `doctor` checks configuration, database headers and
schema, disk, indexes, locks, and runs. `--problems` shows only warnings and
failures. Use `doctor --deep` when you need SQLite's page scan, or
`doctor providers` for provider CLI and sandbox readiness diagnostics.

Diagnosis does not authorize a repair. Review a finding before using any
`--fix-*` option; some options change or delete state and have their own
confirmation requirements.

## Separate run output from host logs

Copy the run ID from [dashboard Runs](../dashboard/#runs-and-errors).
Always name it: omitting the ID selects the most recently scheduled run
globally, which may belong to a different investigation.

```bash
RUN_ID='replace-with-your-run-id'
orbit --workspace my-project run show "$RUN_ID" --no-reconcile
orbit --workspace my-project run logs "$RUN_ID" --no-reconcile
```

`run show` gives structured state and a step summary. `run logs` reads
captured stdout/stderr, including the retained output blobs. Narrow it
with `--step <step-id>` using the activity's step ID. `--no-reconcile`
reports stored records as they are; without it, reading a stale run can
finalize it as interrupted, release reservations, and settle pull claims.

The host feed answers a different question: what were Orbit's processes
doing around that failure?

```bash
orbit log tail --since 30m --level warn --lines 100
orbit log tail --since 30m --target orbit.policy --lines 100
```

`--level warn` includes warnings and errors; `--target` matches a tracing
target prefix. The default feed merges the active operational log
`~/.orbit/state/logs/orbit.jsonl` and agent relay feed. It is host-wide;
`--workspace` does not filter these lines. `ORBIT_LOG_PATH` or `--path`
can select another log path. A short tail is a sample, not proof that no
earlier error occurred.

These commands exit after the snapshot. For a live investigation,
`log tail --follow` continues until Ctrl-C; `run logs "$RUN_ID" --follow`
ends when the run becomes terminal. Ctrl-C stops observing a run without
cancelling it.

## Find the relevant audit events

Audit rows record command and tool outcomes, timing, and attribution.
Start with failures and denials separately; the CLI accepts one status per
query, from `success`, `failure`, or `denied`.

```bash
orbit audit list --since 30m --status failure --limit 100 --json
orbit audit list --since 30m --status denied --limit 100 --json
```

The audit log is host-global, even when you pass the global `--workspace`
selector. Only `audit list --workspace-id` filters by trusted stored
workspace identity. Keep an unscoped query too: setup failures and denials
before workspace resolution can have no workspace ID and disappear from
a scoped result.

Replace the workspace ID below with the one from `workspace list`. The
tool name is an exact name from an event's `tool_name` field. Filters
combine, so remove one if a narrow query returns nothing.

```bash
WORKSPACE_ID='ws_replace_with_your_workspace_id'
orbit audit list --since 30m --workspace-id "$WORKSPACE_ID" \
  --run "$RUN_ID" --limit 100 --json
orbit audit list --since '2026-10-24T18:00:00Z' \
  --tool orbit.task.show --status failure --limit 100 --json
orbit audit stats --since 30m --tool orbit.task.show
```

`--since` accepts a duration or RFC3339 timestamp. It sets a lower bound;
there is no upper-bound flag. Results are newest first and list defaults
to 100 rows. If you reach the limit, narrow the window or filters before
drawing conclusions. `stats` reports totals and durations for its time
and tool selection; it does not accept list's run, workspace-ID, or status
filters.

For calls across machines, list also supports `--caller-machine`,
`--process-machine`, `--transport local|ssh-mcp`, `--capability`,
`--origin-session`, `--mcp-call`, and `--lease`. Use stored values from
the event to correlate a caller with the process that executed it.
An authenticated `role` and effective capabilities describe authority;
`self_reported_actor` is an unverified caller claim. A denial is evidence
of a refused operation, not permission to bypass its policy.

## Inspect and preserve the evidence

Take the numeric `id` from a JSON list result:

```bash
EVENT_ID='replace-with-a-numeric-event-id'
orbit audit show "$EVENT_ID" --json
mkdir -p .orbit/tmp/investigation
orbit audit show "$EVENT_ID" --json \
  > .orbit/tmp/investigation/event.json
orbit --workspace my-project run logs "$RUN_ID" --no-reconcile --json \
  > .orbit/tmp/investigation/run-logs.json
```

Event JSON includes arguments, error details, correlation fields, and
`stdout_truncated` / `stderr_truncated` when present. Those snippets are
not full captures. Run-log JSON contains `records` with `step_id`,
`stdout_blob_ref`, `stderr_blob_ref`, and the resolved `stdout` / `stderr`
text. This is the terminal path for inspecting and exporting a run's
retained output; missing or already reclaimed blobs cannot be recovered
from a truncated audit snippet.

To preserve all matching command-audit events for a time/tool selection:

```bash
orbit audit export --since 30m --tool orbit.task.show --format json \
  --output .orbit/tmp/investigation/audit.json
```

`export` supports JSON or CSV and writes the data to `--output`; stdout
is the confirmation. It exports all matches rather than list's default
100-row sample. It accepts only `--since` and `--tool` as event filters:
it has no status, run, or workspace-ID filter, and `--workspace` does not
make the export workspace-scoped. Save a filtered `audit list --json`
result separately if you need that narrower sample. Use a fresh output
path because an export can overwrite an existing file. Review arguments,
paths, and output for sensitive material before sharing the files.

## Check dashboard server health

For connection failures or uptime monitoring, use the existing
[dashboard Troubleshooting guide](../dashboard/#troubleshooting). It covers
plain `/healthz` liveness and detailed storage/log readiness probes. A live
server alone does not establish that a provider or delivery can succeed;
use `doctor providers` and the run evidence above for those failures.

## Retention and destructive cleanup

Preserve evidence before cleanup. History is finite:

- `retention.audit_days` defaults to 60 days. `orbit gc audit` reports
  reclaimable host-wide command-audit rows, the selected workspace's run
  audit (plus rows for workspace IDs no longer registered), and that
  workspace's unreferenced audit blobs. Another registered workspace
  keeps its own retention window. Deletion requires `--apply` or an
  enabled `store-gc` routine; the setting alone does not delete history.
- `runtime.log_retention_days` defaults to seven days for archives of
  both operational and agent feeds. Their separate size budgets can
  evict archives sooner. `log tail` merges active feeds, not every archive.

Check the effective values and the non-destructive GC report:

```bash
orbit --workspace my-project config get retention.audit_days
orbit config get runtime.log_retention_days
orbit --workspace my-project gc audit
```

See [retention configuration](../../reference/config/#settable-keys)
for the full policy. Raw events and unreferenced blobs removed by GC are
not historical rollups you can query later.

`audit prune` is separate manual cleanup. It permanently deletes matching
host-global command-audit rows and has no workspace-ID filter or preview
mode. It requires explicit confirmation; omitting `--confirm` refuses
the deletion. After exporting any evidence you must keep, an operator
who intends to delete rows older than 90 days can run:

```bash
orbit audit prune --older-than 90d --confirm
```

This is not an investigation step or a way to repair a failed run. Do not
assume a global `--workspace` selector limits its deletion scope.
