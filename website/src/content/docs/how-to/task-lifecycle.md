---
title: Run a Task Lifecycle
description: "Create a task, approve it when needed, run it, and inspect the result."
sidebar:
  order: 2
---

## Create

```bash
TASK_ID=$(orbit task add \
  --title "Update docs for policy profiles" \
  --description "Document how fsProfile selection works for activity YAML." \
  --acceptance-criteria "The docs explain explicit and implicit fsProfile resolution." \
  --acceptance-criteria "The docs include a policy YAML example." \
  --complexity medium \
  --workspace .)
```

## Inspect

```bash
orbit task show "$TASK_ID"
orbit task lint "$TASK_ID"
```

## Attach Artifacts

Store generated notes, reports, or other UTF-8 task outputs with the task:

```bash
orbit task artifact put "$TASK_ID" ./summary.md --path reports/summary.md
orbit task show "$TASK_ID" --fields artifacts
```

## Approve

If the task is `proposed`, approve it into the backlog:

```bash
orbit task update "$TASK_ID" --approve --note "Scope reviewed."
```

`--approve` takes the next approval step from the current status, so the same
command later takes the task from `review` to `done`.

## Execute

```bash
orbit run ship "$TASK_ID"
```

Use local mode when PR creation is not desired:

```bash
orbit run ship --mode local "$TASK_ID"
```

## Review

A successful run leaves the task in `review`. Inspect the resulting diff, CI,
task state, and audit events before approving it out:

```bash
orbit task show "$TASK_ID"
orbit run show "$RUN_ID"
orbit audit list
orbit task update "$TASK_ID" --approve
```

To authorize the run itself to finish delivery instead, ship it with
`--complete`. See [Completing work with
`--complete`](../../getting-started/workflows/#completing-work-with---complete).

## Next

- [Use the Dashboard](../dashboard/) — inspect the same tasks and runs in the operator UI.
- [Run a Continuous Delivery Window](../continuous-delivery/) — the same path across a whole backlog.
- [Schedule Recurring Work](../recurring-work/) — let Orbit file and run the work itself.
