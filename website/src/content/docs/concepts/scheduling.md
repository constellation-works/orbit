---
title: Routines and auto-tasks
description: "How Orbit starts unattended work: the host scheduler clock, routines that fire jobs, and auto-tasks that file recurring chores as tasks."
---

Scheduling decides when work starts. Each clock tick checks two kinds of
scheduled work: routines dispatch jobs, and auto-tasks file recurring chores
as ordinary tasks. To set either up, follow
[Schedule Recurring Work](/how-to/recurring-work/).

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
routine or auto-task you asked for.
:::

## The host scheduler clock

`orbit clock tick` is the scheduler: one pass that fires whatever is due,
records it and exits. The operating system runs it once a minute by default,
through a launchd agent on macOS or a systemd user timer on Linux. There is
no resident daemon, and nothing scheduled fires unless the clock runs.

| To | Run |
|---|---|
| Change the cadence (whole minutes, 1 to 60) | `orbit clock set --cadence-seconds 300` |
| Stop scheduled ticks without changing any routine | `orbit clock pause` |
| Fix a clock that points at a moved Orbit binary | `orbit clock repair` (`orbit update` also does this) |

The clock belongs to the machine, not the repository.

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

The target is always a catalog job, so a routine can do only what that job
does. Besides `cron`, a routine can fire when the backlog changes (`state`,
which the seeded task-pilot routine uses), when deliveries land on a branch
(`deliveries_landed`), or when a predicate over workspace state becomes true
(`condition`). See
[Condition triggers](/reference/scheduling-yaml/#condition-triggers).

- **Runs on every owner machine.** A cron routine runs on each machine that
  has the checkout registered and its clock on. `orbit routine pause` keeps
  it off one machine; `enabled: false` in the file retires it. A `state`
  trigger runs only on the machine named in `owner_machine`.
- **Missed slots collapse.** After a sleep, `missed_run: skip` waits for the
  next slot and `catch_up_once` fires one make-up run. `orbit doctor` warns
  when an enabled routine's last success is older than three times its
  interval.
- **No overlap.** A fire is skipped while the previous one is still running,
  for up to `timeout_minutes`.
- **Per machine.** Routine files and scheduler state are not shared through
  git.

`orbit workspace init` seeds these routines:

| Seeded | Routines |
|---|---|
| Enabled | `worktree-gc`, `task-pilot`, `crew-memory-dream` (which mints nothing while `crew_memory.enabled` is false) |
| Disabled | `ci-failure-sweep`, `dependabot-alert-sweep`, `resident-cycle`, `store-gc`, `task-publication` |

A workspace initialized earlier keeps the `enabled` values it already has.

## Auto-task

An auto-task says **which recurring chore becomes a task**. It is one YAML
file under `.orbit/auto_tasks/`, managed with `orbit auto-task`: a schedule,
an `enabled` toggle, and the task to file, with its title, body, acceptance
criteria and crew. The schedule is a cadence (`cron` or `every_minutes`), a
`deliveries_landed` trigger or a `condition` predicate. Each clock tick files
a task from every enabled definition that is due.

- **A filed task is an ordinary task.** It enters `proposed`, so the task
  pilot assesses it before dispatch, unless its template sets another status
  or carries `no-diff-expected`. It carries an `auto-task:<name>` tag.
- **No pile-up.** By default a fire is skipped while the previous task is
  still open.
- **Mint now to try one.** `orbit auto-task mint <name>`, or **Mint now** in
  the dashboard, files a task immediately and leaves the schedule untouched.
- **Seeded mostly disabled.** The built-in catalog covers QA sweeps, code and
  security reviews, friction curation, doc duties, run-failure patterns, and
  sweeps for code health, architecture and dependency freshness. All are off
  until you enable them, including `host-health`.

## Scheduled tasks

To delay a single task, set its `not_before` instead of writing a recurring
definition:

```bash
orbit task update <id> --not-before 2027-01-15T09:00
orbit task update <id> --clear-not-before
orbit task list --scheduled
```

The time is RFC 3339 with an offset, or a local `YYYY-MM-DDTHH:MM`. A backlog
task waits until then and becomes eligible on the next admission pass. It is
an earliest start, not an exact time, and it doesn't approve proposed work or
bypass dependencies. Dispatchers enforce the hold themselves, so it works
without the scheduler clock.

To start a held task early, ship it explicitly: `orbit run ship <id>` or the
dashboard's Ship. The override is recorded in the task's history. See the
[CLI reference](/reference/cli/#scheduled-tasks) for the schedule filters.

## Which to use

| | Routine | Auto-task |
|---|---|---|
| Schedules | A job | A task |
| Good for | Fixed pipelines: publish task snapshots, collect worktrees, sweep CI | Chores an agent should reason about: a weekly audit, a friction pass |
| Output | A run in job history | A task, with approval, review and history |
| Lives in | `.orbit/routines/*.yaml` | `.orbit/auto_tasks/*.yaml` |

## What scheduling never does

Scheduling changes when work starts, not what it may do.

- **It doesn't complete tasks.** Moving a task out of `review` takes
  [`--complete`](/getting-started/workflows/#completing-work-with---complete)
  on a run you start, or the owner's `workflow.distributed_completion = "done"`
  policy for distributed handoffs.
- **It doesn't approve `proposed` tasks,** with one exception you opt into:
  the CI failure sweep routine, once enabled, promotes the repair tasks it
  files after its pilot validates them.
