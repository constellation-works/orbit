---
title: Routines and Auto-Tasks
description: "How Orbit schedules unattended work: the sweep clock, routines that fire jobs, and auto-tasks that mint recurring chores as tasks."
sidebar:
  order: 4
---

Tasks describe work and jobs execute it, but neither says *when*. That is the
scheduling layer's job. Every scheduled fire, whether it ships a backlog or
mints a weekly chore, follows the same path:

<ol class="orbit-pipeline" aria-label="How a scheduled fire flows through Orbit">
  <li>
    <span class="orbit-pipeline-step">Sweep clock</span>
    <span class="orbit-pipeline-note">The OS wakes <code>orbit sweep</code></span>
  </li>
  <li>
    <span class="orbit-pipeline-step">Routine</span>
    <span class="orbit-pipeline-note">Due on this host</span>
  </li>
  <li>
    <span class="orbit-pipeline-step">Job run</span>
    <span class="orbit-pipeline-note">Ordinary run, ordinary history</span>
  </li>
  <li>
    <span class="orbit-pipeline-step">Auto-task</span>
    <span class="orbit-pipeline-note">Due templates are minted</span>
  </li>
  <li>
    <span class="orbit-pipeline-step">Task</span>
    <span class="orbit-pipeline-note">Normal lifecycle</span>
  </li>
</ol>

## The sweep clock

`orbit sweep` is the scheduler pass. It is stateless in and durable out: it
loads every routine definition from the registered workspaces that opt in,
filters them for the current host, fires whatever is due, records what it did,
and exits. Nothing in Orbit is scheduled unless that pass runs.

The pass is invoked by the operating system, not by a resident daemon. A
per-user launchd agent (macOS) or systemd user timer (Linux) calls it once a
minute by default. That is a deliberate trade: minute granularity is a floor
and event triggers are not possible, but there is no long-lived process to
supervise, and a wedged pass costs one minute, not the scheduler.

The clock is host infrastructure, configured per machine and never versioned.
Pausing it stops scheduled sweeps; a manual `orbit sweep` still works, and no
individual routine's state changes.

## Routine

A routine is a **versioned trigger**. It is one YAML file under
`.orbit/routines/`, committed to the repository, that says which job fires and on
what cadence:

```yaml
schemaVersion: 1
name: ship_sweep_myrepo
enabled: true
trigger:
  cron: "*/20 * * * *"
  missed_run: skip
target: job:workspace_ship_pipeline
policy:
  timeout_minutes: 120
  overlap: forbid
```

The definition is the whole contract. There is no routine registry, no
in-memory schedule, and no inline command payload: a target is always a catalog
reference (`job:<name>`), so what a routine can do is exactly what a reviewed
job can do. To fire a single activity on a schedule, wrap it in a one-step job.

Invariants that shape how routines behave:

- **No host field.** A definition is evaluated by every machine with a
  registered owner checkout and an enabled clock, each against its own store.
  Registration is the opt-in; to keep a routine on one machine, put it under
  `.orbit/routines/local/` there or pause it elsewhere.
- **Versioned enable, host-local pause.** `enabled` lives in the file and is a
  reviewed change; `orbit routine pause` is a per-host override that is never
  synced and survives reboots. Use pause for "not on this machine right now",
  and `enabled: false` to retire a routine everywhere.
- **Missed slots collapse.** When a host was asleep through several due slots,
  `missed_run: skip` waits for the next natural slot and `catch_up_once` fires a
  single make-up run. Neither replays every missed tick.
- **Overlap is forbidden by default.** A due fire is skipped while the previous
  one is in flight; `timeout_minutes` is also the staleness horizon after which
  a stuck fire stops blocking the next.
- **Seeded disabled.** `orbit init` writes a default set — task pilot, ship
  sweep, auto-task scheduler, triage, worktree GC, CI and dependency alert
  sweeps — every one `enabled: false`. Enabling unattended agent work is an
  explicit, versioned decision.

All scheduler state — last fire, cursor, pause, run history — is host-local.
Two hosts sharing a repository share the definitions and nothing else.

## Auto-task

An auto-task is a **task template with a schedule**. Where a routine answers
"which job fires, and when", an auto-task answers "which recurring chore should
become a task". It is one YAML file under `.orbit/auto_tasks/`, managed through
`orbit auto-task`, carrying a cadence, an `enabled` toggle, a dedupe policy, and
the task it should produce: title, body, acceptance criteria, type, priority,
tags, crew, required tools.

The mechanism that turns definitions into tasks is deliberately generic. One
seeded routine, `auto_task_scheduler`, fires the `auto_task_scheduler_pipeline`
job every minute; that job reads every enabled definition and mints a task from
each one that is due. Because each definition carries its own schedule, the
minutely routine is cheap and idempotent, and adding a recurring chore is a new
definition — never new code and never a new routine.

Invariants that shape how auto-tasks behave:

- **A minted task is an ordinary task.** It enters at the status the definition
  declares — `backlog` by default, or `proposed` when each instance should be
  approved by a human — and then follows the normal lifecycle. It carries an
  `auto-task:<name>` provenance tag.
- **Dedupe by default.** `skip-if-open` skips a fire while a previous instance
  is still open, which is what keeps a stalled backlog from accumulating twenty
  identical chores. `always` opts out per definition.
- **Manual mint is unconditional and cursor-inert.** `orbit auto-task mint`
  ignores the schedule, the dedupe policy, and `enabled`, and never moves the
  scheduler's cursor — so trying a definition never shifts its next fire.
- **Catch-up collapses.** A downtime gap mints one make-up task, not one per
  missed slot.
- **Seeded disabled.** Orbit embeds a small default catalog (QA sweep, friction
  curation, security review, code review), materialized on init with
  `enabled: false`. Seeding never mints a task.

## Why both exist

Routines and auto-tasks sit at different heights of the same stack, and it is
tempting to collapse them. They stay separate because they schedule different
kinds of thing.

| | Routine | Auto-task |
|---|---|---|
| Schedules | A job | A task |
| Question | Which job, when, on which host? | Which chore becomes a task? |
| Fires through | `orbit sweep` directly | The generic scheduler routine |
| Lives in | `.orbit/routines/*.yaml` | `.orbit/auto_tasks/*.yaml` |
| Adding one means | A new versioned trigger | A new definition, no new trigger |
| Output | A run in job history | A task in the backlog |

A routine is the right tool when the work is a fixed pipeline — ship what is
ready, collect worktrees, sweep CI failures — that should just run. An auto-task
is the right tool when the work is a chore an agent should *reason about* — a
weekly dependency audit, a daily friction pass — and whose outcome a human may
want to review as a task. The auto-task path also gets task semantics for free:
approval gates, dedupe against open instances, review, and audit history.

## What unattended never gets

Scheduling changes *when* work starts, not *what it is allowed to do*. Two
authorities stay with humans regardless of how a task was created or fired:

- **Entry into the backlog** for a `proposed` task is always a human decision.
  An auto-task can mint into `proposed`, but nothing scheduled approves it.
- **Completion out of `review`** can only be granted with `--complete` on an
  explicit invocation. No workspace setting, environment variable, or routine
  turns it on; unattended shipment always leaves work in `review`.

See [Tasks](../tasks/#approval) for the two gates, and
[Schedule Recurring Work](../../how-to/recurring-work/) for installing the
clock, enabling routines, and writing definitions.
