---
title: Tasks
description: "How Orbit models durable work, state transitions, acceptance criteria, and review."
sidebar:
  order: 2
---

## Definition

A task is a durable unit of work. Its bundle lives under the global Orbit root at
`~/.orbit/tasks/workspaces/<workspace>/<task-id>`, not in the repository checkout. It carries a title, description, acceptance criteria, lifecycle state, context, review notes, and audit history.

Use tasks for work an agent can execute and a human can review. Do not use a task as a scratch note when no observable outcome exists.

A task may also declare `required_tools`, which are fixed at creation — see
[Transition rules](#transition-rules).

## Lifecycle

The common path is:

```text
proposed -> backlog -> in-progress -> review -> done
```

Human-created direct tasks may enter the backlog immediately. Proposed tasks must be approved before normal execution. Review tasks are approved or rejected after the agent produces work.

### Approval

Both human gates use the same command:

```bash
orbit task update "$TASK_ID" --approve --note "Scope reviewed."
```

`--approve` takes the task's *next* approval step, chosen from its current
status — `proposed → backlog`, or `review → done`. Because the transition is
derived rather than stated, `--approve` cannot be combined with field edits or
an explicit `--status`. The note is recorded on the status-history entry.

These two gates are independent, and so is the authority that can satisfy them:

- Entry into the backlog is **always** a human decision. No run, flag, or
  scheduler grants it.
- Completion out of `review` can instead be authorized per run with
  `--complete`, which never reaches back and approves `proposed` work. See
  [Completing work with `--complete`](../../getting-started/workflows/#completing-work-with---complete).

Recurring work minted by an [auto-task](../scheduling/#auto-task) enters at
whichever status its definition declares — `backlog` by default, or `proposed`
when you want to review each instance. See [Schedule Recurring
Work](../../how-to/recurring-work/#3-define-recurring-chores-as-auto-tasks) for
defining one.

### Statuses

| Status        | Purpose |
|---------------|---------|
| `proposed`    | Awaiting human approval before entering the backlog. |
| `backlog`     | Approved and queued for work. |
| `someday`     | Future-scoped — wanted but not yet actionable. Agents skip `someday` tasks. |
| `in-progress` | Actively being worked on. |
| `review`      | Implementation complete; awaiting review/merge. Completion out of `review` requires an `execution_summary` or a successful run. |
| `done`        | Accepted and closed. **Terminal** — reopening needs an explicit human override. |
| `blocked`     | Temporarily paused (waiting on a dependency or decision). A failed run sends a task here only once step recovery is exhausted; an agent-declared failed envelope enters recovery first rather than blocking recoverable work. |
| `archived`    | Soft-deleted, with `orbit task archive` or a `--status archived` update. **Terminal** — restore it to any other status with `orbit task update <id> --status <status> --force`. |
| `rejected`    | Declined. Reconsidered to `backlog` or `in-progress` on the lifecycle table; any other status needs `--force`. |

### Transition rules

Every status change — `orbit task update`, the dashboard, and the
`orbit.task.update` tool alike — is checked against one lifecycle table:

```text
proposed → backlog → in-progress → review → done
         ↘ rejected

someday  → backlog | in-progress
blocked  → backlog | in-progress
review   → backlog | in-progress | rejected
rejected → backlog | in-progress   (reconsider)
any open status → blocked | archived
```

1. **Done is reachable only from `review`,** and needs completion evidence: a
   non-empty `execution_summary`, or a `job_run_id` whose run succeeded. An
   agent cannot mark unstarted work done.
2. **`done` and `archived` are terminal.** A regression in delivered work is a
   *new* task with a `regression_from` relation, not a reopened one. Only a
   rejection may be reconsidered, back to `backlog` or `in-progress`.
3. **`in-progress` requires a plan** when it is entered from `proposed`,
   `someday`, or `blocked` — `orbit task update <id> --status in-progress`
   refuses the transition while the plan is empty or still the unauthored
   placeholder. Send `--plan` on the same command.
4. **Required tools are fixed at creation.** `required_tools` is a list of
   exact canonical registered tool names, normalized, sorted, and deduplicated
   when the task is created; older tasks default to an empty list. No update
   surface — `orbit task update`, the dashboard, or `orbit.task.update` —
   accepts `required_tools` afterwards, regardless of status.

A refused transition is an `invalid_input` error naming the from/to pair and
the missing precondition. A human can override the table from either human
surface — `orbit task update <id> --status <status> --force` on the bare CLI, or
the dashboard's status dropdown, whose off-table targets sit under a **force**
group and ask for one confirmation before they are sent. Both record the change
in the task's history as a `forced` event. The `orbit.task.update` tool and the
MCP surface refuse a `force` argument outright, so an agent stays on the table.

Friction reports use their own `orbit friction` surface and are not task
statuses.

## Quality Bar

A good task states:

- what should change
- where the change should happen
- how to observe success
- which files or selectors matter when known

Acceptance criteria should be testable. Prefer "command X exits successfully" or "file Y contains Z" over "the behavior feels better."
