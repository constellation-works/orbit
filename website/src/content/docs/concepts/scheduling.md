---
title: Routines and auto-tasks
description: "How Orbit schedules unattended work: the host scheduler clock, routines that fire jobs, and auto-tasks that mint recurring chores as tasks."
sidebar:
  order: 4
---

Tasks describe work and jobs run it; scheduling decides *when*. Each clock
tick checks two independent kinds of scheduled work: routines dispatch jobs,
and auto-tasks file recurring chores as ordinary tasks.

<ol class="orbit-pipeline" aria-label="What a scheduler tick checks">
  <li>
    <span class="orbit-pipeline-step">Scheduler clock</span>
    <span class="orbit-pipeline-note">The OS wakes <code>orbit clock tick</code></span>
  </li>
  <li>
    <span class="orbit-pipeline-step">Routine</span>
    <span class="orbit-pipeline-note">Dispatches a due job run</span>
  </li>
  <li>
    <span class="orbit-pipeline-step">Auto-task</span>
    <span class="orbit-pipeline-note">Mints a due task into the normal lifecycle</span>
  </li>
</ol>

:::tip[Let your agent set it up]
Ask your agent to **schedule a weekly QA sweep** or **ship the backlog every
30 minutes**. The `orbit-setup` skill installs the clock, then enables the
routine or auto-task you asked for. Everything ships disabled until then.
:::

## The host scheduler clock

`orbit clock tick` is the scheduler: one pass that fires whatever is due,
records it, and exits. The operating system runs it once a minute by default, through a
launchd agent on macOS or a systemd user timer on Linux. There is no resident
daemon. `orbit clock set --cadence-seconds 300` changes the cadence; whole minutes from
1 to 60 are supported. A tick has a five-minute dispatch budget; overlapping
ticks skip while its sweep lock is held. Nothing is scheduled unless the clock
runs.

The clock belongs to the machine, not the repository. Pausing it stops
scheduled ticks without changing any routine. If the Orbit binary moves, the
clock keeps waking a path that no longer exists; `orbit update` repairs it, and
`orbit clock repair` does so on demand.

## Routine

A routine says **which job fires, and when**. It is one YAML file under
`.orbit/routines/`:

```yaml
schemaVersion: 1
name: ship_sweep_myrepo
enabled: true
trigger:
  cron: "*/30 * * * *"
  missed_run: skip
target: job:workspace_ship_pipeline
policy:
  timeout_minutes: 120
  overlap: forbid
```

The target is always a catalog job, so a routine can do exactly what a
reviewed job can. Instead of `cron`, a routine can watch the backlog: the
seeded task-pilot routine uses a `state` trigger that prepares new or edited
tasks once they settle, and fires nothing while the backlog is unchanged. If
the branch moves under a task's context files while its pilot runs, that task
is prepared again at the new head without using a retry; the rest of the batch
still applies.

- **Runs on every owner machine.** A cron routine runs on each machine that
  has the checkout registered and its clock on. To keep it off one machine,
  `orbit routine pause` it there; `enabled: false` in the file retires it. A
  `state` trigger runs only on the machine it names in `owner_machine`.
- **Missed slots collapse.** After a sleep, `missed_run: skip` records each
  missed slot with a reason and waits for the next one. `catch_up_once` fires
  one make-up run. The seeded worktree-gc routine uses `catch_up_once`.
  `orbit doctor` warns when an enabled routine's last success is older than
  three times its schedule interval.
- **No overlap.** A fire is skipped while the previous one is still running,
  for up to `timeout_minutes`.
- **Seeded disabled.** `orbit workspace init` writes task pilot, ship sweep,
  worktree GC, and CI and dependency-alert sweeps, all with `enabled: false`.

Routine files and scheduler state are per machine; git does not share them.

## Auto-task

An auto-task says **which recurring chore becomes a task**. It is one YAML
file under `.orbit/auto_tasks/`, managed with `orbit auto-task`: a cadence, an
`enabled` toggle, and the task to file, with its title, body, acceptance
criteria, and crew. Each clock tick files a task from every enabled definition
that is due. A new chore is a new definition, never new code.

- **A filed task is an ordinary task.** It enters `backlog` by default, or
  `proposed` if you want to approve each one, and carries an
  `auto-task:<name>` tag.
- **No pile-up.** By default a fire is skipped while the previous task is
  still open.
- **Mint now to try one.** `orbit auto-task mint`, or **Mint now** in the
  dashboard, files a task immediately and leaves the schedule untouched.
- **Seeded disabled.** The built-in catalog covers QA sweeps, code and
  security reviews, friction curation, backlog hygiene, doc duties, and
  run-failure patterns, all off until you enable them.

## Which to use

| | Routine | Auto-task |
|---|---|---|
| Schedules | A job | A task |
| Good for | Fixed pipelines: ship what's ready, collect worktrees, sweep CI | Chores an agent should reason about: a weekly audit, a friction pass |
| Output | A run in job history | A task in the backlog, with approval, review, and history |
| Lives in | `.orbit/routines/*.yaml` | `.orbit/auto_tasks/*.yaml` |

## What scheduling never does

Scheduling changes when work starts, not what it may do. Nothing scheduled
completes a task out of `review` by default: that takes
[`--complete`](../../getting-started/workflows/#completing-work-with---complete)
on a run you start yourself, or the owner's explicit
`workflow.distributed_completion = "done"` policy for distributed handoffs.
Nothing scheduled approves a `proposed` task into
the backlog either, with one exception you opt into: once you enable the CI
failure sweep routine, it promotes the repair tasks it files after its pilot
validates them.

[Schedule Recurring Work](../../how-to/recurring-work/) covers installing the
clock, enabling routines, and writing definitions.
