---
title: Default Workflows
description: "Built-in workflows under orbit run."
sidebar:
  order: 4
---

Orbit ships a default workflow under `orbit run`. It wraps a seeded job pipeline under `crates/orbit-core/assets/jobs/`; the same pipeline is runnable directly via `orbit run job <name>`.

The workflow defaults `--base` to `[workflow].base_branch` from
`config.toml`, or `main` when it is unset. Pass `--base <branch>` to target a
different branch.

## `orbit run ship`

Submit backlog tasks or one or more named tasks through the gated shipment pipeline. The default mode opens PRs; `--mode local` uses the local-only delivery path. The command returns a run ID immediately, while dependency and lock waits happen inside the job.

```bash
orbit run ship
orbit run ship "$TASK_ID"
orbit run ship "$TASK_ID" "$SECOND_TASK_ID" --mode local
orbit run ship "$TASK_ID" --base main
```

Underlying job: `task_auto_pipeline`, which fans into `task_gate_pipeline` and then routes to `task_pr_pipeline` or `task_local_pipeline` from `--mode`.

## Restricting a drain to some crews

`orbit run auto --allow-crew` limits one drain to the crews you name. Reach for it
when a provider is down, rate-limited, or out of budget and you want the rest of
the backlog to keep moving:

```bash
orbit run auto --for 4h --allow-crew opus,sonnet
orbit run readiness --allow-crew opus,sonnet    # preview what that would skip
```

It is opt-in — omit it and the drain runs every crew, exactly as before — and it
is scoped to that one run:

- **Validated up front.** Every name must be a crew this workspace configures.
  An unknown or empty one fails the command before anything is dispatched. No
  configuration file is written or changed.
- **Inherited by the whole run.** The leaf and epic pipelines the drain starts
  carry the same restriction, and it is re-checked at each activity against the
  crew that actually resolved — including an activity that uses
  `[workflow].system_crew`. An excluded provider cannot be reached by naming a
  different alias for it. The check compares effective configured identity, so a
  crew resolving to the same provider and model as a permitted one is permitted;
  naming a wrapper is not by itself provider usage. Crew precedence is unchanged
  (explicit, then `task.crew`, then `[workflow].default_crew`) — the allowlist
  gates the winner rather than picking one.
- **Skips, never remaps.** A backlog task whose crew is excluded stays in
  `backlog` on its own crew; the drain simply does not start it, and
  `orbit run readiness --allow-crew ...` reports it as `crew_not_allowed` along
  with the crew it would have run as. To actually move that work, reassign the
  task's crew yourself. There is no automatic fallback to another provider.
- **Only affects what this run starts.** Work another invocation already has in
  flight keeps running; nothing is cancelled. Permitted tasks keep filling the
  free slots at the usual rate.

## Changing a running drain's worker count

`orbit run auto --concurrency N` sets how many tasks a drain keeps in flight. To
change that number on a drain that is already running, retune it rather than
cancelling it:

```bash
orbit run show "$RUN_ID"                       # current ceiling and who last set it
orbit run concurrency "$RUN_ID" --set 7
orbit run concurrency "$RUN_ID" --set 3 --reason 'provider rate limited'
```

Cancelling and resubmitting looks equivalent and is not: it mints a new run ID,
restarts the window, and makes you re-state `--complete` and `--allow-crew`.
Retuning keeps all of them, and keeps the children the drain already started.

- **Raising** it fills the extra slots from the same backlog on the next
  admission pass, usually within a poll interval.
- **Lowering** it stops new admissions until enough children finish. Tasks
  already in flight are never cancelled or shortened.
- The ceiling is bounded by the leaf pipeline's own active-run limit, and a run
  that is not a drain, has not started, or has already finished is refused with
  the reason.
- `--if-revision N` applies the change only while the ceiling is still the one
  you read, so two operators cannot silently overwrite each other.
  `orbit run readiness` and `orbit run show` both report the value in force.

## Stopping a running drain

`orbit run auto --stop` ends **new admissions** for this workspace's active auto
coordinator. You do not look up a run ID. Children the drain already started
keep running under the completion authority they were admitted with, and the
coordinator is not cancelled.

```bash
orbit run auto --stop
orbit run auto --stop --json
orbit run show "$RUN_ID"                 # Admissions: stopped by ...
```

A second `--stop`, or `--stop` with no active coordinator, is a no-op. Other
workspaces and other jobs are untouched. `--stop` cannot be combined with the
flags that start a drain (`--for`, `--concurrency`, `--complete`, `--allow-crew`).

This is not cancellation. To stop workers that are already running, cancel each
child explicitly:

```bash
orbit run cancel "$CHILD_RUN_ID" --confirm
```

Parent cancellation of the coordinator is the wrong tool for this job: auto
children are detached so they outlive the parent step, and cancelling a parent
that *was* blocking would cascade.

## Completing work with `--complete`

By default a successful task ends in `review`, and a separate operator action
takes it to `done`. `--complete` is your explicit authorization, granted on one
invocation, for that run to finish delivery itself:

```bash
orbit run ship "$TASK_ID" --complete
orbit run auto --for 4h --complete
```

It is off unless you pass it. No workspace setting, environment variable, or
unattended routine (including `orbit run ship-sweep`) turns it on.

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
- **`no-diff-expected` work** — validated work that produced no diff completes
  without needing a PR.

Two limits are worth knowing:

- `orbit run auto --complete` is *blanket* authorization. It covers every task
  the drain admits for its whole window, including work that reaches the backlog
  after the run starts — not only what is visible when you submit.
- `--complete` authorizes delivery completion and the `review -> done`
  transition only. It never approves `proposed` work into the backlog, and it
  does not stand in for an independent review verdict; the transition is
  recorded against the authorizing run and operator in the task's history.

Submission stays asynchronous either way: the command prints the durable run ID
and returns without knowing the eventual outcome. Follow it with
`orbit run show <RUN_ID>`.

For an operator workflow that prepares pilot context, explicitly authorizes
backlog work, runs a bounded drain, and handles recovery, see [Run a Continuous
Delivery Window](../how-to/continuous-delivery/).

## Direct Job Execution

For schemaVersion 2 jobs without a workflow alias, invoke them directly:

```bash
orbit job list
orbit run job task_auto_pipeline
orbit run job task_auto_pipeline --input mode=local
orbit run job task_auto_pipeline --wait
```

A job run is submitted to a detached worker: the command prints the run ID and
returns as soon as the run is durable, without claiming its eventual outcome.
Pass `--wait` to block on the submitted run instead — it exits nonzero unless
the run succeeded.

## Inspecting Runs

Every workflow run is durable. Inspect with:

```bash
orbit run history -j task_auto_pipeline
orbit run show <RUN_ID>
orbit run logs <RUN_ID>
```
