---
title: Schedule Recurring Work
description: "Run Orbit unattended: the sweep clock, routines that fire jobs on a schedule, and auto-task definitions that mint tasks."
sidebar:
  order: 4
---

Orbit has two layers of scheduling, and they answer different questions.

| Layer | Question it answers | Where it lives |
|---|---|---|
| **Routines** | *Which job should fire, on what cadence?* | Versioned YAML in `.orbit/routines/` |
| **Auto-tasks** | *Which recurring chore should become a task?* | Versioned YAML in `.orbit/auto_tasks/`, managed by `orbit auto-task` |

Both are driven by the same clock: `orbit clock tick`. Nothing is scheduled until
that clock runs. This guide is the operating procedure; the model behind it —
why the layers are separate and what each one guarantees — is in
[Routines and Auto-Tasks](../../concepts/scheduling/).

## 1. Start the host clock

`orbit clock tick` is the scheduler pass. It loads routine and auto-task
definitions from every registered, active owner checkout. Due routines dispatch
normal job runs; due auto-tasks mint normal tasks in-process.

Point the OS at it once per host:

```bash
orbit routine init --install-clock
```

That installs a per-user clock unit — launchd on macOS, a systemd user timer on
Linux — which invokes `orbit clock tick` every minute. `orbit routine init` without
the flag just reports this host's identity; it never creates or rewrites host
identity, which is `orbit init`'s job.

Inspect and control the clock:

```bash
orbit clock status
orbit clock set --cadence-seconds 300   # whole-minute cadence, in seconds
orbit clock pause
orbit clock enable
orbit clock repair                      # repoint the unit at this binary
```

Pausing the clock stops scheduled ticks. A manual `orbit clock tick` still works,
and it does not change any individual routine's pause state.

The installed unit names the orbit binary by absolute path, so installing orbit
somewhere else — or removing the install the unit names — stops unattended
sweeps while `orbit clock status` still reports the clock as enabled.
`orbit update` repairs the unit itself as its last step; `orbit clock repair` is
the same repair for a binary another package manager installed. Repair rewrites
and re-registers the unit; it never resumes a clock you paused.

You can always run the pass by hand, which is the right way to try a change:

```bash
orbit clock tick --dry-run       # report what would fire; write nothing
orbit clock tick --verbose       # every routine and auto-task row
orbit clock tick --json
orbit --workspace <name> clock tick  # only that workspace's schedules
```

A pass visits every registered owner checkout on the host. The global
`--workspace` selector narrows it to one — in both dry-run and live passes,
nothing outside the selected workspace is evaluated, fired, or recorded. An
unregistered selector fails instead of falling back to the whole host.

By default the tick prints only noteworthy rows — fires, mints, retries,
baselines, and errors — so a per-minute clock does not fill the log with
`not_due` churn. `orbit sweep` remains a compatibility alias with identical
arguments and output.

## 2. Enable the routines you want

Registering the checkout is the whole opt-in — there is no config key to set,
and every registered owner checkout's definitions are evaluated by this host's
clock.

`orbit workspace init` seeds a set of default routines into `.orbit/routines/`,
each **disabled**, because enabling unattended agent work is a deliberate,
versioned decision. `orbit workspace sync` refreshes that shipped set on a newer
binary while preserving local edits. The shipped set covers task pilot preflight, ship sweeps, worktree GC, and
CI/dependency alert sweeps.

```bash
orbit routine list               # toggles, next-due, last fire
orbit routine show "$ROUTINE_NAME"
```

To enable one, edit its YAML in `.orbit/routines/` and set `enabled: true`. That
is a tracked change, reviewed like any other.

### Routine shape

```yaml
schemaVersion: 1
name: ship_sweep_myrepo
description: Ship this workspace's ready backlog through the gated pipeline.
enabled: true
trigger:
  cron: "*/20 * * * *"  # 5-field cron, evaluated in host-local time
  missed_run: skip      # or catch_up_once
target: job:workspace_ship_pipeline
policy:
  timeout_minutes: 120
  overlap: forbid       # or allow
  retries:
    max: 0
    backoff_minutes: 15
```

Notes that matter in practice:

- **There is no host field.** Every machine with a registered owner checkout and
  an enabled clock evaluates the definition against its own store. To keep a
  routine off a machine, pause it there.
- **`missed_run`** decides what happens to slots that fell in a gap while the
  host was asleep. `skip` (the default) waits for the next natural slot;
  `catch_up_once` fires a single make-up run no matter how many slots were
  missed.
- **`overlap: forbid`** (the default) skips a due fire while a previous one is
  still in flight. `timeout_minutes` is also the staleness horizon: after it, a
  stuck fire stops blocking the next one and is recorded as timed out.
- **`target`** is a job (`job:<name>`). To fire a single activity on a schedule,
  wrap it in a one-step job.

### Pausing on one host

Pauses are host-local, never synced, and survive reboots. Use them for
"not on this machine right now," not to retire a routine:

```bash
orbit routine pause "$ROUTINE_NAME"
orbit routine resume "$ROUTINE_NAME"
```

To retire a routine everywhere, set `enabled: false` in its versioned
definition instead.

## 3. Define recurring chores as auto-tasks

An auto-task is a **template for a task**, minted on a schedule. Adding a
recurring chore is a new definition — never new code and never a new routine.

The host tick does the minting directly. It reads each enabled definition and
mints from the due ones without creating a scheduler job run. Enable the host
clock and the definition; there is no scheduler routine to enable.

### Create a definition

```bash
orbit auto-task add \
  --name weekly-dep-audit \
  --description "Weekly check for outdated dependencies." \
  --cron "0 9 * * 1" \
  --title "Audit outdated dependencies" \
  --body "Check the lockfile for outdated or vulnerable dependencies and open follow-up work." \
  --criterion "Every outdated direct dependency is listed with its current and latest version." \
  --criterion "Anything with a known advisory has a filed follow-up task." \
  --type chore \
  --priority medium \
  --tag maintenance
```

Schedule it with exactly one trigger: `--cron` (5-field), `--every-minutes`,
or `--deliveries-landed '<JSON>'`. The delivery trigger fires once verified
deliveries land on a branch (`branch`, `threshold`, `max_wait_minutes`,
`coverage`); preview one with `orbit auto-task show <name> --preview` before
enabling it.

Useful options:

| Option | Effect |
|---|---|
| `--status` | Status each minted task enters. Defaults to `backlog`; use `proposed` when you want a human to approve each instance. |
| `--dedupe` | `skip-if-open` (default) skips the fire while a previous instance is still open. `always` fires regardless. |
| `--crew` | Crew override for minted tasks. |
| `--priority` | `low`, `medium`, `high`, or `critical`. |
| `--tag` | Applied to each minted task, in addition to the provenance tag. Repeatable. |
| `--required-tools` | Exact canonical tool names copied to every minted task. |

`--dedupe skip-if-open` is the setting that keeps a stalled backlog from
accumulating twenty identical chores. Leave it alone unless you genuinely want
overlapping instances.

### Inspect, update, and disable

```bash
orbit auto-task list
orbit auto-task list --enabled
orbit auto-task show weekly-dep-audit
orbit auto-task update weekly-dep-audit --cron "0 9 * * 2"
orbit auto-task toggle weekly-dep-audit off
```

`orbit auto-task update` changes only the fields you pass. `toggle` is the
kill-switch, not a delete — the definition and its history are preserved, so you
can turn it back `on` later.

### Delete a definition

When a chore no longer applies, delete it rather than leaving it disabled:

```bash
orbit auto-task delete weekly-dep-audit --reason "moved to Renovate"
```

Delete removes the definition file and its scheduler cursor, and writes an
audit record with the optional reason. It refuses while a task minted from the
definition is still open, and names those tasks; `--force` deletes anyway and
leaves the open tasks alone. For a `--deliveries-landed` definition it also
drops the coverage ledger through the audited `reset`, together with the
automation refs the ledger pinned, so it refuses whenever that reset would.

Deleting one of the definitions `orbit workspace init` ships — for example
`code-review` or `qa-sweep` in a repository that holds no code — also records an
opt-out. Later `orbit workspace init --force` and `orbit workspace sync` runs
leave it absent, and `orbit doctor` does not report it missing. To bring one
back with its shipped content:

```bash
orbit auto-task restore code-review
```

The restored definition is disabled, as shipped, and reseeds manage it again.

### Recover a delivery-triggered definition

A definition scheduled with `--deliveries-landed` keeps a coverage ledger, so it
has extra controls. `recover` and `reset` preview without `--reason` and apply
only with it:

```bash
orbit auto-task show delivery-qa --preview                                 # baseline and observations; admits nothing
orbit auto-task recover delivery-qa --adopt-settings --reason "cadence changed"  # resume after a settings change; keeps coverage debt
orbit auto-task reset delivery-qa --reason "re-baseline"                   # forget coverage debt; re-baseline at the branch head
orbit auto-task update delivery-qa --waive-batch "$BATCH_ID" --waiver-reason "flaky infra"  # waive one settled failed batch
```

`recover` also takes `--reissue-action` and `--replay-history`. `reset --force`
abandons an executing action rather than cancelling it.

### Mint one now

To test a definition, or to run a chore off-schedule, mint it directly:

```bash
orbit auto-task mint weekly-dep-audit
```

This ignores the schedule, the dedupe policy, and even `enabled`, and it leaves
the scheduler's cursor untouched — so a manual mint never shifts the next
scheduled fire. It is the fastest way to see exactly what a definition produces
before you trust it unattended.

## 4. Watch it work

A minted task is an ordinary task, and a routine fire is an ordinary run:

```bash
orbit auto-task list                                   # name, enabled state, schedule
orbit routine list                                     # toggles, next-due, last fire
orbit clock tick --dry-run                             # scheduler decisions
orbit task list --tag maintenance --status backlog     # what got minted
```

From there the work is the normal path. Approve anything minted as `proposed`,
then let a delivery window pick it up:

```bash
orbit task update "$TASK_ID" --approve
orbit run auto --for 4h
```

See [Run a Delivery Window](../continuous-delivery/) for the drain
itself.

## Unattended shipping

If you want scheduled *shipment* rather than scheduled task creation, that is
the ship sweep. Opt the workspace in and enable the seeded routine:

```bash
orbit config set workflow.auto_ship true
```

`orbit run ship-sweep` dispatches ship runs in every registered workspace with
ready backlog tasks, skipping any workspace that has not set `auto_ship`. It
never grants `--complete`: unattended shipment still leaves work in `review` for
a human.

## Health

```bash
orbit clock status
orbit clock tick --dry-run --verbose
orbit doctor
```

If routines are not firing, check in that order: the clock is enabled, this
checkout is registered as an owner (`orbit workspace list`), the definition has
`enabled: true`, and it is not paused on this host.
