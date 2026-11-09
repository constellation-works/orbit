---
type: design
summary: "Spec: how a manager delegates, relays, asks for status and controls residents — orbit.resident.grant, relay, control, show/list, the opt-in resident toolset, and the attention kind residents report through"
last_validated: 2026-11-05
title: Spec — Manager channel
owner: claude
status: Draft
feature: resident-orchestrator
tags: [resident-orchestrator, spec, mcp]
related_features: [resident-orchestrator, operator-attention, fleet-notices, human-decision-channel]
related_artifacts: [ORB-21229, ORB-21242, ORB-21245]
last_updated: 2026-11-02
---

# Spec: Manager channel

The manager reaches residents only through Orbit records. It composes existing tools (task writes,
`orbit.decision.*`, `orbit.notice.*`, `orbit attention`) with resident operations for granting
authority, relaying words, controlling cycles, inspecting mandates (`show`, `list`, `digest`,
`metrics`) and writing checkpoints. They are advertised on MCP only with the opt-in
`--toolset resident`. Residents answer through checkpoints and decision requests, with attention
items surfacing open work; there is no mailbox in either direction.

## Why This Exists

The human talks to one session. That session must be able to hand work off, pass on what the human
said without paraphrase, and stop things, while being unable to do anything the human did not
authorise. Plain task comments could carry all of it, but they cannot refuse a relay that smuggles
in authority, cannot tell a relay from a note, and cannot stop a resident from firing. The tools
below are thin, governed wrappers that close exactly those gaps.

## Composition

| Manager action | Calls |
|---|---|
| File a mandate | `orbit.task.add` (assignment, tag `resident`), `orbit.task.artifact` action `put` (`resident/mandate.json`) |
| Grant authority | `orbit.resident.grant` |
| Relay a decision or direction | `orbit.resident.relay` |
| Relay an answer to an escalation | The human answers the decision request (`orbit decision answer`, dashboard, email). If the human answered in the manager chat, the manager relays it with `request_id` and the human still confirms the decision request; the relay alone is a direction |
| Status | `orbit.resident.show`; if stale, `orbit.resident.control` `wake` with `status_request: true` |
| Pause, resume, stop, take over, release | `orbit.resident.control` |
| Broadcast | `orbit.notice.post` |
| Everything waiting on the human | `orbit.attention` |
| Digest | `orbit.resident.digest` ([evolution-ledger](./evolution-ledger.md#manager-digest)) |

## `orbit.resident.grant`

Input (as built in [ORB-21242]): `{task_id, add?: [right], workspaces?: [ws], remove?: [right],
drop_workspaces?: [ws], deadline?: rfc3339, quote?: string, orchestrator?, model?}`, at least one
change. `quote` is required for a widening from any caller but the operator; the relaying session
(crew, model, session id) is taken from the caller, not from input.
Output: `{outcome, authority, request?, deduplicated}`, with `outcome` `granted` (an operator
widening, applied), `requested` (a `resident_authority` decision request was raised or an open one
for the same addition reused; nothing changed,
[mandate-and-authority](./mandate-and-authority.md#widening)), `narrowed` or `unchanged`. One call
may not both widen and narrow: it is refused, so a caller narrows first and then asks to widen.
CLI: `orbit resident grant <task> --add … --add-workspace … --remove … --drop-workspace … --deadline …
--quote …`.

## `orbit.resident.relay`

Input: `{task_id, quote: string, source: {human, via, session?, said_at}, request_id?: string, reverses?: string}`
plus the attribution fields `orchestrator?` and `model?`.
Output: `{task_id, comment: {index, at, by}, answer}`: task comments carry no ids, so the written
comment is named by its 1-based position, and `answer` is the record as written. Writes one
`resident.answer` comment ([checkpoint-contract](./checkpoint-contract.md#residentanswer-v1)) and
records `relayed_by` from the caller (`crew` from the session's orchestrator or agent, `model`
from its model). Refused: an empty or over-long quote (> 4,000 characters), input carrying rights,
workspaces, status or any field other than those listed (the operation refuses unknown fields,
and `source` refuses unknown keys), a quote that would need redaction (refused, never rewritten,
so the words stay verbatim), or a mandate in `ended` state. A relay is a wake event: the answer
comment is in the wake digest. CLI: `orbit resident relay <task> --quote … --source '<json>'
[--request-id …] [--reverses …]`.

## `orbit.resident.control`

Input: `{task_id, action: pause|resume|stop|wake|takeover|release, reason: string, status_request?: bool, by_session?: string}`
plus `orchestrator?` and `model?`.
Output: `{task_id, action, state, effective_at, by}`, and for `stop` a `handoff` of `review`,
`cycle_live` (finalize hands off), `already` or `refused: <why>` (the lifecycle refused the move;
the operator moves the assignment). An open assignment, including `proposed`, is walked through
`in-progress` to `review`. `status_request` applies only to
`wake`. CLI: `orbit resident control <task> <action> --reason … [--status-request] [--by-session …]`.

Each action appends a `resident_control` history entry whose note is
`{action, state, reason, by, status_request?, by_session?}`; the last entry's `state` is the
control state. A mandate's reported state is `paused` or `taken_over` from it, and `ended` after
`stop`. `resume`, `wake` and `release` written after the last cycle start add the wake reason
`wake` (and `status_request`) to the due check, consumed by the next cycle; the cadence floor
still applies. The brief carries `status_request: true` for such a cycle, and finalize
synthesizes an `incomplete` checkpoint if the agent wrote none, so a status request always
leaves a checkpoint. An unreadable control entry makes the mandate `invalid`, never active.

Pause and stop prevent a new cycle from starting. A live cycle keeps its frozen grant, completes,
checkpoints (or receives a synthesized checkpoint from `resident_finalize`), and exits at the next
cycle boundary, within at most one agent budget.

| Action | From state | To state | Effect |
|---|---|---|---|
| `pause` | `active` | `paused` | No new cycle starts; a live cycle follows the boundary semantics above. |
| `resume` | `paused` | `active` | Due on the next tick |
| `stop` | any but `ended` | `ended` | No new cycle starts; a live cycle follows the boundary semantics above, then the assignment moves to `review` with a synthesized handoff. |
| `wake` | `active` | `active` | Due on the next tick, ignoring the heartbeat (not the floor); `status_request` makes the cycle checkpoint even when idle |
| `takeover` | `active`, `paused` | `taken_over` | No fires; records who took over |
| `release` | `taken_over` | `active` | Due on the next tick; the cycle reads takeover-period comments as new |

Who may call: any caller that can write the assignment, because every action is narrowing or
neutral. `resume` and `release` restore firing under the existing authority; they cannot widen it.
Each action is a history entry with the caller and reason.

## `orbit.resident.show` / `orbit.resident.list`

`show {task_id}` reports the mandate artifact (or why it is missing or invalid), effective authority
and its grant/narrowing history, control and state, open grant requests, live cycle, latest parsed
checkpoint and stalled status. `list {include_ended?}` returns `{count, mandates}` with each row's
task, title, status, state and reason, goal/deadline, effective rights/workspaces, authority revision,
open request count and stalled flag. Both are read-only and scoped to the bound workspace.

[ORB-21245] adds `control` (state and last action), the `paused` and `taken_over` mandate states,
and `stalled` with `stalled_reason` to `show`; it adds `stalled` to each `list` row. Slice 1
([ORB-21242]) shipped: `show {task_id}` returns the task, `state` (`active`, `handed_off`, `ended` or
`invalid` with `state_reason`), the parsed mandate (or
`mandate_error`), the effective authority, the folded authority record with `granted[]` and
`narrowed[]` (or `authority_error`), and open grant requests. `list {include_ended?}` returns
`{count, mandates}` rows with state, goal, deadline, effective rights and workspaces, authority
revision and open request count. Later slices added the latest checkpoint and live cycle to `show`,
plus stalled status to `show` and `list`; those current fields are summarized above.

## The `resident` toolset

`orbit mcp serve --toolset resident` advertises the tools above; [ORB-21242] introduced it with
`grant`, `show` and `list`, [ORB-21245] added `relay` and `control`, and the `manager` and
`resident` MCP roles include it. A resident cycle's guard refuses `grant`, `relay` and `control`:
a cycle never speaks for the human or changes its own firing. The default
toolset does not,
because a leaf agent has no use for them. A resident cycle's own grant includes
`orbit.resident.checkpoint` and `orbit.resident.show` for its own mandate regardless of toolset.

## What residents report through

| Signal | Where | Attention kind |
|---|---|---|
| Progress, decisions, costs | `resident.checkpoint` on the assignment | — |
| An escalation | decision request on a human-crew task, plus a `resident.request` index comment | `decision_open` |
| Handoff | assignment in `review` with execution summary | `review_waiting` |
| Stalled | an `active` mandate with no checkpoint for 2 × heartbeat (since creation when it never checkpointed), or three `incomplete` outcomes in a row, or its workspace's cycle routine disabled — no cycle can fire at all then, however recently it checkpointed | `resident_stalled` |
| Something broken | friction, or an `incident` notice (agent class) | `friction_untriaged` |

## Agent Signature

Designed by claude (claude-opus-5-5) for [ORB-21229]; built by [ORB-21242] and [ORB-21245].

## Task References

- [ORB-21229] — designed the manager channel
- [ORB-21242] — implements `orbit.resident.grant`, `show` and `list`
- [ORB-21245] — implements `relay`, `control`, the `resident` toolset and `resident_stalled`

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
