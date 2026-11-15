---
title: Investigate failures from the terminal
description: "Trace a failed run or a denied tool call through doctor, run output, host logs and audit events."
---

When a run fails or a tool call is denied, work through four sources in
order: `orbit doctor`, the run's captured output, the host log, and the audit
log. Keep the scope small: one host, one workspace, one run, a short time
window. To resume, replay or cancel a run, see
[Runs and errors](/how-to/dashboard/#runs-and-errors).

## Establish the scope and check health

Run these on the machine that executed the work. Selecting a workspace does
not move the investigation to another host; for that, see
[Run Orbit across hosts](/how-to/multi-host/).

```bash
orbit workspace list --json
orbit --workspace my-project doctor --problems
```

Note the workspace's `ws_*` ID for audit filtering. `--problems` prints only
warnings and failures.

| Command | Use it for |
|---|---|
| `orbit doctor --deep` | A full SQLite page scan of the databases. |
| `orbit doctor providers` | Provider CLI and sandbox readiness. |
| `orbit doctor --check <name>` | One check with its full message and remediation. |

:::caution
Review a finding before you use a `--fix-*` option. Some change or delete
state.
:::

## Separate run output from host logs

Copy the run ID from the dashboard's Runs view. Always name it: without an
ID, these commands select the most recently scheduled run on the machine.

```bash
RUN_ID='replace-with-your-run-id'
orbit --workspace my-project run show "$RUN_ID" --no-reconcile
orbit --workspace my-project run logs "$RUN_ID" --no-reconcile
```

`run show` gives the run's state and a step summary. `run logs` prints the
captured stdout and stderr; narrow it with `--step <step-id>`.

Keep `--no-reconcile` while you investigate. Without it, reading a stale run
can finalize it as interrupted, release its reservations, and settle its pull
claims.

The host log shows what Orbit's processes were doing around the failure:

```bash
orbit log tail --since 30m --level warn --lines 100
orbit log tail --since 30m --target orbit.policy --lines 100
```

`--level warn` includes errors. `--target` matches a tracing target prefix.
The log is host-wide: `--workspace` does not filter it. A short tail is a
sample, not proof that nothing failed earlier.

These commands print and exit. Add `--follow` to keep watching: `log tail`
runs until Ctrl-C, and `run logs` ends when the run does. Ctrl-C stops the
watching, not the run.

## Find the relevant audit events

The audit log records command and tool outcomes, with timing and
attribution. `--status` takes one value per query: `success`, `failure` or
`denied`.

```bash
orbit audit list --since 30m --status failure --limit 100 --json
orbit audit list --since 30m --status denied --limit 100 --json
```

The audit log is host-wide too. The global `--workspace` selector does not
filter it; only `--workspace-id` does. Run an unscoped query as well: a
failure before Orbit resolved the workspace has no workspace ID.

Then narrow by run or tool. Filters combine, so drop one if a query returns
nothing.

```bash
WORKSPACE_ID='ws_replace_with_your_workspace_id'
orbit audit list --since 30m --workspace-id "$WORKSPACE_ID" \
  --run "$RUN_ID" --limit 100 --json
orbit audit list --since '2026-10-24T18:00:00Z' \
  --tool orbit.task.show --status failure --limit 100 --json
orbit audit stats --since 30m --tool orbit.task.show
```

- `--since` takes a duration or an RFC 3339 timestamp. There is no upper
  bound flag.
- Results are newest first, 100 rows by default. If you hit the limit, narrow
  the query before you draw conclusions.
- `--tool` takes the exact name from an event's `tool_name` field.
- `stats` accepts only `--since` and `--tool`.

For calls across machines, `audit list` also filters by `--caller-machine`,
`--process-machine`, `--transport local|ssh-mcp`, `--capability`,
`--origin-session`, `--mcp-call` and `--lease`. In an event, `role` is
authenticated; `self_reported_actor` is an unverified claim by the caller.

## Inspect and preserve the evidence

Take the numeric `id` from a list result:

```bash
EVENT_ID='replace-with-a-numeric-event-id'
mkdir -p .orbit/tmp/investigation
orbit audit show "$EVENT_ID" --json \
  > .orbit/tmp/investigation/event.json
orbit --workspace my-project run logs "$RUN_ID" --no-reconcile --json \
  > .orbit/tmp/investigation/run-logs.json
```

The event JSON holds arguments, error details and truncated output snippets
(`stdout_truncated`, `stderr_truncated`). The full output is in the run-log
JSON: each entry in `records` has a `step_id` and the `stdout` and `stderr`
text. Once Orbit reclaims a run's output, an audit snippet cannot replace it.

To save every audit event for a time window and tool:

```bash
orbit audit export --since 30m --tool orbit.task.show --format json \
  --output .orbit/tmp/investigation/audit.json
```

`export` writes JSON or CSV to `--output` and includes every match, not a
100-row sample. It filters only by `--since` and `--tool`; for a narrower
set, save an `audit list --json` result. Use a fresh output path, and check
the files for sensitive arguments and output before you share them.

## Check dashboard server health

If the dashboard does not respond, see its
[Troubleshooting guide](/how-to/dashboard/#troubleshooting) for the health
probes. A live server does not show that a provider or a delivery works; use
`orbit doctor providers` and the run evidence for that.

## Retention and destructive cleanup

History is finite. Save evidence before it ages out or you clean up.

- **Audit rows and output:** kept for `retention.audit_days`, 60 by default.
  Nothing is deleted until you run `orbit gc audit --confirm` or enable the
  `store-gc` routine. Without `--confirm`, `orbit gc audit` only reports what
  it would delete.
- **Host log archives:** kept for seven days, or less when they exceed their
  size budget. `log tail` reads the active log, not every archive.

```bash
orbit --workspace my-project config get retention.audit_days
orbit --workspace my-project gc audit
```

See [retention configuration](/reference/config/#settable-keys) for the
full policy.

`orbit audit prune` is a separate, manual deletion. It permanently deletes
host-wide audit rows older than the age you give. It has no preview and no
workspace filter, and it refuses without `--confirm`:

```bash
orbit audit prune --older-than 90d --confirm
```
