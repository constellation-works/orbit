---
name: orbit-orchestrate
description: Prepares and supervises an Orbit backlog. Deduplicates and pilots tasks (task-pilot), assigns crews, promotes ready work, dispatches with `orbit run ship` and `orbit run auto` drains, diagnoses failed or stuck `jrun-*` runs, and routes CI/QA sweep findings into repair tasks. Use for delivery operations across tasks, jobs, activities and workflow runs. Use orbit for one assigned task and orbit-setup for machine configuration or scheduler installation.
---

# Orbit Orchestrate

Keep authorized work moving using durable task, run and delivery evidence.
A managed implementation worker stays within its assigned task; this skill
is for the operator supervising work.

## Operating loop

Inspect → deduplicate → author → prepare → promote → dispatch → verify → repair.

- Discover the owning workspace and use its returned selector; see
  [tool surface](../orbit/references/tool-surface.md). Never substitute a local
  store for an unavailable owner.
- Follow the user's scope, crew choices and concurrency/build budget. After
  authorized preparation succeeds, promote promptly. An active drain can claim
  backlog work immediately, so finish scope changes before exposing it.
- Creation, promotion and completion are distinct. `--complete` authorizes
  delivery for work admitted in that invocation's window; it does not itself
  authorize promotion, future windows or releases; `run auto --approve-proposed`
  is the separate opt-in for a drain to approve qualifying proposed tasks, and
  `run auto --auto-fix` the separate opt-in to repair its own PRs' failed
  required checks and conflicts, which never merges.
  Existing continuous-delivery authorization may cover them; apply the user's
  actual instructions.
- Verify persisted preparation, actual diffs, required checks and merge state.
  Post-merge review/QA findings become repairs under continuous delivery; do not
  invent an extra pre-merge approval gate. Repository protections still apply.
- Diagnose blocked work before retrying. Preserve user interventions and active
  work. A stopped admission loop does not mean its children were cancelled.
- Record `orchestrator` separately from execution `crew`. Neither attribution
  field grants authority. Leave a durable handoff when a lane cannot proceed.

## Mid-run steering

Use a steering comment to correct the agent's course while the admitted task
scope and acceptance criteria remain right. Do not cancel a run just to add
guidance: steering is delivered at the next prompt that includes task context,
not injected into a step that is already running.

Post an operator comment on the task while the run is in flight:

```bash
orbit task update <task-id> --comment "Please prioritize the existing acceptance criteria for the retry path."
```

This also works for a claimed task. Write to the owner's task record; the owner
admits an operator's comment-only update while the execution claim is active,
and the claimed leaf reads and records delivery through that owner. Other
unscoped task edits remain refused during the claim. Automation and agent
comments are not steering.

The run carries outstanding operator comments, oldest first, in its next safe
task-context prompt. Each comment is delivered at most once per run. Verify
delivery with `orbit task show <task-id>`: find the `steering_delivered` event
for the run and step, then confirm the comment is listed under `delivered`. A
comment under `truncated` was not carried in full. The host maintains the
delivery watermark as internal pipeline state; the event is the operator-facing
receipt. Agent narrative in the execution summary may help diagnose a run, but
is not required to confirm delivery.

When steering is enabled, `task_pr_pipeline` and `task_local_pipeline`, plus
`task_claimed_pr_pipeline` and `task_claimed_local_pipeline`, check for
undelivered comments before `commit`. An outstanding comment sends the
uncommitted candidate through a bounded steering round, up to `max_rounds`.
If the limit is spent, or the last round delivers none of the remaining
comments, the task is held as `steering_unconsumed` and the run stops without
committing. Inspect the task comments, then requeue, reply with clarification,
or accept the candidate through the normal review record.

Configure steering for the workspace:

```toml
[workflow.mid_run_steering]
enabled = true
max_rounds = 2
```

`enabled` defaults to `false`; `max_rounds` defaults to `2` and accepts
`1` through `10`. Settings refresh at prompt and gate checkpoints. Enabling
while runs are already in flight leaves missing watermarks absent, so operator
comments since dispatch are outstanding at the next prompt or gate unless an
inherited candidate or claim watermark provides the baseline; prefer enabling
between drains. Disabling takes effect at the next checkpoint and leaves
existing watermark and round state inert.

Steering cannot change the task's actual specification. A changed title,
description, or acceptance criteria changes the task meaning digest. If the
old-scope candidate must not continue, cancel the live child run if needed and
wait for it and its claim to settle. Then update the task and requeue for fresh
implementation with the old candidate discarded. See
[failed runs](references/orchestration.md#failed-runs) for requeue mechanics.

## Choose the reference

| Need | Read |
|---|---|
| Correct course while a task is running | [Mid-run steering](#mid-run-steering) |
| Prepare/promote work and supervise progress | [Operating loop](references/loop.md) |
| Scope completion authority and dispatch windows | [Authorization](references/authorization.md) |
| Ship/auto command examples, locks and capacity | [Dispatch mechanics](references/orchestration.md) |
| Jobs, activities, run commands and CI sweep | [Workflows](references/workflows.md) |
| Diagnose a specific failed or stuck run | [Run debugging](references/run-debugging.md) |
| Find or answer a human decision request on held work | [Decision requests](references/run-debugging.md#decision-requests-on-held-work) |
| Match an observed failure to a remedy | [Common failures](references/common-failures.md) |
| Route CI findings, recover delivery and verify deployment | [Recovery](references/recovery.md) |
| Salvage an incomplete or stopped candidate instead of starting over | [Salvage an incomplete candidate](references/recovery.md#salvage-an-incomplete-candidate) |
| Work through typical orchestration decisions | [Walkthroughs](references/walkthroughs.md) |

Use [task authoring](../orbit/references/task-authoring.md) when filing work.
Use [orbit-setup](../orbit-setup/SKILL.md) when the cause is host configuration,
installation, scheduler setup or maintenance; do not load those procedures
for ordinary dispatch.
