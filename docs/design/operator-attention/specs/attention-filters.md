---
type: design
summary: "Spec: every orbit attention filter, its tool-input twin, and how filters combine, validate and order"
last_validated: 2026-11-01
title: Spec — Attention filters
owner: claude
status: Draft
feature: operator-attention
tags: [operator-attention, spec]
related_features: [operator-attention, host-registry, federated-mcp]
related_artifacts: [ORB-21164]
last_updated: 2026-11-01
---

# Spec: Attention filters

`orbit attention` and `orbit.attention` take the same optional filters. They combine with
**AND across different filters and OR within one repeated filter**. A filter narrows the
result; it never changes an item's `severity`, `fingerprint` or `next`, so the same item reads
identically through any filter combination.

## Why This Exists

The feed must serve three readers with different needs: an operator scanning everything, an
orchestrator scoped to one workspace, and a hook that wants only high-severity news. Filters
that behave differently per surface, or silently match nothing on a typo, would make an empty
result untrustworthy.

## Filters

CLI flags use kebab-case; tool inputs use the snake_case twin and accept a string wherever an
array is shown.

| CLI flag | Tool input | Matches | Notes |
|---|---|---|---|
| `--workspace <sel>` (repeatable) | `workspace: string[]` | The item's workspace | A registered name (`orbit`), a `ws_*` id, or a host-qualified selector copied from `orbit workspace list`. A name matches every workspace of that name on every host in scope; add `--host` to narrow. A token that names no workspace is an error (`unknown_workspace`), never an empty result. |
| `--all` | `all: true` | Every workspace the host reaches | Conflicts with `--workspace`. Reaches registered remote hosts as [attention-federation](./attention-federation.md) defines. |
| `--host <name>` (repeatable) | `host: string[]` | The item's host | A host name or `machine_id` as `orbit host list` prints it; the local machine is matched by its own name. An unregistered host is an error. |
| `--kind <kind>` (repeatable) | `kind: string[]` | `item.kind` | Values from the closed set in [attention-item](./attention-item.md#kinds-and-sources-of-truth). An unknown value is a usage error that lists the set. |
| `--exclude-kind <kind>` (repeatable) | `exclude_kind: string[]` | Items whose kind is *not* listed | Wins over `--kind` when a kind is in both. |
| `--severity <min>` | `severity: string` | `item.severity` at or above `min` | `critical` > `high` > `medium` > `low`. |
| `--status <status>` (repeatable) | `status: string[]` | `item.task.status` | Any task status (`proposed`, `blocked`, `review`, …). |
| `--tag <tag>` (repeatable) | `tag: string[]` | Any of `item.task.tags` | OR within the flag. |
| `--task <id>` (repeatable) | `task: string[]` | `item.refs.task_id` | A task id; its prefix also names the host that holds it ([host routing](../../host-registry/specs/host-routing.md)). |
| `--crew <crew>` (repeatable) | `crew: string[]` | `item.task.crew` | The crew assigned to the task. |
| `--orchestrator <crew>` (repeatable) | `orchestrator: string[]` | `item.task.orchestrator` | The orchestrator attributed to the task. |
| `--older-than <duration>` | `older_than: string` | `item.age_seconds` at least the duration | `90m`, `36h`, `3d` (the duration syntax of `orbit run auto --for`). Items with `since: null` never match. |
| `--since <cursor>` | `since: string` | Items new or changed since the cursor | [attention-cursor](./attention-cursor.md). |
| `--limit <n>` | `limit: integer` | Caps `items` after ordering and delta | Default 100, maximum 500. `counts.matching` is the pre-limit total and `counts.truncated` says whether the cap cut anything. |

## Combination semantics

- **Task-scoped filters need a task.** `--status`, `--tag`, `--task`, `--crew` and
  `--orchestrator` match the `task` object of an item. An item with no task (`host_unhealthy`,
  `ci_red` with no repair task, a run with no task) cannot match them and is excluded whenever
  any of them is given. This is the AND rule applied literally, and the echoed `filters` make
  it visible.
- **Scope filters pick sources, item filters pick items.** `--workspace`, `--all` and `--host`
  decide which workspaces and hosts are read at all. The rest filter the derived items.
- **Contradictory combinations are empty, not errors.** `--task ORB-1 --host other` where the
  prefix names a different host returns zero items with both filters echoed. Malformed or
  unresolvable tokens (an unknown workspace, host, kind, status, duration or cursor shape) are
  errors; a well-formed filter that matches nothing is not.
- **Source pushdown.** `--kind` and `--exclude-kind` decide which kinds are read, so
  `--exclude-kind ci_red` skips the network read entirely and `--kind pilot_warning` reads no
  runs or claims. Other filters cannot be pushed down and apply after derivation.
- **Evaluation order.** Resolve scope → read the selected kinds' sources → build items →
  apply item filters → apply `--severity` and `--older-than` → order → apply the cursor →
  apply `--limit` → mint the next cursor. Filters therefore run before the cursor, so a cursor
  only ever tracks items the caller's filters could show.

## Defaults

- **Workspace.** With no scope filter, a call bound to a workspace (a CLI run inside a
  workspace checkout, or an MCP session launched with `--workspace`) reads that workspace;
  an unbound call reads `--all`. `--all` makes the broad reading explicit. A hook should
  always pass its scope explicitly so a changed working directory cannot change its feed.
- **Severity, kinds, limit.** No minimum severity, every kind, `--limit 100`.
- **Exit status.** `0` whenever the query ran, with or without items or `degraded` entries.
  Non-zero only for usage and resolution errors. Automation reads the JSON `counts`; the tool
  does not encode "items exist" in the exit code.

## Examples

```sh
orbit attention --workspace orbit                       # one workspace, every kind
orbit attention --all --severity high --json            # everything high or above
orbit attention --kind pilot_warning --status proposed  # warnings on proposed tasks
orbit attention --all --exclude-kind ci_red --exclude-kind friction_untriaged
orbit attention --task ORB-21164                        # why is this task listed
orbit attention --all --older-than 24h --tag operator-ux
orbit attention --all --since atc1.… --json             # a hook's later injection
```

## Task References

- [ORB-21164] — specified the attention filters

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
