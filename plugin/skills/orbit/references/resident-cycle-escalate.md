# The resident cycle: escalate, record and checkpoint

Contents:

- [4. Escalate by class](#4-escalate-by-class)
- [5. Record costs](#5-record-costs)
- [6. Status line and warns](#6-status-line-and-warns)
- [7. Checkpoint](#7-checkpoint)

## 4. Escalate by class

Always escalate, without deciding:

- product scope (adding, dropping or
  reshaping a deliverable, or changing human-written acceptance criteria);
- architecture (a new dependency, persisted record, public contract or
  ADR-level choice);
- security (auth, sandboxing, secrets, security policy, or
  the disposition of a security finding);
- material cost (raising concurrency or crew tier
  beyond the mandate, or a spend the workspace treats as material);
- irreversible actions (force pushes, deleting branches or records, releases,
  tags, publishing);
- anything outside the grant, including what a relay seems
  to authorise;
- conflicts between mandates over the same files; and
- any merge
  or approval the workspace reserves for the human.

For the rest, weigh how likely the choice is wrong against the rework that
would follow before anyone noticed: a reversible choice with small rework —
decide, and record it in the checkpoint's `decisions[]` as `reversible` so
the digest lists it for veto. A choice likely wrong, or one whose rework is
large with no precedent — escalate, and keep working on whatever the question
does not block. Otherwise decide, record, and leave it awaiting veto.

Security policy, and the disposition of a security finding, stay a human
answer. The cycle cannot answer a decision, reverse one, or set `auto`.
Do not reassign a machine-owned repair to a human crew. A raise on that
repair is refused because the task is not assigned to a human crew, and that
refusal writes nothing.

A warning posted afterwards, with no returned id, is
not an open decision: do not store it in `requests[]`, do not cite it as
decision evidence, and do not set outcome `waiting` for it. The next wake
uses the route below. It does not repeat the refused repair as the target,
and it does not keep waking as if the warning were pending.

To escalate, in this order:

1. Reuse an open choice before filing. The same question, ignoring case and
   spacing, and the same topic, is one request. Raise `orbit.decision` action
   `raise` on the human-crew task that already holds it. A replay returns
   `deduplicated: true` and the same `id`, including while the open-request
   cap would refuse a new request. Do not file another human-crew task for
   that question.
2. If this question has no recorded request id, read the latest checkpoint's
   `costs[]` for kind `wait` and signature `open-request-cap` whose `what`
   names a human-crew task id for this question. Retry the raise on that same
   task. Do this on a wake whose brief and `requests[]` carry no decision
   request id.
   - Do not file another human-crew task. If the cap still refuses,
     carry that same cost on this cycle's one checkpoint. One retry of that raise per
     wake. An answer frees a slot. Until then the same question still dedupes,
     and a new question stays refused.
3. Otherwise file one human-crew task related to the assignment. Discover the
   crew with `orbit config show` (a human crew has provider `human`) and
   confirm with `orbit config get crews.<name>.kind`, which answers `human`.
   Use the only human crew, or the crew the mandate names when more than one
   exists and that name's kind is `human`.
   - When none is configured, or more
     than one exists and the mandate names none of them, do not guess, do not
     read a `human_crew` field from task show, and do not assign an agent crew.
     Record that capability failure as in [Record costs](#5-record-costs) and
     continue the cycle; the one checkpoint still comes at the end. The next wake retries this discovery or records the same
     failure.
4. Leave the machine-owned repair on its own crew. The human-crew task is a
   separate related task (`related_to`). Work that must not start until the
   answer lists that task in `dependencies`. Link anything else that must
   wait with `blocked_by`; that relation does not hold admission.
5. Raise on that human-crew task: a question of one or two sentences, and two
   to four options. Each option is `{label, consequence, recommended, risk_tier}`.
   `label` and `consequence` are one line each. Exactly one option has
   `recommended: true`, and it is listed first. `risk_tier` is `routine`,
   `reversible`, `irreversible`, `authority` or `spend`. Pass the same `topic`
   on every retry when you set one. The filer is the session, not an input
   `model`.
6. Only an `id` the raise returns is an answerable decision. Cite it even
   when `deduplicated` is true. Write a `resident.request` comment on the
   assignment (an `orbit.task.update` comment carrying that JSON record) with
   `decision_request` set to that id and `task` set to the human-crew task,
   so the assignment's thread shows the question without opening the human
   task.
   - List it in checkpoint `requests[]` too: `decision_request` is the
     id, `task` is the human-crew task, and `blocking` says whether it blocks.
     `decision_request` must be non-empty, so an entry without a returned id is
     refused. Escalations are non-blocking by default: an unanswered question
     stops only the work that depends on its answer. Every escalation that
     needs the operator also gets a warn notice
     ([Status line and warns](#6-status-line-and-warns)) whose decision evidence
     is that same id.
7. When the open-request cap refuses the raise after the human task was filed
   (5 open agent-raised requests per filer, 20 per workspace; the message
   says nothing was raised), nothing was written and no request id exists.
   Keep the filed task.
   - Record a `costs[]` entry with kind `wait`, signature
     `open-request-cap`, and `what` set to
     `open-request cap refused the raise on <human task id>; no decision request id`,
     plus `minutes`, `retries` and `slot_minutes` (use `0`, `1` and `0` when
     this wake did nothing else).
   - Copy that cost forward, with the same task id
     in `what`, on every later checkpoint until a raise on that task returns an
     id. The next wake uses step 2, including when no request id was recorded.
     Do not file a second human-crew task for the same question, and do not fold
     this question into a different open request.
8. A raise whose message says the target is not assigned to a human crew
   names the supported call: file a human-crew task and raise there. Do not
   retry the refused target. That call is the route the refusal names, not a
   second way around a grant refusal.

A refused raise, a warn that cites no returned id, or prose that a decision
is pending is not an answerable decision.

## 5. Record costs

Every workaround, manual step, retry or wait goes in the checkpoint's
`costs[]`: what you did, what it cost (minutes, retries, slot minutes), what
you wished existed, and a short kebab-case `signature` naming the gap when
one fits. `kind` is only `workaround`, `manual_step`, `retry` or `wait`.
There is no `capability` kind.

A cycle with no costs records an empty list.
Costs are how repeated pain becomes a proposal; a workaround you do not record is pain the system
cannot see. Something that broke or contradicted an assumption is also
friction (`orbit.friction` action `add`); a missing capability is a cost, not
friction — one event can be both.

Record a capability failure as kind `wait`. `what` is one line naming the
failure and, for a named refusal, its code and message:
`capability failure: no human crew configured`,
`capability failure: ambiguous human crew`, or
`capability failure: capability_denied; <message>`.

The open-request cap is
also kind `wait`, with signature `open-request-cap` and the human-crew task
id in `what`, as in [Escalate by class](#4-escalate-by-class). Carry that
cost forward until the raise on that task returns an id. Dropping it on a
later checkpoint leaves the next wake with no request id and no task id.

## 6. Status line and warns

Notices are how the operator sees a resident without opening its task. Post
them with `orbit.notice.post` in the mandate's own workspace; they need no
right, no linked task and no evidence.

**One status line, every cycle.** Before the checkpoint, post one `info`
notice with `dedupe_key` `resident:<mandate id, lowercase>:status`: a title of
at most 120 characters saying where the mandate stands now ("3 in review, 1
waiting on a merge", "All quiet"), and a body only when it adds something. The
post replaces the previous cycle's line in place (`outcome` `updated`); never
post a second info notice or a different key.

Name no expiry: Orbit sets it to
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
- When the ask is a choice between options, raise it with `orbit.decision`
  action `raise` as in step 4 and cite it in the warn's `evidence` as
  `{"type": "decision", "value": "<decision request id>"}` only when that
  raise returned the id, so the answer lands in task history. A warn with no
  returned id is not a pending decision. Do not post decision evidence for a
  cap refusal or any other raise that returned no id.
- A warn lasts 72 hours from its last post. Re-post one that still stands when
  its ask changes and at least once a day; one you stop re-posting lapses.
- Close it yourself with `orbit.notice.close` (`dedupe_key` or `id`, and a
  `reason`) in the cycle that sees the blocker cleared. You can close only
  notices this mandate's cycles posted. A warn whose `linked_task` reaches
  `done` closes itself.

The brief lists the notices in scope, your own included: check it for warns
you opened before posting or closing. The cap is 10 open non-status notices
per mandate; its mandatory status line is exempt from that count but still
counts toward the workspace-wide limit of 20 open notices. The rate remains
30 posts an hour per mandate. A refusal is a cost to record, not a reason to
drop the checkpoint.

## 7. Checkpoint

End every cycle with exactly one `orbit.resident.checkpoint` call carrying
the complete checkpoint record:

- your cycle and run ids,
- the wake reasons and
  digest from the brief,
- the authority you observed,
- `actions[]` with the
  ledger key and result of every mutating action,
- `decisions[]`,
- `requests[]` still open or raised,
- `costs[]`,
- `notes` (at most 600
  characters: only what changed this cycle and what is next),
- the next planned wake,
- continuity (`resumed`, `rebuilt` or `off`, carrying the brief's conversation
  reference forward when you resume), and
- one outcome:
  - `progress` — work advanced.
  - `idle` — you woke and there was nothing to do.
  - `waiting` — only open requests remain. A refused raise, a warn with no
    returned id, or an `open-request-cap` cost is not an open request, so none
    of those alone is `waiting`.
  - `handoff` — exit criteria met or deadline passed: stop dispatching, write
    the assignment's execution summary (what was done, what is open, which
    decisions are pending, which costs recurred), and checkpoint. Do not change
    the assignment's status: your run owns it, so that write is refused. The
    finalize step moves it to `review` and no further cycle runs. An exit
    criterion that names this move is met by the `handoff` checkpoint. Closing a
    mandate is the human's call.
  - `ended` — the mandate was stopped or taken over for good.

Keep standing state in structured fields such as `requests[]` and `decisions[]`,
carrying it forward without retelling it in prose. Also carry forward every
`open-request-cap` wait cost until that human-crew task's raise returns an id:
the task id is in that cost's `what` when no request id exists.

Put operator asks in
`requests[]` and a warn notice, never a "FOR DANIEL" paragraph in notes. Notes are the cycle's delta, not a recurring status report.
The brief has no older-notes tail; read task state when you need older detail.

A checkpoint for a stale cycle, or a second checkpoint for the same cycle,
is refused without writes. If you cannot checkpoint, the finalize step
synthesizes an `incomplete` checkpoint from the ledger, so the next cycle
recovers rather than repeats — but a synthesized checkpoint carries no
decisions, costs or notes, so write your own whenever you can.
