---
title: Find what needs attention
description: "List what is waiting on an operator across workspaces and hosts, filter it, and follow changes with a cursor."
---

`orbit attention` lists what is waiting on an operator, with the reason and a
suggested next command for each item. It is read-only: it runs none of the
commands it suggests, no session hooks and no background work.

```bash
orbit attention                # the current workspace
orbit attention --all --json   # every workspace and host, as JSON
```

Each row shows severity, kind, workspace, reference, age, reason and the
suggested command. Federated results also show the host. To act on an item,
run the command or open the workspace in the [dashboard](/how-to/dashboard/).

The kinds are `pilot_warning`, `pilot_hold`, `blocked`, `decision_open`,
`review_waiting`, `run_failed`, `run_stuck`, `claim_stale`, `ci_red`,
`friction_untriaged`, `notice_open`, `resident_stalled`, `routine_tripped`
(a routine whose circuit breaker is open after repeated failures) and
`host_unhealthy`. An open decision may be folded into its hold, block or review.

:::caution
An empty list does not prove that nothing waits. Check `degraded` in the JSON
output, or the messages below the table: a source or host may have been
unreadable. The `run_failed`, `run_stuck`, `claim_stale` and `host_unhealthy`
kinds need Operator capability; a session without it reports those sources as
degraded. A live resident cycle reads them, read-only, for the workspaces its
mandate covers. On a replica, a `run_failed` item that names a task is also
degraded: the owner's task status is not readable there, so the item does not
by itself mean the task is still open. In JSON, `detail.held_claim` is true
only while that run's claim is still live; `detail.claimed_leaf` records that
the run was a claimed leaf, including after the claim settled.
:::

## Choose scope and filter

`--all` reads every active workspace on this host and every registered host.
To pick workspaces instead, repeat `--workspace` with registered names or IDs;
it conflicts with `--all`. Repeat `--host` to narrow the hosts by name or
machine ID.

| Option | Effect |
| --- | --- |
| `--kind`, `--exclude-kind` | Include or exclude kinds. Repeatable. |
| `--severity` | Minimum severity: `low`, `medium`, `high` or `critical`. |
| `--status`, `--tag`, `--task` | Filter by task lifecycle state, tag or ID. Repeatable. |
| `--crew`, `--orchestrator` | Filter by assigned crew or attributed orchestrator. Repeatable. |
| `--older-than` | Minimum age, for example `90m`, `36h` or `3d`. |
| `--limit` | Maximum items, 1–500. The default is 100. |
| `--since` | The cursor from a previous response. |

Different options combine with AND; repeated values of one option combine with
OR.

```bash
orbit attention --all --severity high --kind run_failed --kind run_stuck
```

## Follow changes

To poll, use `--json`, save the response's `cursor.next`, and pass it back
with `--since`. The next response returns only new or changed items.

- When `--limit` truncates the result, the response says so. Continue with the
  new `cursor.next`.
- An expired or invalid cursor restarts the full list.
- Check `degraded` on every read.

## From an MCP client

The same read is the `orbit.attention` tool in the
[`tasks-read` toolset](/how-to/mcp-integration/#toolsets). A session bound to
one workspace cannot use `all`, `host` or another workspace to widen its
scope.
