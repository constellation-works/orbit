---
title: First Task
description: "Create a proposed Orbit task, approve it into the backlog, ship it, and inspect the run and review artifact."
sidebar:
  order: 3
---

A task is the durable unit of work. Everything else in Orbit — shipping,
backlog drains, recurring work, the audit trail — is organized around one.

This page is one sequence in a prepared disposable repository: create a
`proposed` task, inspect it, approve it into `backlog`, ship it, then inspect
the run and the review artifact. Later sections cover other shipment shapes.

## Before you start

Complete [Install Orbit](../install/) first. You need all of the following
before you create work or dispatch a run:

- `orbit` on your `PATH`. Confirm with `orbit --version`.
- Global and workspace state: [`orbit init`](../install/#initialize-state),
  then [`orbit workspace init`](../install/#initialize-state) in the
  repository.
- An authenticated provider CLI. Dispatch runs through it. `orbit init`
  probes `PATH` for the executors it supports; see
  [Prerequisites](../install/#prerequisites) and the
  [setup explorer](../../concepts/agents/#set-up-an-executor).
- For the default PR mode, the GitHub CLI (`gh`) authenticated in the same
  environment. See [Prerequisites](../install/#prerequisites).
- On Linux, a working Bubblewrap sandbox before the first dispatch. See
  [Prepare the sandbox](../install/#prepare-the-sandbox).

`orbit doctor` is the workspace health check after that setup.

The commands below assume you are in a disposable git repository that already
has `orbit workspace init` completed, so they do not touch a real project.

## Create a task

`orbit task add` prints the new task ID and nothing else. Capture it.
New tasks enter `proposed` unless you pass another `--status`; the snippet
sets that status explicitly so the rest of the sequence matches.

```bash
TASK_ID=$(orbit task add \
  --title "Create orbit-hello.txt" \
  --description "Add orbit-hello.txt at the repository root containing the text 'hello from orbit'." \
  --acceptance-criteria "orbit-hello.txt exists at the repository root." \
  --acceptance-criteria "orbit-hello.txt contains the text 'hello from orbit'." \
  --complexity low \
  --status proposed \
  --workspace .)

echo "$TASK_ID"
```

`--title` and `--complexity` are required. Acceptance criteria are effectively
required too: agents self-evaluate against them to decide when the work is
actually done, and a task without them has no finish line. Repeat
`--acceptance-criteria` for each one.

`--context` narrows the work to the files that matter, as `file:`, `dir:`, or
`symbol:` selectors. See [Choose Scopes](../../how-to/scoping-rules/).

You can also just ask an agent with Orbit's MCP tools available:

```text
create an orbit task for ...
```

## Inspect it

The task should be `proposed`:

```bash
orbit task list
orbit task show "$TASK_ID"
orbit task lint "$TASK_ID"
```

`orbit task lint` is the quality check: it flags stale paths and vague
acceptance criteria before an agent wastes a run on them.

## Approve it into the backlog

A `proposed` task needs an explicit approval before anything will run it:

```bash
orbit task update "$TASK_ID" --approve --note "Scope reviewed."
```

`--approve` takes the task's *next* approval step from its current status:
`proposed` becomes `backlog`. The same flag later takes `review` to `done`.
Because the transition is derived, `--approve` cannot be combined with field
edits or an explicit `--status`. Approving a task that is already `backlog`
is refused — that is a different status, not a missing approval.

After this command, `orbit task show "$TASK_ID"` reports `backlog`.

## Ship it

```bash
orbit run ship "$TASK_ID"
```

This submits the task through the gated shipment pipeline and prints a durable
run identifier immediately — it does not wait for the outcome. Copy the
`Run ID:` value:

```text
Workflow: ship
Job ID: task_auto_pipeline
Run ID: jrun-YYYYMMDD-HHMM-N
State: submitted
Inspect: orbit run history -j task_auto_pipeline | orbit run show jrun-YYYYMMDD-HHMM-N
```

```bash
RUN_ID=jrun-YYYYMMDD-HHMM-N   # paste the printed identifier
```

`orbit run ship "$TASK_ID" --json` prints the same fields as an object. Copy
`run_id` from that object if you prefer structured output.

The default mode opens a pull request and requires `gh` as in
[Before you start](#before-you-start).

## Watch it

```bash
orbit run show "$RUN_ID"
orbit run logs "$RUN_ID"
orbit task show "$TASK_ID"
```

`orbit run show` with no run ID is the most recent run on this machine. Use it
only when you know nothing else submitted in between.

A successful run leaves the task in `review`, not `done`. Inspect the
execution summary and the review artifact before you approve the result:

```bash
orbit task show "$TASK_ID" --fields status,execution_summary,artifacts
orbit task artifact get "$TASK_ID" review-gate.json
```

`review-gate.json` is the settled review certificate when the pipeline's
review gate completed. `orbit task show "$TASK_ID" --json` also includes a
`review` object built from that artifact.

Approve the result when you have looked at it. The task moves from `review`
to `done`:

```bash
orbit task update "$TASK_ID" --approve
```

If you would rather authorize the run itself to finish delivery, pass
`--complete` when you ship. That is explained in
[Delivery Workflows](../workflows/#completing-work-with---complete).

## Other shipment shapes

Keep these off the first path. They are the same pipeline with different
delivery or selection.

Deliver in place instead of opening a pull request:

```bash
orbit run ship --mode local "$TASK_ID"
```

Ship several known tasks in one run, and target a specific base branch.
`$SECOND_TASK_ID` is another identifier you captured the same way as
`$TASK_ID` — it is not created by the commands above.

```bash
orbit run ship "$TASK_ID" "$SECOND_TASK_ID" --base main
```

## Next

- [Delivery Workflows](../workflows/) — the whole `orbit run` surface.
- [Run a Task Lifecycle](../../how-to/task-lifecycle/) — the same path in more detail, including attaching artifacts.
- [Run a Continuous Delivery Window](../../how-to/continuous-delivery/) — drain a whole backlog instead of one task.
