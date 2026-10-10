---
title: Tasks
description: "How Orbit models durable work: statuses, the two approval gates, transition rules, and acceptance criteria."
sidebar:
  order: 2
---

## Definition

A task is a durable unit of work that an agent executes and you review. It
carries a title, description, acceptance criteria, status, context, review
notes, and history. Its bundle lives under
`~/.orbit/tasks/workspaces/<workspace>/<task-id>`, not in your repository.

Every task needs an observable outcome. Don't file one as a scratch note.

A task can also declare `required_tools`: Orbit tools its agent needs beyond
the activity's baseline. They are fixed at creation; see
[Transition rules](#transition-rules).

## Lifecycle

The common path:

```text
proposed -> backlog -> in-progress -> review -> done
```

A `proposed` task needs your approval before a run picks it up. A task you
create directly can start in the backlog (`orbit task add --status backlog`).
Once the agent delivers, the task waits in `review` until you approve or reject
it.

### Approval

You pass both gates with the same command:

```bash
orbit task update "$TASK_ID" --approve --note "Scope reviewed."
```

`--approve` takes the task's next approval step from its current status:
`proposed → backlog`, or `review → done`. Because Orbit picks the transition,
you can't combine `--approve` with field edits or `--status`. The note is
recorded in the task's status history.

The two gates are independent:

- **Entering the backlog takes an explicit approval.** You give it in the
  dashboard or the CLI, or your agent gives it with `orbit.task.update` when
  you ask; the task's history records who. You can also authorize a local
  drain with `orbit run auto --approve-proposed`: on every pass it pilots
  qualifying `proposed` tasks and approves those it verifies, including tasks
  filed during the window. A task qualifies with the `no-diff-expected` tag,
  or with context files and an assessed complexity. Duplicate, already-landed,
  conflict or warning findings keep it proposed, as does `no-auto-approve`.
  This is off by default and refused with `--pull`; only the owner approves
  work. An enabled CI failure sweep can also promote the repair tasks it
  files once its pilot validates them.
- **Completing out of `review`** can instead be authorized per run with
  `--complete`, or for distributed handoffs with the owner's
  `workflow.distributed_completion = "done"`. Completion never approves
  `proposed` work. See
  [Completing work with `--complete`](../../getting-started/workflows/#completing-work-with---complete).

An [auto-task](../scheduling/#auto-task) files each recurring task at the
status its definition declares: `backlog` by default, or `proposed` when you
want to approve each one. See
[Schedule Recurring Work](../../how-to/recurring-work/#3-define-recurring-chores-as-auto-tasks).

### Statuses

| Status        | Meaning |
|---------------|---------|
| `proposed`    | Waiting for your approval. |
| `backlog`     | Approved and queued. |
| `someday`     | Wanted, but not actionable yet. Agents skip it. |
| `in-progress` | Being worked on. |
| `review`      | Delivered and waiting for review or merge. Moving to `done` needs an `execution_summary` or a successful run. |
| `done`        | Accepted and closed. **Terminal**: only a human override reopens it. |
| `blocked`     | Paused on a dependency, a decision, or a failure. A failed run lands here only after step recovery is exhausted. If dispatch could not find the provider CLI's launcher, the task is *infra-blocked*: `orbit doctor` lists it under `infra-blocked-tasks`, and `orbit task recheck-blocked --confirm` returns it to `backlog` once the launcher resolves. |
| `archived`    | Soft-deleted with `orbit task archive` or `--status archived`. **Terminal**: restore it with `orbit task update <id> --status <status> --force`. |
| `rejected`    | Declined. It can be reconsidered back to `backlog` or `in-progress`; any other status needs `--force`. |

### Transition rules

Every status change is checked against one lifecycle table, whether it comes
from `orbit task update`, the dashboard, or the `orbit.task.update` tool:

```text
proposed    → backlog | someday | in-progress | rejected
backlog     → proposed | someday | in-progress | rejected
someday     → backlog | in-progress | rejected
in-progress → backlog | someday | review | rejected
review      → backlog | in-progress | done | rejected
blocked     → backlog | in-progress
rejected    → backlog | in-progress   (reconsider)
any open status → blocked | archived
```

1. **`done` is reachable only from `review`,** and it needs evidence: a
   non-empty `execution_summary`, or a `job_run_id` whose run succeeded. An
   agent can't mark unstarted work done.
2. **`done` and `archived` are terminal.** A regression in delivered work gets
   a *new* task with a `regression_from` relation; the old one stays closed.
3. **Starting work needs a plan.** Moving to `in-progress` from any status
   other than `backlog` is refused while the plan is empty or still the
   unauthored placeholder. Send `--plan` on the same command.
4. **Required tools are fixed at creation.** `required_tools` holds exact
   registered tool names, normalized when the task is created. No update path
   (`orbit task update`, the dashboard, or `orbit.task.update`) accepts it
   afterwards.

A refused transition is an `invalid_input` error that names both statuses and
the missing precondition.

As a human, you can override the table: `orbit task update <id> --status
<status> --force` on the CLI, or the **force (off-table)** group in the
dashboard's status menu, which asks you to confirm first. Both record a
`forced` event in the task's history. The `orbit.task.update` tool refuses a
`force` argument, so agents stay on the table.

Friction reports are separate records under `orbit friction`, not task
statuses.

## Quality bar

A good task states:

- what should change
- where the change should happen
- how to observe success
- which files or selectors matter, when known

## Review layers

Orbit can review a delivery at three points. Before-PR and before-landing
review are alternatives: `review.before_pr` and `review.before_landing` cannot
both be `true`. After-landing review is a separate switch and can be enabled
alongside either one. The two config switches default to `false`; the
after-landing auto-task ships disabled.

| Layer | When it runs | What it can change | How to enable it | What it blocks |
|---|---|---|---|---|
| Before-PR | Before Orbit opens the pull request. | The reviewer can fix the candidate before the PR exists. | Set `review.before_pr = true` with `orbit config set review.before_pr true`. | PR creation waits for review and fixes. It requires the PR delivery route. |
| Before-landing | After Orbit opens the pull request, while hosted CI runs. | The reviewer can fix the PR head; Orbit validates and pushes the fix onto the open PR. | Set `review.before_landing = true` with `orbit config set review.before_landing true`. | Merge waits for an approved, settled head. Any other outcome leaves the PR open and the task in `review`. |
| After-landing | In batches after deliveries land. | The review task records confirmed findings as follow-up bug tasks; it does not change an already-landed delivery. | Run `orbit auto-task toggle delivery-code-review on`. | It does not block a delivery from landing. A batch remains owed until it has valid review evidence. |

The dashboard's Config tab shows all three switches and their source. For the
same effective state, use `orbit config show` or `orbit doctor`; the first two
reviews show their time limit, and after-landing shows when its next batch is
due. After-landing review is independent of the choice between before-PR and
before-landing review.

Write testable acceptance criteria. Prefer "command X exits successfully" or
"file Y contains Z" over "the behavior feels better."
