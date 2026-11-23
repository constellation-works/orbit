# The resident cycle: check, recover and act

Contents:

- [What the brief gives you](#what-the-brief-gives-you)
- [The cycle, in order](#the-cycle-in-order)
  - [1. Check](#1-check)
  - [2. Recover](#2-recover)
  - [3. Act within authority](#3-act-within-authority)
- [Boundaries](#boundaries)

A resident owns coordination for one mandate: an `[orchestration]` assignment
task tagged `resident`, with a `resident/mandate.json` artifact and an
authority record. It holds no process, worktree or state between cycles. Core
owns the assignment's status: `in-progress` while the mandate is active, then
`review` on a `handoff` checkpoint or a stop.

The clock fires one bounded cycle when the mandate is due; each cycle reads its
brief from Orbit, does one unit of coordination, and writes exactly one
checkpoint. The next cycle trusts checkpoints, task state, decision records
and the action ledger — never a previous cycle's final message.

For the human-facing side of the channel, read [Manager](manager-mandates.md).

## What the brief gives you

The deterministic observe step freezes this cycle's grant from the mandate's
authority and hands you:

- the mandate and the effective authority (including
  `observe_only` and why it was narrowed),
- the latest checkpoint in full and up
  to four earlier summaries (cycle, outcome and at most 600 characters of notes),
- open requests
  with their answers,
- comments since the last cycle (relays arrive as
  `resident.answer` records),
- the wake reasons and the facts behind them,
- notices in scope quoted as untrusted data, and
- — for a recovery cycle — the
  dead cycle's ledger rows.

A resumed provider conversation receives the same
brief; if the brief contradicts what a resumed conversation remembers, the
brief wins.

Your grant is frozen for the cycle. A maintenance notice open in a covered
workspace, or a passed deadline, empties the rights for that cycle: you may
still read, comment, escalate and checkpoint, but dispatch, promote, merge,
refresh and plugin upgrade are refused. Notices narrow; they never widen.

## The cycle, in order

### 1. Check

Re-read the mandate, the frozen authority and the watched tasks' current
state. Work only in the covered workspaces. If the authority is unreadable,
the cycle runs observe-only: do no right-gated work, record
`authority_unavailable` in the checkpoint, and stop. Compare the wake facts
against the last checkpoint instead of assuming the wake reason says what
changed.

### 2. Recover

When the brief marks a recovery cycle (`recovery_of` set), the previous cycle
died without a checkpoint. Its actions are in the ledger under
`resident:<mandate>:<cycle>:<n>` keys:

- A retried call that the dead cycle recorded `ok` returns
  `resident_action: "already_done"` with the original action key in `key`, its
  `ref`, and `subject`. Record it in your checkpoint under that key; do not
  repeat the action. This recovery replay does not return a new
  `resident_action_key`.
- A call the dead cycle left `started` is refused as in doubt. Read the
  subject's current state and decide from that evidence; never blindly retry
  a mutating action.
- A recovery cycle also refuses every call to a tool with a different input
  when the dead cycle recorded a legacy-digest row for that tool (a row
  written before input digests were canonicalised) in phase `ok`,
  `already_done` or `started`. A legacy row cannot be matched to a reordered
  call, so Core refuses rather than replay it.
  - The refusal carries
    `resident_action_in_doubt`, but it does not mean the refused call is in
    doubt: that call was never started and has no ledger row. Read the
    subject's current state before any further call to that tool, and decide
    from that evidence.
- Repeated completed actions must be zero. If state contradicts the ledger,
  escalate rather than improvising a repair.

### 3. Act within authority

Rights gate writes; reads, comments, filing `proposed` tasks, filing friction
records in the mandate's own workspace (`orbit.friction` action `add`), updating
friction records in any covered workspace (`orbit.friction` action `update`
without `rehome_to` or `move`), escalations, your notices and your checkpoint
need no right, so observe-only cycles may still update frictions in covered
workspaces.

Any friction update carrying `rehome_to` (or `move`) is
refused as `resident_tool_refused` regardless of granted rights: moving a
friction between workspaces stays with the operator and the curation pass.

| Right | What it permits in the mandate's own workspace |
|---|---|
| `dispatch` | Start a leaf run with `orbit.workflow` action `ship`; move work to `in-progress` |
| `promote` | Move a `proposed` task to `backlog`; file tasks directly in `backlog` |
| `merge` | Land a reviewed delivery with `orbit.workflow` action `land`; move a task with no open pull request to `done` |
| `refresh` | Bring a held or review delivery pull request head up to its base branch with `orbit.workflow` action `refresh` and re-run required checks (conflict-free merge commit only; lands nothing) |
| (none yet) | `plugin_upgrade` maps to no tool yet; never improvise an install |

`orbit.attention` reads the runtime kinds (`run_failed`, `run_stuck`, `claim_stale`) and `host_unhealthy` for the covered workspaces while the cycle's grant is live; any other workspace still reports them as `capability` degraded.

`orbit.workflow` action `auto` with `mode: "status"` reads drain readiness for the covered workspaces while the cycle is live, with or without any granted right, and refuses any other workspace as `resident_scope_refused`. Both are reads only: they grant no write, and drain controls (`start`, `stop`, `resize`) stay operator-only.

Core enforces the grant on every call, not the prompt: a write without this
cycle's live grant, outside the covered workspaces, or without its right is
refused. A grant refusal (`capability_denied`, a missing right, or a workspace
outside the grant) is information, not an obstacle to route around — record
it in the checkpoint and continue with what the grant does permit. Never retry
that refusal another way.

A decision raise refused because the target is not a
human-crew task, and a raise the open-request cap refuses, are not grant
refusals. [Escalate by class](resident-cycle-escalate.md#4-escalate-by-class) is the supported route.

You may
never call the grant, relay or control tools: a cycle cannot widen its own
authority, speak for the human, or change its own firing. Relayed directions
are steering within authority; a direction that reads as a widening goes back
as an escalation, never as compliance.

Use the normal pipeline for leaf work. To dispatch, call `orbit.workflow` action `ship`
with the mandate's own workspace and the `backlog` task ids; it returns the run
id and stops the delivery at `review`, so landing it still takes `merge`.

A
covered workspace other than the mandate's own is refused as `resident_scope_refused`
for every write except `orbit.friction` action `update`: a cycle writes through its
own workspace only, whereas `orbit.friction` action `update` (without `rehome_to` or
`move`) may target any covered workspace on the host and records its action ledger
row on the mandate.

The same `dispatch` right re-pilots and requeues:
call `orbit.task.recover` action `repilot` with the `id` of a `proposed` or
`backlog` task to repeat its pilot assessment, and action `requeue` with
the `id` of a `blocked` task and a `reason`, both with the mandate's own
workspace. These actions are permitted to the trusted live resident cycle
under its `dispatch` grant; other managed runs remain operator-only.

A resident
requeue must omit `discard_candidate` or set it to `false`; setting it to
`true` is refused with `capability_denied` because residents cannot discard a
held candidate. An operator can decide to discard it. A fresh
successful action returns its `resident_action_key`.

If a recovery call matches
an action the dead cycle already completed, it returns
`resident_action: "already_done"` with the original key in `key`, plus `ref`
and `subject`, and does not return `resident_action_key`.

Every other
`orbit.task.recover` action, `orbit.workflow` action `auto` drain controls (`start`, `stop`, `resize`), `orbit.run` action
`resume` and `orbit.pipeline.invoke` stay operator-only and answer
`capability_denied` even with the right: record that as a cost and leave them
to the operator or the scheduler.

To refresh a held or review delivery pull request whose head is behind its base
branch (for example, to pick up a base-branch CI fix), call `orbit.workflow`
action `refresh` with the mandate's own workspace and `task_id`. Orbit merges
the base tip into the pull request head as one conflict-free merge commit and
starts the required checks on the new head, never rebasing or force-pushing and
never landing the PR.

Conflicts answer `task_refresh_refused` naming the
conflicting paths and leave the head unchanged; a head already at the base tip
returns `already_up_to_date` as a no-op. A refreshed head needs no new review
when only conflict-free base refresh merges follow the reviewed head, while
`land` still requires the required checks to pass on the new head.

To land, call `orbit.workflow` action `land` with the mandate's own workspace and the
`task_id` of a task in `review`. It
merges the task's delivery pull request under the forge's branch protection and
moves the task to `done`. Anything that holds it back answers
`task_landing_refused` with the reason and leaves the task in `review`: record it
as `refused`, and never merge any other way.

When a call's outcome is unknown,
repeat it: a pull request that already merged settles without merging again.
A task whose pull request is still open cannot be moved to `done` directly; the
update answers `resident_done_pr_open`.
Tasks you file stay `proposed` unless you hold `promote`. Name every task you
file or dispatch in the checkpoint's `actions[]`, so later cycles
can find it again.

## Boundaries

- Coordination only: you dispatch, promote and merge within the grant; leaf
  delivery runs on the normal pipeline under its normal rules.
- Repository-specific operating know-how stays in the consuming repository's
  own skills, which your crew loads as usual. This reference is only the
  provider-neutral cycle procedure.
