---
title: Run a Continuous Delivery Window
description: "Prepare tasks, check readiness, authorize a bounded backlog drain, retune or stop it, and recover safely."
sidebar:
  order: 3
---

Use this guide when you want a deliberate, time-bounded period of automatic
delivery. It separates preparation, human approval, delivery, and recovery so an
asynchronous run ID is never mistaken for a completed change.

## 1. Prepare proposed work

Start with the zero-input pilot. It discovers only `proposed` and `backlog`
tasks that have no `context_files`, then prepares bounded pilot groups.

```bash
orbit run job task_pilot_pipeline --wait
```

The pilot applies validated `context_files` selectors, but it does not approve
or dispatch work. Review both the pilot run and the task's applied selectors:

```bash
orbit run history -j task_pilot_pipeline
orbit run show "$PILOT_RUN_ID"
orbit task show "$TASK_ID" --fields status,context_files
```

Use an explicit task list only when you intend to inspect those exact tasks; the
job input is a JSON array, not a space-separated list:

```bash
orbit run job task_pilot_pipeline --input 'task_ids=["TASK-123","TASK-456"]' --wait
```

## 2. Authorize the backlog

After reviewing a proposed task's pilot result, explicitly approve that task:

```bash
orbit task update "$TASK_ID" --approve --note "Pilot context reviewed for this delivery window."
```

That approval moves a `proposed` task to `backlog`. Pilot preparation itself
does not grant approval, and neither does anything you pass to the drain in
step 4. If the task is already in `backlog`, do not approve it again: once its
pilot has applied the context you need, it is ready for the next delivery run.

## 3. Check what will actually start

`orbit run readiness` is a read-only snapshot that explains, task by task, why
the drain would or would not start something. Run it before you commit to a
window:

```bash
orbit run readiness
orbit run readiness --concurrency 8
orbit run readiness "$TASK_ID" --json
```

It reserves nothing, reconciles nothing, and submits nothing, so an eligible
task is not *guaranteed* to start — but an ineligible one tells you why now
instead of after an empty window. Common reasons a backlog task waits:

- **Unsatisfied dependencies.** Approval does not bypass them.
- **An active file lock.** Two tasks whose declared selectors overlap cannot run
  at once. `orbit task locks list` shows what is currently held;
  `orbit task locks contention` shows which files the pending backlog collides
  on, which is what really caps your parallelism.
- **`crew_not_allowed`,** when you are previewing a crew restriction (below).

Dependencies and locks are not bypassed by approval or by the drain. They keep
affected work in the backlog until it is eligible, so a submitted window may
legitimately leave some tasks unfinished.

## 4. Start a bounded window

Choose the duration, the parallelism, and whether this one run may complete
delivery:

```bash
orbit run auto --for 3h
orbit run auto --for 3h --concurrency 8
orbit run auto --for 3h --complete
```

`--for` bounds only the start of new work; a task already in flight when the
window expires still finishes. `--concurrency` (default 5) is parallelism, not
batch size: the drain tops the slots up from the whole backlog as each one
frees.

`--complete` is opt-in authorization for this run to take the work it ships from
`review` to `done`. On a drain it is *blanket* authorization covering every task
the run admits during its whole window, including work that reaches the backlog
after the command starts. It does **not** approve `proposed` tasks into the
backlog — that stayed a human step in step 2 — and it does not substitute for a
review verdict. The full semantics, including how PR and local modes verify a
merge before completing, are in [Completing work with
`--complete`](../../getting-started/workflows/#completing-work-with---complete).

The command returns a durable parent run ID after submission, not a statement
that its tasks have completed. Record it as `$AUTO_RUN_ID` and inspect the
parent and its children:

```bash
orbit run show "$AUTO_RUN_ID"
orbit run trace "$AUTO_RUN_ID"
orbit run show "$CHILD_RUN_ID"
orbit run logs "$CHILD_RUN_ID"
```

`orbit run trace` shows the run tree; use the child IDs it reports when a
particular task needs investigation. Inspect the task directly to distinguish
its submitted run from its actual lifecycle state:

```bash
orbit task show "$TASK_ID" --fields status,job_run_id,comments
```

### Restricting the window to some crews

`--allow-crew` limits one drain to the crews you name. Reach for it when a
provider is down, rate-limited, or out of budget and you want the rest of the
backlog to keep moving:

```bash
orbit run auto --for 4h --allow-crew opus,sonnet
orbit run readiness --allow-crew opus,sonnet    # preview what that would skip
```

It is opt-in — omit it and the drain runs every crew — and it is scoped to that
one run:

- **Validated up front.** Every name must be a crew this workspace configures.
  An unknown or empty one fails the command before anything is dispatched. No
  configuration file is written or changed.
- **Inherited by the whole run.** The leaf and epic pipelines the drain starts
  carry the same restriction, and it is re-checked at each activity against the
  crew that actually resolved — including an activity that uses
  `[workflow] system_crew`. An excluded provider cannot be reached by naming a
  different alias for it. The check compares effective configured identity, so a
  crew resolving to the same provider and model as a permitted one is permitted;
  naming a wrapper is not by itself provider usage. Crew precedence is unchanged
  (explicit, then `task.crew`, then `[workflow] default_crew`) — the allowlist
  gates the winner rather than picking one.
- **Skips, never remaps.** A backlog task whose crew is excluded stays in
  `backlog` on its own crew; the drain simply does not start it, and
  `orbit run readiness --allow-crew ...` reports it as `crew_not_allowed` along
  with the crew it would have run as. **There is no automatic fallback to
  another provider.** To actually move that work, reassign the task's crew
  yourself with `orbit task update <id> --crew <name>`.
- **Only affects what this run starts.** Work another invocation already has in
  flight keeps running; nothing is cancelled.

## 5. Retune a running drain

To change how many tasks a live drain keeps in flight, retune it rather than
cancelling it:

```bash
orbit run show "$AUTO_RUN_ID"                  # current ceiling and who last set it
orbit run concurrency "$AUTO_RUN_ID" --set 7
orbit run concurrency "$AUTO_RUN_ID" --set 3 --reason 'provider rate limited'
```

Cancelling and resubmitting looks equivalent and is not: it mints a new run ID,
restarts the window, and makes you re-state `--complete` and `--allow-crew`.
Retuning preserves the run ID, its deadline, its completion authorization, and
every child it already dispatched.

- **Raising** the ceiling fills the extra slots from the same backlog on the
  next admission pass, usually within a poll interval.
- **Lowering** it stops new admissions until enough children finish. Tasks
  already in flight are never cancelled or shortened.
- The ceiling is bounded by the leaf pipeline's own active-run limit. A run that
  is not a drain, has not started, or has already finished is refused with the
  reason.
- `--if-revision N` applies the change only while the ceiling is still the one
  you read, so two operators cannot silently overwrite each other.
  `orbit run readiness` and `orbit run show` both report the value in force.

## 6. Stop admissions, or cancel work

These are two different actions. Pick deliberately.

**Stop new admissions.** `orbit run auto --stop` ends new admissions for this
workspace's active auto coordinator. You do not need a run ID:

```bash
orbit run auto --stop
orbit run auto --stop --json
orbit run show "$AUTO_RUN_ID"            # Admissions: stopped by ...
```

Children the drain already started **keep running**, under the completion
authority they were admitted with. The coordinator is not cancelled. A second
`--stop`, or `--stop` with no active coordinator, is a no-op, and other
workspaces and jobs are untouched. `--stop` cannot be combined with the flags
that start a drain (`--for`, `--concurrency`, `--complete`, `--allow-crew`).

**Cancel work already in flight.** That is a separate, per-child action:

```bash
orbit run trace "$AUTO_RUN_ID"           # find the child run IDs
orbit run cancel "$CHILD_RUN_ID" --confirm
```

Cancelling the coordinator is the wrong tool: auto children are detached so they
outlive the parent step, and cancelling a parent that *was* blocking would
cascade. So the usual shutdown is `--stop` first, then cancel only the specific
children you actually want to abandon.

## 7. Recover from a failed delivery

First inspect the failed child run and its task. Fix a real code, review, or
dependency problem before creating or authorizing corrective work; do not re-run
blindly.

```bash
orbit run show "$CHILD_RUN_ID"
orbit run logs "$CHILD_RUN_ID"
```

For a task blocked by an attributable failed run, use bounded triage to
re-backlog only an environmental failure:

```bash
orbit run triage "$TASK_ID"
orbit run history -j task_triage_pipeline
```

Triage never changes a task a human blocked by hand, and it leaves a
non-environmental diagnosis blocked for an operator decision. Inspect the triage
run before reopening the delivery window.

If a run is stuck `pending` with no live worker, cancel it to release its task
reservations:

```bash
orbit run cancel "$RUN_ID" --confirm
```

Then check the workspace as a whole, and reclaim the worktrees left behind by
settled tasks:

```bash
orbit doctor
orbit gc worktrees             # reports what it would reap
orbit gc worktrees --confirm   # actually removes them
```

`orbit gc worktrees` only collects worktrees whose task has settled to `done`,
`rejected`, or `archived`, and it reports without removing unless you pass
`--confirm`.

## 8. Keep the task record durable

A delivery window mutates a lot of task state. To snapshot it somewhere you can
restore from, see [Publish and Restore Tasks](../task-publication/).

For an operator working across hosts, [federated MCP
setup](../mcp-integration/#register-the-federated-mux) explains how to select
the owning workspace — without implying automatic failover.
