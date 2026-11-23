# Automation: the scheduler

## Contents

- [The chain](#the-chain)
- [Turning the scheduler on](#turning-the-scheduler-on)
- [Recommended enablement order](#recommended-enablement-order)
- [Verify without firing](#verify-without-firing)
- [Observe and control](#observe-and-control)
- ["Why didn't it fire?"](#why-didnt-it-fire)

How scheduled work happens, and what to turn on. A fresh workspace carries the
defaults selected by its [seeding policy](configuration-layers.md#seeding-policy).
Scheduled work requires a host clock and enabled definitions.
This is the reference for opting in deliberately.

## The chain

```text
OS clock unit (every minute)
  └── orbit clock tick                  stateless: what is due on this host?
        ├── routine (.orbit/routines/*.yaml) → job:<name> → normal run
        └── auto-task (.orbit/auto_tasks/*.yaml) → normal task
```

Three things must all be true for a routine to fire:

1. This host has a clock unit installed (or something invokes `orbit clock tick`).
2. The routine's `enabled:` is true.
3. The routine is not paused on this host.

The workspace itself opts in by being registered: the tick loads definitions
from every registered, active checkout on the host, and there is no config key
to set. A **replica** checkout fires only its host-local `worktree-gc` and
`store-gc` routines, and only on a cron trigger. Worktree GC reclaims that
host's claimed-leaf worktrees after asking the owner whether each task
settled.

Store GC prunes this host's own store for that workspace (command
audit and resource history are host-wide; run audit and terminal pipeline
state are this workspace's) and does not call the owner. Every other routine,
and a delivery- or state-triggered copy of either job, is listed as
`owner-only` with the owner machine named and never fires there. Auto-tasks
are not evaluated there, and the auto-task panel disables toggle and manual
mint with the owner machine named.

## Turning the scheduler on

**1. Install the host clock.**

```bash
orbit routine init --install-clock
```

Reads the machine identity written by `orbit init` and installs the per-user OS
clock unit that runs `orbit clock tick` every minute — launchd on macOS, a systemd
user timer on Linux. It never creates or rewrites machine identity; `orbit init`
owns that.

```bash
orbit clock status                  # cadence and native manager state
orbit clock pause|enable            # host-wide, without touching definition state
orbit clock set --cadence-seconds 300  # whole-minute cadence, reloads the unit; a paused clock stays paused
```

The clock starts with no login environment. Credentials its runs need (on a Mac,
`CLAUDE_CODE_OAUTH_TOKEN`) come from the owner-only `~/.orbit/clock.env`, which
the install creates; see [macOS](macos.md).

`clock pause` stops scheduled invocation; a manual `orbit clock tick` still works.
`orbit sweep` is a compatibility alias for the same tick and produces the same output.

A tick has a five-minute cooperative deadline shared by discovery, routine
evaluation, auto-tasks, and recovery. An operation already running finishes
before the next boundary checks the budget; remaining workspaces are reported
in `skipped_workspaces`, `deadline_exceeded` is true, and the tick exits nonzero.
Skipped schedules keep their cursors unchanged, so the next tick retries
according to each definition's missed-run policy.

Source git and GitHub commands have a shared
30-second pass budget including fetch-lock waits; timeout cleanup kills their
process group, including fetch helpers.

The Linux service adds a ten-minute `TimeoutStartSec` with `KillMode=mixed`
as the hard backstop for an operation that never returns. Keep worker
containment enabled so pipeline runs have their own scopes; workers opting
out remain in the service cgroup and can be killed when that service stops.
`orbit clock status` and `orbit doctor` flag overdue or failed ticks and unsafe
recovery settings. Run `orbit clock repair` to update an older service, even
when it already names the current binary.

A paused clock stays paused.

**2. Enable routines, one at a time, as an operator.** Each routine is a YAML
file in this checkout's `.orbit/routines/`. Set its `enabled` field with the
audited verbs rather than by editing the file:

```bash
orbit routine list                          # rendered names, enabled and paused state
orbit routine enable <routine-name>         # operator only: sets enabled: true
orbit routine show <routine-name>           # confirm enabled and the effective state
orbit clock tick --dry-run                  # what would fire on this host; dispatches nothing
```

`enable` and `disable` (`orbit routine disable <routine-name>`) need operator
authority; an agent session is refused. They act on the selected workspace:
`--workspace <name>` when given (`orbit --workspace <name> routine enable
<routine-name>`), otherwise the current checkout. A routine that belongs to
another registered workspace is refused, and the error names the workspace to
rerun with.

The write re-checks the state it read and refuses if the routine
changed in the meantime; the command is audited. Copy the name from `orbit
routine list`: seeded names carry the workspace suffix (`<base>-<workspace>`),
which the base names in the [table below](automation-routines.md#the-seeded-routines) omit.

`.orbit/` is per-user state that git ignores, so the enabled field is a local
setting, not a commit. The host-local pause (`orbit routine pause`) and the host
clock (`orbit clock`) are separate controls; see
[Observe and control](#observe-and-control). Registering the checkout already
made the workspace a routine source; an older `.orbit/config.toml` may still
carry a `[routines]` section, which is ignored with a warning and can be
deleted.

<a id="the-nine-seeded-routines"></a>
<a id="the-ten-seeded-routines"></a>
<a id="the-eleven-seeded-routines"></a>

## Recommended enablement order

A newly initialized workspace starts with steps 1 and 2 enabled. Otherwise,
enable in this order and stop wherever the value runs out. Each step is safe
without the ones after it; **the reverse is not true.**

1. **`worktree-gc` first.** It is the only one that reclaims disk, and every
   implementation workflows create runs that can leave worktrees behind.
   Deterministic evidence-filing sweeps do not create worktrees. Running
   unattended drains without GC is how a workspace fills a disk. Watch one
   cycle with `orbit gc worktrees --dry-run` before enabling. →
   [maintenance-worktree-gc.md](maintenance-worktree-gc.md)
2. **`task-pilot`.** Its agent inspection is read-only; its apply step writes validated task selectors, and it makes everything downstream safer:
   populated `context_files` are what conflict detection and file reservation
   use to keep parallel runs off each other's files. It fires on task creation
   or a material edit (debounced two minutes, at most ten) rather than on a
   clock, so an unchanged backlog costs nothing.

   See
   [Tuning which tasks task-pilot prepares](automation-task-pilot.md#tuning-which-tasks-task-pilot-prepares).

No routine ships work. Dispatch the backlog with `orbit run auto` drains or
`orbit run ship` once the steps above have proved themselves against real
traffic. A workspace that still holds the retired `ship-sweep` routine loads it,
warns, and never runs it; `orbit workspace sync` retires the seeded copy.

## Verify without firing

```bash
orbit routine list                 # every routine: enabled / paused, next due
orbit routine show <name>          # definition, effective state, recent fires
orbit clock tick --dry-run         # what would fire; dispatches nothing
orbit clock tick --dry-run --verbose  # include not-due rows
orbit --workspace <name> clock tick --dry-run  # restrict to one workspace
```

A dry tick dispatches no routine and mints no auto-task, but it is not
write-free: it can still persist the validation status of registered checkouts
and open or migrate the host's scheduler store.

The global `--workspace` selector narrows a sweep — dry-run or live — to one
registered workspace's routines; without it the pass covers every registered
checkout on the host. A replica checkout contributes only its cron worktree-GC
and store-GC routines.

## Observe and control

Fires are ordinary runs — they appear in `orbit run history` under the actor
`routine/<name>`. Three controls act at different scopes, and an enabled
routine can still be held on one host:

| Control | Scope | Commands |
|---|---|---|
| Enabled (the definition) | The routine's versioned `enabled` field in this checkout | `orbit routine enable <name>`, `orbit routine disable <name>` (operator only, audited) |
| Host-local pause | This host only; unversioned; survives reboots | `orbit routine pause <name>`, `orbit routine resume <name>` |
| Host clock | Every routine and auto-task this host runs | `orbit clock pause`, `orbit clock enable` (definition state is untouched) |

```bash
orbit routine enable <routine-name>   # set enabled: true (operator only)
orbit routine disable <routine-name>  # set enabled: false (operator only)
orbit routine pause <name>            # hold it on this host only
orbit routine resume <name>           # clear the host-local pause
orbit clock status                    # clock cadence and whether it is paused
```

Verify an enablement, a pause and the clock separately. `orbit routine list`
shows each routine's enabled and paused state and its next due time, and
`orbit routine show <name>` shows its definition and effective state. An
auto-task uses its own kill-switch, `orbit auto-task toggle <name> on|off`
([Auto-tasks](auto-tasks-authoring.md)).

## "Why didn't it fire?"

Resolve the toggles in this order — `orbit routine list` shows all three at once:

1. `enabled: false` in this checkout's definition (`orbit routine enable` sets
   it; per-user, gitignored).
2. A local pause (`orbit routine pause`: this host only, unversioned, durable
   across reboots).
3. A tripped circuit breaker. After three fires in a row fail with the same
   error class the sweep stops firing the routine; `orbit routine list` shows
   it as `tripped` and `orbit routine show <name>` names the error class, last
   run and next trial time. The sweep retries once after a backoff (15 minutes,
   doubling to at most 4 hours) and after the host's Orbit build changes; a
   successful trial closes it.

   Fix the cause, then `orbit routine breaker reset
   <name>`, or `orbit routine breaker bypass <name>` to let one fire through.

If neither explains it, check further out: is this checkout registered as an
owner (`orbit workspace list`), is the clock unit running and not paused
(`orbit clock status`), and did the tick itself error
(`orbit log tail --level warn --since 1h`)? For a fire
that started and then failed, the run is the evidence —
[run-debugging-triage.md](../../orbit-orchestrate/references/run-debugging-triage.md).
