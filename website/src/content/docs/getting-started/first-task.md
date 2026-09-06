---
title: First Task
description: "Create an Orbit task, inspect it, approve it into the backlog, and ship it."
sidebar:
  order: 3
---

A task is the durable unit of work. Everything else in Orbit — shipping,
backlog drains, recurring work, the audit trail — is organized around one.

## Create a task

```bash
TASK_ID=$(orbit task add \
  --title "Create orbit-hello.txt" \
  --description "Add orbit-hello.txt at the repository root containing the text 'hello from orbit'." \
  --acceptance-criteria "orbit-hello.txt exists at the repository root." \
  --acceptance-criteria "orbit-hello.txt contains the text 'hello from orbit'." \
  --complexity low \
  --workspace .)

echo "$TASK_ID"
```

`--title` and `--complexity` are required. Acceptance criteria are effectively
required too: agents self-evaluate against them to decide when the work is
actually done, and a task without them has no finish line. Repeat
`--acceptance-criteria` for each one.

Two options are worth setting early:

- `--context` narrows the work to the files that matter, as `file:`, `dir:`, or
  `symbol:` selectors. See [Choose Scopes](../../how-to/scoping-rules/).
- `--status proposed` parks the task for human approval instead of putting it
  straight into the backlog.

You can also just ask an agent with Orbit's MCP tools available:

```text
create an orbit task for ...
```

## Inspect it

```bash
orbit task list
orbit task show "$TASK_ID"
orbit task lint "$TASK_ID"
```

`orbit task lint` is the quality check: it flags stale paths and vague
acceptance criteria before an agent wastes a run on them.

## Approve it into the backlog

`orbit task add` puts work straight into `backlog` unless you asked for
`proposed`. A `proposed` task needs an explicit approval before anything will
run it:

```bash
orbit task update "$TASK_ID" --approve --note "Scope reviewed."
```

`--approve` takes the task's *next* approval step, chosen from its current
status: `proposed` becomes `backlog`, and later `review` becomes `done`. Because
the transition is derived, `--approve` cannot be combined with field edits or an
explicit `--status`.

## Ship it

```bash
orbit run ship "$TASK_ID"
```

This submits the task through the gated shipment pipeline and prints a durable
run ID immediately — it does not wait for the outcome. The default mode opens a
pull request. Deliver in place instead with:

```bash
orbit run ship --mode local "$TASK_ID"
```

Ship several known tasks in one run, and target a specific base branch:

```bash
orbit run ship "$TASK_ID" "$SECOND_TASK_ID" --base main
```

## Watch it

```bash
orbit run show              # the most recent run
orbit run show "$RUN_ID"
orbit run logs "$RUN_ID"
orbit task show "$TASK_ID"
```

A successful run leaves the task in `review`, not `done` — completion is a
separate, deliberate step. Approve it when you have looked at the result:

```bash
orbit task update "$TASK_ID" --approve
```

If you would rather authorize the run itself to finish delivery, pass
`--complete` when you ship. That is explained in
[Delivery Workflows](../workflows/#completing-work-with---complete).

## Next

- [Delivery Workflows](../workflows/) — the whole `orbit run` surface.
- [Run a Task Lifecycle](../../how-to/task-lifecycle/) — the same path in more detail, including artifacts and review.
- [Run a Continuous Delivery Window](../../how-to/continuous-delivery/) — drain a whole backlog instead of one task.
