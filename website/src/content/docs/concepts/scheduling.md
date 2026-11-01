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
Ask your agent to **schedule a weekly QA sweep** or **publish task snapshots
weekly**. The `orbit-setup` skill installs the clock, then enables the
routine or auto-task you asked for. Most ship disabled; the hourly crew-performance
refresh is enabled but does no scan while crew performance is off.
:::

## The host scheduler clock

`orbit clock tick` is the scheduler: one pass that fires whatever is due,
records it, and exits. The operating system runs it once a minute by default, through a
launchd agent on macOS or a systemd user timer on Linux. There is no resident
daemon. `orbit clock set --cadence-seconds 300` changes the cadence; whole minutes from
1 to 60 are supported. A tick has a five-minute dispatch budget; overlapping
ticks skip while its sweep lock is held. No routine or auto-task fires unless
the clock runs.

The clock belongs to the machine, not the repository. Pausing it stops
scheduled ticks without changing any routine. If the Orbit binary moves, the
clock keeps waking a path that no longer exists; `orbit update` repairs it, and
`orbit clock repair` does so on demand.

## Routine

A routine says **which job fires, and when**. It is one YAML file under
`.orbit/routines/`:

```yaml
schemaVersion: 1
name: ci_failure_sweep_myrepo
enabled: true
trigger:
  cron: "5 * * * *"
  missed_run: skip
target: job:ci_failure_sweep_pipeline
policy:
  timeout_minutes: 30
  overlap: forbid
```

The target is always a catalog job, so a routine can do exactly what a
reviewed job can. Instead of `cron`, a routine can watch the backlog: the
seeded task-pilot routine uses a `state` trigger that prepares new or edited
tasks once they settle, and fires nothing while the backlog is unchanged. If
the branch moves under a task's context files while its pilot runs, that task
is prepared again at the new head without using a retry; the rest of the batch
still applies. A routine can also fire on verified deliveries landing on a
branch (`deliveries_landed`), or when a typed predicate over workspace state
becomes true for a subject (`condition`); see
[Condition triggers](../../reference/scheduling-yaml/#condition-triggers).

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
- **Seeded mostly disabled.** `orbit workspace init` writes task pilot, task
  publication, worktree and store GC, CI and dependency-alert sweeps, the
  daily UX-rules routine, and the finding-verification and pilot-warning
  triage routines, all with `enabled: false`. The one exception is the hourly crew-performance
  refresh, seeded enabled; while crew performance is off, its action skips
  the scan and writes no snapshot.

Routine files and scheduler state are per machine; git does not share them.

## Auto-task

An auto-task says **which recurring chore becomes a task**. It is one YAML
file under `.orbit/auto_tasks/`, managed with `orbit auto-task`: a schedule, an
`enabled` toggle, and the task to file, with its title, body, acceptance
criteria, and crew. The schedule is a cadence (`cron` or `every_minutes`), a
`deliveries_landed` trigger, or a `condition` predicate over workspace state
that fires once per subject; see
[Condition triggers](../../reference/scheduling-yaml/#condition-triggers).
Each clock tick files a task from every enabled definition
that is due. A new chore is a new definition, never new code.

- **A filed task is an ordinary task.** It enters `backlog` by default, or
  `proposed` if you want to approve each one, and carries an
  `auto-task:<name>` tag.
- **No pile-up.** By default a fire is skipped while the previous task is
  still open.
- **Mint now to try one.** `orbit auto-task mint`, or **Mint now** in the
  dashboard, files a task immediately and leaves the schedule untouched.
- **Seeded disabled.** The built-in catalog covers QA sweeps, code and
  security reviews, friction curation, backlog hygiene, doc duties,
  run-failure patterns, and maintenance sweeps for code health, architecture,
  structure, dependency freshness, flaky tests and CI budgets, all off until
  you enable them.

## Scheduled tasks

For a single task, set its optional `not_before` instead of creating a
recurring definition. The task stays in its normal status; a backlog task
waits until that instant, and the hold lifts on the first admission pass
afterwards. It is an earliest start, not an exact-time promise, and does not
approve proposed work or bypass dependencies. Dispatchers enforce this hold
without needing the scheduler clock.

Use `orbit task update <id> --not-before <time>` to set it and
`--clear-not-before` to clear it. The CLI accepts RFC 3339 with an offset or
a local `YYYY-MM-DDTHH:MM`. `orbit task list --scheduled` lists future
schedules, including proposed tasks, soonest first. An operator can start
named tasks early with `orbit run ship <id> --ignore-schedule`, which records
the override in task history. See the [CLI reference](../../reference/cli/#scheduled-tasks)
for the dashboard controls and schedule filters.

## Which to use

| | Routine | Auto-task |
|---|---|---|
| Schedules | A job | A task |
| Good for | Fixed pipelines: publish task snapshots, collect worktrees, sweep CI | Chores an agent should reason about: a weekly audit, a friction pass |
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
