---
title: Delivery Workflows
description: "The orbit run surface: shipping one task, draining a backlog, sweeping workspaces, triaging failures, and inspecting runs."
sidebar:
  order: 4
---

Everything Orbit executes is a job run. `orbit run` gives the common ones names
so you do not have to remember job IDs.

| Command | What it does |
|---|---|
| [`orbit run ship`](#orbit-run-ship) | Ship selected tasks, or the ready backlog, through the gated pipeline. |
| [`orbit run auto`](#orbit-run-auto) | Drain the backlog for a time window, several tasks at a time. |
| [`orbit run readiness`](#orbit-run-readiness) | Explain why backlog tasks can or cannot start. |
| [`orbit run triage`](#orbit-run-triage) | Re-backlog tasks blocked by environmental run failures. |
| [`orbit run ship-sweep`](#orbit-run-ship-sweep) | Dispatch ship runs across every opted-in workspace. |
| [`orbit run job`](#direct-job-execution) | Run any job definition directly. |

Every one of these is **asynchronous**. The command prints a durable run ID and
returns; it does not know the eventual outcome. Follow up with
[`orbit run show`](#inspecting-runs).

Ship workflows default `--base` to `[workflow] base_branch` from `config.toml`,
or `main` when unset. Pass `--base <branch>` to target a different branch.

## `orbit run ship`

Submit one or more named tasks — or, with no arguments, the ready backlog —
through the gated shipment pipeline.

```bash
orbit run ship
orbit run ship "$TASK_ID"
orbit run ship "$TASK_ID" "$SECOND_TASK_ID" --mode local
orbit run ship "$TASK_ID" --base main
```

`--mode pr` (the default) opens or updates a pull request. `--mode local`
delivers in place. When you omit `--mode`, the mode comes from the workspace's
registry entry, falling back to `pr`.

Underlying job: `task_auto_pipeline`, which fans into `task_gate_pipeline` and
then routes to `task_pr_pipeline` or `task_local_pipeline`.

## `orbit run auto`

Drain the workspace backlog for a window, keeping several tasks in flight at
once:

```bash
orbit run auto                                  # one tick, then stop
orbit run auto --for 4h
orbit run auto --for 4h --concurrency 8
```

The drain re-lists the whole backlog every pass and keeps `--concurrency` tasks
in flight (default 5), starting a replacement as each one finishes rather than
waiting for a batch to drain. An epic root runs alongside the leaves, one at a
time. `--for` bounds only the *start* of new work: a task already being shipped
when the window expires still finishes.

Running a real delivery window — preparing work, choosing concurrency,
restricting crews, retuning, stopping, and recovering — is covered end to end in
[Run a Continuous Delivery Window](../../how-to/continuous-delivery/).

## `orbit run readiness`

A read-only snapshot explaining why backlog tasks are or are not eligible right
now. It reserves nothing, submits nothing, and mutates nothing:

```bash
orbit run readiness
orbit run readiness "$TASK_ID" "$SECOND_TASK_ID"
orbit run readiness --concurrency 8 --json
```

## `orbit run triage`

Scan tasks that a failed job run left `blocked`, and re-backlog the ones whose
failure was environmental:

```bash
orbit run triage
orbit run triage "$TASK_ID"
```

Tasks a human blocked by hand are never touched, and a non-environmental
diagnosis stays blocked for an operator decision. An empty candidate set is a
clean no-op.

## `orbit run ship-sweep`

Dispatch a ship run in every registered workspace that has ready backlog tasks.
Only workspaces with `[workflow] auto_ship = true` are swept; everything else is
reported as skipped. This is the unattended entry point, intended for a
scheduler:

```bash
orbit run ship-sweep --dry-run
orbit run ship-sweep --json
```

## Completing work with `--complete`

By default a successful task ends in `review`, and a separate operator action
takes it to `done`. `--complete` is your explicit authorization, granted on one
invocation, for that run to finish delivery itself:

```bash
orbit run ship "$TASK_ID" --complete
orbit run auto --for 4h --complete
```

It is off unless you pass it. No workspace setting, environment variable, or
unattended routine — including `orbit run ship-sweep` — turns it on.

What the run then does depends on the mode:

- **`--mode local`** — the task reaches `done` only after the bundle has
  committed, merged, and pushed. A failed merge or push fails the run with the
  task still in `review`.
- **`--mode pr`** — the run opens or reuses the PR as usual, then merges it
  through GitHub. Branch protections and required checks are respected; Orbit
  never uses an administrative bypass. If required checks are still running it
  enables GitHub auto-merge and keeps waiting — enabling auto-merge is not
  success on its own. The task moves to `done` only after the PR is verified
  merged. A closed or blocked PR, a refused auto-merge, or an expired wait
  budget fails the run and leaves the task in `review`.
- **Work that produced no diff** — validated `no-diff-expected` work completes
  without needing a PR.

Two limits are worth knowing:

- **`orbit run auto --complete` is blanket authorization.** It covers every task
  the drain admits for its whole window, including work that reaches the backlog
  *after* the run starts — not only what is visible when you submit. Work the
  drain never admits does not inherit it, and neither does any other run.
- **It authorizes completion only.** `--complete` grants the `review → done`
  transition and the delivery that precedes it. It never approves `proposed`
  work into the backlog — that is a separate human step,
  `orbit task update <id> --approve` — and it does not stand in for an
  independent review verdict. The transition is recorded in the task's history
  against the authorizing run and operator.

## Direct job execution

For jobs without a workflow alias, invoke them by ID:

```bash
orbit job list
orbit run job task_auto_pipeline
orbit run job task_auto_pipeline --input mode=local
orbit run job task_auto_pipeline --wait
```

`--wait` blocks on the submitted run instead of returning immediately, and exits
nonzero unless the run succeeded.

## Inspecting runs

Every run is durable and inspectable:

```bash
orbit run history                        # recent runs
orbit run history -j task_auto_pipeline  # one job's runs
orbit run show "$RUN_ID"                 # state and step summary
orbit run logs "$RUN_ID"                 # raw stdout/stderr
orbit run events "$RUN_ID"               # audit events
orbit run trace "$RUN_ID"                # parent/child run tree
```

`orbit run show` with no run ID shows the most recently scheduled run. Add
`-s <step_id>` to any of these to narrow to a single step.

To stop a run that has not reached a terminal state:

```bash
orbit run cancel "$RUN_ID" --confirm
```

Cancellation signals the owner process (TERM, then KILL), releases the run's
task reservations, and finalizes the run as `cancelled`. It is also the
remediation for a stuck `pending` run with no live worker. A run that already
finished returns `already_terminal` without replacing its outcome.
