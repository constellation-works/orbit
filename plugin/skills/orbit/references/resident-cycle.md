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
`observe_only` and why it was narrowed), the latest checkpoint in full and up
to four earlier summaries (cycle, outcome and at most 600 characters of notes), open requests
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
escalations, your notices and your checkpoint need no right.

| Right | What it permits in covered workspaces |
|---|---|
| `dispatch` | Start a leaf run with `orbit.workflow` action `ship`; move work to `in-progress` |
| `promote` | Move a `proposed` task to `backlog`; file tasks directly in `backlog` |
| `merge` | Land a reviewed delivery with `orbit.workflow` action `land`; move a task with no open pull request to `done` |
| (none yet) | `plugin_upgrade` maps to no tool yet; never improvise an install |

`orbit.attention` reads the runtime kinds (`run_failed`, `run_stuck`, `claim_stale`) and `host_unhealthy` for the covered workspaces while the cycle's grant is live; any other workspace still reports them as `capability` degraded. That is a read only: it grants no write.

Core enforces the grant on every call, not the prompt: a write without this
cycle's live grant, outside the covered workspaces, or without its right is
refused. A refusal is information, not an obstacle to route around — record
it in the checkpoint and continue with what the grant does permit. A decision
raise refused because its target is not a human-crew task is an invalid
target, not a grant refusal: do not repeat that target, and do not treat the
refusal as an open decision. The next wake uses the escalation route in step
4. That route is the supported call, not a bypass. You may
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
file or dispatch in the checkpoint's `actions[]`, so later cycles
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

To escalate, put the choice on a human-crew task and raise the decision
there. This is the only answerable route, including a security choice on a
machine-owned repair. A raise whose `task_id` is the assignment, that repair,
or any other task not assigned to a human crew is refused and writes nothing.
Do not reassign the repair to a human crew. The refusal is not an open
decision, and a warning or a comment that only says one is pending is not one
either.

1. Reuse an open choice before you file or raise. Read the latest
   checkpoint's `requests[]`, `brief.open_requests`, and `orbit.decision`
   action `list` with `gate` `agent_request`. When a human-crew task already
   holds this choice, use that request's id. Raising the same question again
   — case and spacing ignored, the same `topic` — returns that request with
   `deduplicated: true` and writes nothing. Do not file a second human-crew
   task for the same open choice. A task holds one open request; a different
   question on it is refused until that request is answered.
2. Otherwise file one human-crew task in the assignment's workspace with
   `orbit.task.add`. Set `crew` to a configured human crew: `orbit config
   show` lists one with provider `human`, and `orbit config get
   crews.<name>.kind` answers `human`. Choose the sole human crew, or the one
   matching the operator this mandate is for. If no human crew is configured,
   or more than one matches and you cannot tell which, do not raise. Record
   that as a capability failure (step 5) and still checkpoint. Do not assign
   an agent crew. Relate the
   task to the assignment, and link anything that must wait as blocked by it.
   Filing needs no right.
3. Raise on that human-crew task, never on the assignment or the
   machine-owned repair: `orbit.decision` action `raise` with `task_id` set
   to it, a question of one or two sentences, and two to four options. Each
   option is one line of `label` and `consequence`, exactly one has
   `recommended: true` and is listed first, and each carries a `risk_tier` of
   `routine`, `reversible`, `irreversible`, `authority` or `spend`. The
   returned `id` is the decision request. Answering stays the human's. You
   cannot answer, reverse, or set `auto`.
4. Record that returned id in three places, and nowhere else as the decision.
   The warn's `evidence` is
   `{"type": "decision", "value": "<returned id>"}`. The assignment gets a
   `resident.request` comment (an `orbit.task.update` comment carrying that
   JSON record) whose `decision_request` is that id and whose `task` is the
   human-crew task, so the assignment's thread shows the question without
   opening the human-crew task. The checkpoint's `requests[]` entry names the
   same `decision_request`, the human-crew task, and whether it blocks.
5. A later wake retries this route when the last cycle missed it: the raise
   targeted a task that is not a human crew's, only a warning was posted, or
   a request was recorded without a returned id. Do not repeat the refused
   target. When the route itself cannot run — no human crew is configured,
   more than one human crew matches and you cannot tell which, or filing or
   raising answers a named capability refusal such as `capability_denied` —
   record that code and message in the checkpoint's `costs[]`. Name the
   capability failure. It is not a pending decision.

Security always takes this route: auth, sandboxing, secrets, the disposition
of a security finding, and any security policy. You do not choose. The
human's answer is the only change to that policy.

Escalations are non-blocking by default: an unanswered question stops only
the work that depends on its answer. Every escalation that needs the operator
also gets a warn notice
([Status line and warns](#6-status-line-and-warns)).

### 5. Record costs

Every workaround, manual step, retry or wait goes in the checkpoint's
`costs[]`: what you did, what it cost (minutes, retries, slot minutes), what
you wished existed, and a short kebab-case `signature` naming the gap when
one fits. A cycle with no costs records an empty list. Costs are how repeated
pain becomes a proposal; a workaround you do not record is pain the system
cannot see. Something that broke or contradicted an assumption is also
friction (`orbit.friction` action `add`); a missing capability is a cost, not
friction — one event can be both.

### 6. Status line and warns

Notices are how the operator sees a resident without opening its task. Post
them with `orbit.notice.post` in the mandate's own workspace; they need no
right, no linked task and no evidence.

**One status line, every cycle.** Before the checkpoint, post one `info`
notice with `dedupe_key` `resident:<mandate id, lowercase>:status`: a title of
at most 120 characters saying where the mandate stands now ("3 in review, 1
waiting on a merge", "All quiet"), and a body only when it adds something. The
post replaces the previous cycle's line in place (`outcome` `updated`); never
post a second info notice or a different key. Name no expiry: Orbit sets it to
twice the mandate's heartbeat, so a resident that stops cycling goes silent
instead of looking alive, and naming one is refused. Post it even when nothing
changed, since the post is what keeps the line alive. On a `handoff` or
`ended` checkpoint the line says so. When the manager pauses, takes over,
stops, resumes or releases the mandate, Orbit writes that line itself.

**A warn for each thing that needs the operator.** A blocker or issue only
the operator can clear gets one `warn` notice, never an `incident` (that kind
means something is broken) and never a line in the status notice:

- The title names the exact ask: "Decide: retry or drop the stuck delivery",
  "Fix the GitHub credential on the build host", "Merge #412".
- One warn per blocker. Give `linked_task` when a task is the blocker and let
  the key derive, or pass `dedupe_key` `resident:<mandate>:warn:<blocker>` (for
  example `…:warn:pr-412`). Posting the same key again updates that warn, so a
  repeat cycle never opens a duplicate.
- When the ask is a choice between options, follow the step 4 route first
  and cite only the id that raise returned, in the warn's `evidence` as
  `{"type": "decision", "value": "<returned id>"}`, so the answer lands in
  task history. A warn with no such id is not an answerable decision.
- A warn lasts 72 hours from its last post. Re-post one that still stands when
  its ask changes and at least once a day; one you stop re-posting lapses.
- Close it yourself with `orbit.notice.close` (`dedupe_key` or `id`, and a
  `reason`) in the cycle that sees the blocker cleared. You can close only
  notices this mandate's cycles posted. A warn whose `linked_task` reaches
  `done` closes itself.

The brief lists the notices in scope, your own included: check it for warns
you opened before posting or closing. The limits are 10 open notices and 30
posts an hour per mandate; a refusal is a cost to record, not a reason to
drop the checkpoint.

### 7. Checkpoint

End every cycle with exactly one `orbit.resident.checkpoint` call carrying
the complete checkpoint record: your cycle and run ids, the wake reasons and
digest from the brief, the authority you observed, `actions[]` with the
ledger key and result of every mutating action, `decisions[]`,
`requests[]` that name a returned decision id, `costs[]`, `notes` (at most 600
characters: only what changed this cycle and what is next), the next planned wake,
continuity (`resumed`, `rebuilt` or `off`, carrying the brief's conversation
reference forward when you resume), and one outcome:

- `progress` — work advanced.
- `idle` — you woke and there was nothing to do.
- `waiting` — only requests that name a returned decision id remain. A refused raise or a warning alone is not this outcome.
- `handoff` — exit criteria met or deadline passed: stop dispatching, write
  the assignment's execution summary (what was done, what is open, which
  returned decision ids are still unanswered, which costs recurred), and
  checkpoint. A refused raise or a warning alone is not a pending decision. Do not change
  the assignment's status: your run owns it, so that write is refused. The
  finalize step moves it to `review` and no further cycle runs. An exit
  criterion that names this move is met by the `handoff` checkpoint. Closing a
  mandate is the human's call.
- `ended` — the mandate was stopped or taken over for good.

Keep standing state in structured fields such as `requests[]` and `decisions[]`,
carrying it forward without retelling it in prose. `requests[].decision_request`
is the id `orbit.decision` action `raise` returned. A later wake reuses that
open choice instead of raising a new one. Do not carry a refused raise, or a
warning that has no returned id, as an open request. Put operator asks in
`requests[]` and a warn notice, never a "FOR DANIEL" paragraph in notes. Notes are the cycle's delta, not a recurring status report.
The brief has no older-notes tail; read task state when you need older detail.

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
