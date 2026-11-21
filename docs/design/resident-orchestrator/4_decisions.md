---
title: Resident Orchestrator — Decisions
owner: claude
last_updated: 2026-11-20
last_validated: 2026-11-02
status: Draft
feature: resident-orchestrator
doc_role: decisions
type: design
summary: "Why the rewrite owns coordination only, rides the clock, keeps loops out of chats and consoles, gates widening on the human, trusts only Orbit state, keeps evolution apart from friction, and lets `dispatch` re-pilot and requeue"
tags: [resident-orchestrator]
paths: ["crates/orbit-core/src/application/resident/**", "crates/orbit-core/assets/jobs/**"]
related_features: [distributed-drain, routines, activity-job, human-decision-channel, fleet-notices]
related_artifacts: [ORB-21229, ORB-12488, ORB-10815, ORB-10776]
---

# Resident Orchestrator — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions
remain in place so their original reasoning stays legible. See
[CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.
Nothing in this folder is built yet, so every entry below is admitted through Door 2: each is a
standing rule for resident and manager work that slices and later changes must apply.

## Coordination only: the resident owns no execution

**Recorded:** 2026-11 · [ORB-21229] (supersedes nothing live; the earlier folder was removed by [ORB-12488])

### Context

Orbit had a resident-orchestrator design before. It was first written in PR #630 (`442c00361`,
"design resident orchestrator epic pickup") and revised in PR #934 (`6c31b58c0`). [ORB-10815]
(`3a09021e8`) then rewrote it as v2. V1 was two jobs: a workspace sequencer that took one action per
auto tick and could `decision: hold` the whole workspace behind an epic, and an epic orchestrator
that could dispatch children but not edit the tree. [ORB-10776] had already pushed the fire clock
out of Orbit and added an internal session log as the orchestrator's memory. V2 gave every epic one
stable worktree and branch. Children landed into it sequentially in local mode, an epic finisher
agent worked inside that worktree, and an `epic`-tagged root held one reservation over the union of
its descendants' `context_files`.

[ORB-12488] removed it with the epic pipeline ("Epic is a tag, not a pipeline",
[distributed-drain decisions](../distributed-drain/4_decisions.md#epic-is-a-tag-not-a-pipeline)).
The design owned execution, and every execution feature cost something everywhere else. It pinned
a drain slot for the epic's life. The union reservation shadowed unrelated leaves. Two activities
needed admission exclusions for it. It ran only on the owner. It needed its own GC rule, an internal
session-log store, and drain-scan exclusions. The benefit, one review artifact per epic, was never
worth that against a single leaf a strong crew can take whole. A second host made the cost visible.

What the old design did not solve is what this one is for: who drives a mandate when no human chat
is open. The old resident was a supervisor for an epic's execution, fired by an external cron. It
was not a front door, had no authority model, and no human channel beyond comments.

### Decision

A resident owns **coordination only**, and any future resident change must keep it that way:

- no worktree, branch or tree edit of its own; it changes code only by filing or dispatching leaf
  tasks;
- no pinned drain slot; a cycle is an ordinary short job run, and leaf work takes slots through the
  normal drain;
- no admission path, reservation or footprint of its own; the leaf tasks it dispatches are admitted
  by their own `context_files` like any other;
- no store beyond the records listed in [2_design.md](./2_design.md): checkpoints are task comments,
  authority is a task record, escalations are decision requests, costs feed evolution records.

When a resident change seems to need one of these, the need goes to the leaf pipeline or the drain
as a generic feature, not into the resident.

### Consequences

- The [ORB-12488] decision stands unchanged: epics stay tags, and there is one leaf pipeline.
- A resident crash loses at most one cycle of coordination. It never strands a worktree or holds a
  slot.
- A resident cannot finish a large change by editing the tree itself, as the v2 finisher could. A
  combined result is reviewed as a leaf task, filed by the resident when needed.
- Cost: work that genuinely needs one integrated review across several leaves takes one more leaf
  task (an integration or review leaf) and its slot time, rather than happening in an epic worktree.

## Residents ride the clock: no resident daemon

**Recorded:** 2026-11 · [ORB-21229]

### Context

A resident has to react to events (a run finishing, a decision answered) and also wait for hours.
A long-lived process per resident, or one daemon hosting all of them, would react instantly and
could keep a provider conversation warm. Orbit already decided against resident processes: the OS
owns the clock, and the sweep is stateless-in, durable-out
([routines decision](../routines/4_decisions.md#the-os-owns-the-clock-stateless-orbit-sweep-under-launchdsystemd-no-resident-daemon)).
A daemon also needs supervision, restart policy and an upgrade story (polaris RB-15 replaces the
binary on every host), and it would hold state that a crash loses.

### Decision

A resident is an Orbit activity fired by the existing clock through a typed state trigger. Nothing
runs between cycles. Reaction latency is bounded by the clock cadence and the mandate's cadence
floor, not by a listener. Any later design that wants a resident to react faster lowers the
cadence or adds a trigger source to the clock; it does not add a resident process.

### Consequences

- A binary upgrade needs no resident restart: the next cycle runs the new binary.
- Crash recovery is the general recovery of a job run plus the checkpoint contract.
- Cost: reaction latency is at least one clock tick (60 s by default) and at most the cadence floor
  (10 min by default) after a change, and a provider conversation is not kept warm between cycles.

## Orchestration loops do not live in chat sessions

**Recorded:** 2026-11 · [ORB-21229]

### Context

Today's loops live in interactive chats, as self-scheduled wakeups, 30-minute monitors and ssh
polls. They work while the chat is open and stop at a closed laptop, a crash or a compaction. Making
them more robust in place (longer monitors, a persistent session host) was one alternative.

### Decision

No orchestration loop runs in a chat. A chat session, including the manager's, holds no loop and no
state that Orbit does not hold. It can be closed at any moment without losing work. A chat is for
the human conversation, an override (`takeover`) or a probe, and anything that must keep happening
after it closes becomes Orbit state and a resident cycle.

### Consequences

- The manager can run on any provider, and two manager sessions can coexist.
- Takeover is a state change on the mandate, not a hand-off of a chat transcript.
- Cost: the manager has to re-read Orbit to answer "what is going on", which takes a few tool calls
  where a long-running chat would remember. The digest and `orbit resident show` exist to keep that
  cheap.

## No tmux or PTY console for residents

**Recorded:** 2026-11 · [ORB-21229]

### Context

One way to keep orchestrator sessions alive without the human's chat is to host them in tmux or a
PTY on the server, through the [web terminal](../_archived/web-terminal/1_scope.md) or ssh, so the human can
attach and the session keeps running. That keeps the familiar chat shape and needs no new records.

### Decision

Residents are not hosted consoles. A console session is a daemon with a terminal: it holds state in
the provider's context, dies with its process, compacts, and can be driven by typing into it. That
bypasses the authority grant and the audit trail. The web terminal stays a human tool; it is not a
resident runtime, and no resident feature may depend on attaching to one.

### Consequences

- What a resident did is always in Orbit records, never only in a scrollback.
- Cost: the human cannot watch a resident think in real time. They see checkpoints after each cycle,
  and live run logs only while a cycle runs.

## Narrowing is free, widening needs the human

**Recorded:** 2026-11 · [ORB-21229]

### Context

Several actors touch a resident's authority: the human, the manager, the resident itself, fleet
notices and workspace config. A model where every change needs the human makes pausing a misbehaving
resident slow. A model where the manager can grant on the human's behalf makes the manager's
honesty and reading comprehension the security boundary.

### Decision

Any change that reduces what a resident may do (removing a right, dropping a workspace, shortening a
deadline, pausing, stopping, a maintenance notice going observe-only) may be made by any actor that
can write the mandate and is recorded. It takes effect for cycle execution at the next cycle
boundary. Pause and stop prevent a new cycle from starting. A live cycle keeps its frozen grant,
completes, checkpoints (or receives a synthesized checkpoint from `resident_finalize`), and exits at
the next cycle boundary, within at most one agent budget. A stop then hands the assignment to
`review`. Any change that increases authority takes effect only when the human records it: directly
with operator authority, or by answering a `resident_authority` decision request that quotes the
relay verbatim. No relay, notice, direction, config default or resident decision widens authority.
This rule applies to every future authority dimension added to residents.

### Consequences

- A bad relay or a prompt-injected notice can at worst pause a resident, never unlock a merge.
- The manager is a relay, not a delegate: its words never carry authority.
- Cost: every widening costs the human one decision, even when the human just said it in the
  manager chat. A mandate that needs a new right mid-flight waits for that answer.

## A cycle is correct from Orbit state alone

**Recorded:** 2026-11 · [ORB-21229]

### Context

A cycle's agent could hand its successor a summary in its final message, or the successor could
resume the same provider conversation and remember. Both are cheap and often better context. Both
are lost on a crash, an expired conversation or a provider switch, and the root guide forbids
activities that rely on agent output
([activity-job](../activity-job/2_design.md)).

### Decision

Every resident cycle must be able to act correctly with no previous final message and no provider
conversation, using only the mandate, the authority, checkpoints, task and run state, decision
records, notices and the audit log. The cycle job declares no response envelope. A resumed
conversation is an optional adapter that may add context but never supplies a fact the brief lacks.
A finalizer synthesizes a checkpoint when the agent wrote none.

### Consequences

- Any registered crew can pick up any mandate at any cycle.
- What a resident must remember goes into the checkpoint's `notes`, where the human can read it too.
- Cost: every cycle pays to rebuild its brief, a few thousand tokens of checkpoints and state,
  whether or not a warm conversation could have skipped it.

## Evolution records are separate from friction

**Recorded:** 2026-11 · [ORB-21229]

### Context

Recurring workarounds could be recorded as friction, which already has dedupe, triage and a
`resolves` link. But friction is a repair queue: one record per thing that broke, closed by a fix. A
missing capability that costs ten minutes in every cycle never "breaks". Logged as friction, it is
either triaged as working-as-designed or drowned among real breakages. Its cost summed across
residents, which is the point, has nowhere to live.

### Decision

Capability gaps are evolution records, keyed by signature, corroborated by cost events, ranked by a
scheduled pass, and turned into `proposed` design tasks only above measured thresholds. Friction
stays the record for defects and contradicted assumptions. When a future signal could go to either,
the test is: would a fix make the system *behave as designed* (friction) or *be designed
differently* (evolution)?

### Consequences

- The human sees "what keeps costing us" ranked by measured cost, separate from "what broke".
- Proposals reach the human only as `proposed` tasks; what to build stays the human's decision.
- Cost: one more record kind, with its own tools and states, and agents must choose between two
  records for borderline events. Some events will be logged in both.

## Residents re-pilot and requeue under `dispatch`

**Recorded:** 2026-11-20 · [ORB-25959] · decided by the formation chief under Daniel's delegation
of 2026-11-20 (the chief's ruling on on-call [ORB-25951]); not Daniel's own approval

**Code anchors:** `crates/orbit-runtime/src/authorization.rs` `RESIDENT_CYCLE_RIGHT_GATED_OPERATIONS`;
`crates/orbit-tools/src/builtin/orbit/task/operation.rs` `TaskVerb::preempt`;
`crates/orbit-core/src/adapter/tool_host/task/operation.rs` `dispatch`

### Context

The rights table gave `dispatch` `orbit.task.recover` actions `repilot` and `requeue`, but the
operator row ran before the cycle guard, so a cycle holding the right was refused
`capability_denied`; only `orbit.workflow` action `ship` was exempt. Every re-pilot or requeue a
resident had prepared became a blocking escalation to the human, pilot-warned tasks waited
overnight for a human orchestrator, and residents recorded the gap as a cost in nearly every
checkpoint. The alternative was to keep both operator-only and leave them to escalations.

### Decision

A resident cycle's run passes the operator row and the managed-run refusal for exactly two recover
actions, `repilot` and `requeue`, under the same guard as `ship`: the run is a cycle only by its
trusted managed-run binding, never by input, and the cycle guard requires its live cycle, the
`dispatch` right and a covered workspace, and keys the write on the action ledger. The handlers'
own operator checks are skipped only for a call that guard ledgered. Every other recover action,
`orbit.run` actions `auto` and `resume`, and `orbit.pipeline.invoke` stay operator-only. A future
operator-row tool reaches residents the same way: name it in the exemption list and let the cycle
guard decide, never by widening the row itself.

### Consequences

- A resident re-pilots a stale or warned task and requeues a held delivery itself, keyed and
  auditable, instead of escalating.
- Callers outside a resident cycle keep the operator requirement.
- Cost: a resident with `dispatch` can override a recorded escalation (requeue) and spend pilot runs
  (re-pilot) without a human in the loop; the grant is the only gate, so `dispatch` is a wider right
  than it was, and the change takes the security review the chief's ruling asked for.

## Residents cannot discard a candidate on requeue

**Recorded:** 2026-11-21 · [ORB-26184] · Approved by Daniel in the chief's chat
(2026-11-21), answering “Approve all three as the captain recommends?” with
“Yes, all three.”

### Context

The [dispatch requeue decision](#residents-re-pilot-and-requeue-under-dispatch)
lets a live resident cycle requeue a blocked task so its next run can resume a
held delivery. The same action also accepted `discard_candidate: true`, which
skips publication authentication and discards the candidate and its undelivered
Orbit pull requests. The approval to let residents requeue did not include that
discard behavior.

### Decision

A live resident cycle may requeue only when `discard_candidate` is omitted or
false. A requeue with `discard_candidate: true` is refused with
`capability_denied` and names the parameter. Non-resident operator callers keep
the ability to discard a candidate through requeue.

### Consequences

- A resident requeue preserves the held candidate and its pull requests for the
  next run to resume.
- When a resident determines that a held candidate should be discarded, it
  leaves that decision to an operator.
- Cost: discarding a held candidate needs an operator action rather than being
  completed by the resident cycle.

## Task References

- [ORB-21229] — rewrote the resident-orchestrator design and recorded these decisions
- [ORB-25959] — let resident cycles with `dispatch` re-pilot and requeue
- [ORB-26184] — refuse resident cycles' candidate discards during requeue
- [ORB-25951] — on-call whose chief ruling delegated and conditioned that change
- [ORB-12488] — removed the epic pipeline and the earlier resident-orchestrator folder
- [ORB-10815] — revised the earlier design to v2: epic-owned worktree and windowed drain
- [ORB-10776] — moved the earlier design's fire clock outside Orbit and added its session log

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
