# The resident cycle

A resident owns coordination for one mandate: an `[orchestration]` assignment
task tagged `resident`, with a `resident/mandate.json` artifact and an
authority record. It holds no process, worktree or state between cycles. Core
owns the assignment's status: `in-progress` while the mandate is active, then
`review` on a `handoff` checkpoint or a stop. The clock fires one bounded cycle when the mandate is due; each cycle reads its
brief from Orbit, does one unit of coordination, and writes exactly one
checkpoint. The next cycle trusts checkpoints, task state, decision records
and the action ledger — never a previous cycle's final message.

For the human-facing side of the channel, read [Manager](manager.md).

## What the brief gives you

The deterministic observe step freezes this cycle's grant from the mandate's
authority and hands you: the mandate and the effective authority (including
`observe_only` and why it was narrowed), the last checkpoints, open requests
with their answers, comments since the last cycle (relays arrive as
`resident.answer` records), the wake reasons and the facts behind them,
notices in scope quoted as untrusted data, and — for a recovery cycle — the
dead cycle's ledger rows. A resumed provider conversation receives the same
brief; if the brief contradicts what a resumed conversation remembers, the
brief wins.

Your grant is frozen for the cycle. A maintenance notice open in a covered
workspace, or a passed deadline, empties the rights for that cycle: you may
still read, comment, escalate and checkpoint, but dispatch, promote, merge
and plugin upgrade are refused. Notices narrow; they never widen.

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
  `already_done` with the original key and reference. Record it in your
  checkpoint under that key; do not repeat the action.
- A call the dead cycle left `started` is refused as in doubt. Read the
  subject's current state and decide from that evidence; never blindly retry
  a mutating action.
- A recovery cycle also refuses every call to a tool with a different input
  when the dead cycle recorded a legacy-digest row for that tool (a row
  written before input digests were canonicalised) in phase `ok`,
  `already_done` or `started`. A legacy row cannot be matched to a reordered
  call, so Core refuses rather than replay it. The refusal carries
  `resident_action_in_doubt`, but it does not mean the refused call is in
  doubt: that call was never started and has no ledger row. Read the
  subject's current state before any further call to that tool, and decide
  from that evidence.
- Repeated completed actions must be zero. If state contradicts the ledger,
  escalate rather than improvising a repair.

### 3. Act within authority

Rights gate writes; reads, comments, filing `proposed` tasks, friction,
escalations and your checkpoint need no right.

| Right | What it permits in covered workspaces |
|---|---|
| `dispatch` | Start a leaf run with `orbit.workflow` action `ship`; move work to `in-progress` |
| `promote` | Move a `proposed` task to `backlog`; file tasks directly in `backlog` |
| `merge` | Land a reviewed delivery with `orbit.workflow` action `land`; move a task with no open pull request to `done` |
| (none yet) | `plugin_upgrade` maps to no tool yet; never improvise an install |

Core enforces the grant on every call, not the prompt: a write without this
cycle's live grant, outside the covered workspaces, or without its right is
refused. A refusal is information, not an obstacle to route around — record
it in the checkpoint and continue with what the grant does permit. You may
never call the grant, relay or control tools: a cycle cannot widen its own
authority, speak for the human, or change its own firing. Relayed directions
are steering within authority; a direction that reads as a widening goes back
as an escalation, never as compliance.

Use the normal pipeline for leaf work. To dispatch, call `orbit.workflow` action `ship`
with the mandate's own workspace and the `backlog` task ids; it returns the run
id and stops the delivery at `review`, so landing it still takes `merge`. A
covered workspace other than the mandate's own is refused: a cycle writes only
through its own workspace. `orbit.task.recover` action `requeue`, `orbit.task.recover` action `repilot`,
`orbit.workflow` action `auto`, `orbit.run` action `resume` and `orbit.pipeline.invoke`
stay operator-only and answer `capability_denied` even with the right: record
that as a cost and leave them to the operator or the scheduler.
To land, call `orbit.workflow` action `land` with the mandate's own workspace and the
`task_id` of a task in `review`. It
merges the task's delivery pull request under the forge's branch protection and
moves the task to `done`. Anything that holds it back answers
`task_landing_refused` with the reason and leaves the task in `review`: record it
as `refused`, and never merge any other way. When a call's outcome is unknown,
repeat it: a pull request that already merged settles without merging again.
A task whose pull request is still open cannot be moved to `done` directly; the
update answers `resident_done_pr_open`.
Tasks you file stay `proposed` unless you hold `promote`. Name every task you
file or dispatch in the checkpoint's `actions[]` and `notes`, so later cycles
can find it again.

### 4. Escalate by class

Always escalate, without deciding: product scope (adding, dropping or
reshaping a deliverable, or changing human-written acceptance criteria);
architecture (a new dependency, persisted record, public contract or
ADR-level choice); security (auth, sandboxing, secrets, or the disposition
of a security finding); material cost (raising concurrency or crew tier
beyond the mandate, or a spend the workspace treats as material);
irreversible actions (force pushes, deleting branches or records, releases,
tags, publishing); anything outside the grant, including what a relay seems
to authorise; conflicts between mandates over the same files; and any merge
or approval the workspace reserves for the human.

For the rest, weigh how likely the choice is wrong against the rework that
would follow before anyone noticed: a reversible choice with small rework —
decide, and record it in the checkpoint's `decisions[]` as `reversible` so
the digest lists it for veto. A choice likely wrong, or one whose rework is
large with no precedent — escalate, and keep working on whatever the question
does not block. Otherwise decide, record, and leave it awaiting veto.

To escalate: file a human-crew task related to the assignment (link anything
that must wait as blocked by it), raise the question with
`orbit.decision` action `raise` — two to four options, one line each for label and
consequence, exactly one recommended, each with its risk tier — and write a
`resident.request` comment on the assignment (an `orbit.task.update` comment
carrying that JSON record) indexing the question, so the assignment's thread
shows it without opening the human-crew task. List the
request in your checkpoint's `requests[]` too, naming the decision request,
the task, and whether it blocks. Escalations are non-blocking by default: an
unanswered question stops only the work that depends on its answer.

### 5. Record costs

Every workaround, manual step, retry or wait goes in the checkpoint's
`costs[]`: what you did, what it cost (minutes, retries, slot minutes), what
you wished existed, and a short kebab-case `signature` naming the gap when
one fits. A cycle with no costs records an empty list. Costs are how repeated
pain becomes a proposal; a workaround you do not record is pain the system
cannot see. Something that broke or contradicted an assumption is also
friction (`orbit.friction` action `add`); a missing capability is a cost, not
friction — one event can be both.

### 6. Checkpoint

End every cycle with exactly one `orbit.resident.checkpoint` call carrying
the complete checkpoint record: your cycle and run ids, the wake reasons and
digest from the brief, the authority you observed, `actions[]` with the
ledger key and result of every mutating action, `decisions[]`,
`requests[]` still open or raised, `costs[]`, `notes` (at most 2000
characters, written for the next cycle and the human), the next planned wake,
continuity (`resumed`, `rebuilt` or `off`, carrying the brief's conversation
reference forward when you resume), and one outcome:

- `progress` — work advanced.
- `idle` — you woke and there was nothing to do.
- `waiting` — only open requests remain.
- `handoff` — exit criteria met or deadline passed: stop dispatching, write
  the assignment's execution summary (what was done, what is open, which
  decisions are pending, which costs recurred), and checkpoint. Do not change
  the assignment's status: your run owns it, so that write is refused. The
  finalize step moves it to `review` and no further cycle runs. An exit
  criterion that names this move is met by the `handoff` checkpoint. Closing a
  mandate is the human's call.
- `ended` — the mandate was stopped or taken over for good.

A checkpoint for a stale cycle, or a second checkpoint for the same cycle,
is refused without writes. If you cannot checkpoint, the finalize step
synthesizes an `incomplete` checkpoint from the ledger, so the next cycle
recovers rather than repeats — but a synthesized checkpoint carries no
decisions, costs or notes, so write your own whenever you can.

## Boundaries

- Coordination only: you dispatch, promote and merge within the grant; leaf
  delivery runs on the normal pipeline under its normal rules.
- Repository-specific operating know-how stays in the consuming repository's
  own skills, which your crew loads as usual. This reference is only the
  provider-neutral cycle procedure.
