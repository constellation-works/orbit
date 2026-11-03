---
title: Find what needs attention
description: "Find operator waits across Orbit workspaces and hosts, filter them, and follow changes with a cursor."
---

`orbit attention` collects what is waiting on an operator in one read-only
list. Start with a snapshot, then inspect an item's reason and suggested next
command before deciding what to do:

```bash
orbit attention
orbit attention --all --json
```

The list covers pilot warnings and holds, blocked tasks, open decisions, review
waits, failed or stuck runs, stale claims, red CI, untriaged friction, open
notices, stalled residents and unhealthy hosts. A decision may be folded into
its hold, block or review. Results show severity, kind, workspace, reference,
age, reason and a suggested command; federated results also show the host.
Suggested recovery or cancellation commands do not run automatically. The query
runs no session hooks or background work.

Check `degraded` in JSON output, or the degradation messages below the table,
before interpreting an empty list: a source or host may be unreadable. Runtime
kinds require Operator capability; a session without it reports the affected
sources as degraded. Use the [dashboard](../dashboard/) to inspect and act on
individual workspaces and runs after finding a wait here.

## Choose scope and filter

`--all` reads every active workspace on this host and every registered host.
Alternatively, repeat `--workspace` for registered workspace names or IDs;
`--workspace` conflicts with `--all`. Repeat `--host` to narrow the hosts by
name or machine ID.

Use these options to focus the list:

| Option | Effect |
| --- | --- |
| `--kind`, `--exclude-kind` | Include or exclude kinds; repeat either option. Run `orbit attention --help` for the available values. |
| `--severity` | Set the minimum severity: `low`, `medium`, `high` or `critical`. |
| `--status`, `--tag`, `--task` | Filter by task lifecycle state, tag or ID; each is repeatable. |
| `--crew`, `--orchestrator` | Filter by assigned crew or attributed orchestrator; each is repeatable. |
| `--older-than` | Set a minimum age, for example `90m`, `36h` or `3d`. |
| `--limit` | Limit returned items to 1–500; the default is 100. |
| `--since` | Supply the opaque cursor from a previous response. |

Filters combine with AND across fields and OR within repeated values. Kind
selection happens before sources are read. For example:

```bash
orbit attention --all --severity high --kind run_failed --kind run_stuck
```

## Follow changes

For repeated reads, use `--json` and save the response's `cursor.next`. Pass it
back with `--since` to see new or changed items; unchanged items are omitted.
The response reports cursor status and counts, including whether the result was
truncated. Continue with the new `cursor.next` when a limit leaves items to
return. An expired, invalid or unsupported cursor restarts the full list; the
cursor is stateless and can itself be truncated when its seen set is large.
Check `degraded` on each read as well.

The same read is available to MCP clients as `orbit.attention` in the
[`tasks-read` toolset](../mcp-integration/#toolsets). A bound MCP session cannot
use `all`, `host` or another workspace to widen its scope.
