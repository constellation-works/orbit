---
type: design
summary: "Spec: how a manager delegates, relays, asks for status and controls residents — orbit.resident.grant, relay, control, show/list, the opt-in resident toolset, and the attention kind residents report through"
last_validated: 2026-11-02
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
`orbit.decision.*`, `orbit.notice.*`, `orbit attention`) and adds four resident tools:
`orbit.resident.grant`, `orbit.resident.relay`, `orbit.resident.control` and the read pair
`orbit.resident.show` / `orbit.resident.list`. They are advertised on MCP only with the opt-in
`--toolset resident`. Residents answer through checkpoints, decision requests and attention items;
there is no mailbox in either direction.

## Why This Exists

The human talks to one session. That session must be able to hand work off, pass on what the human
said without paraphrase, and stop things, while being unable to do anything the human did not
authorise. Plain task comments could carry all of it, but they cannot refuse a relay that smuggles
in authority, cannot tell a relay from a note, and cannot stop a resident from firing. The tools
below are thin, governed wrappers that close exactly those gaps.

## Composition

| Manager action | Calls |
|---|---|
| File a mandate | `orbit.task.add` (assignment, tag `resident`), `orbit.task.artifact.put` (`resident/mandate.json`) |
| Grant authority | `orbit.resident.grant` |
| Relay a decision or direction | `orbit.resident.relay` |
| Relay an answer to an escalation | The human answers the decision request (`orbit decision answer`, dashboard, email). If the human answered in the manager chat, the manager relays it with `request_id` and the human still confirms the decision request; the relay alone is a direction |
| Status | `orbit.resident.show`; if stale, `orbit.resident.control` `wake` with `status_request: true` |
| Pause, resume, stop, take over, release | `orbit.resident.control` |
| Broadcast | `orbit.notice.post` |
| Everything waiting on the human | `orbit.attention` |
| Digest | `orbit.resident.digest` ([evolution-ledger](./evolution-ledger.md#manager-digest)) |

## `orbit.resident.grant`

Input: `{task, add?: [right], remove?: [right], workspaces_add?: [ws], workspaces_drop?: [ws], quote: string, source?: {human, via, said_at}}`.
Output: `{applied: bool, authority, decision_request?: id}`. `applied` is true for an operator caller
or a pure narrowing. Otherwise a `resident_authority` decision request is returned
([mandate-and-authority](./mandate-and-authority.md#widening)). Mixing additions and removals in
one call applies the removals at once and requests the additions.

## `orbit.resident.relay`

Input: `{task, quote: string, source: {human, via, session?, said_at}, request_id?: string, reverses?: string}`.
Output: `{comment_id}`. Writes one `resident.answer` comment
([checkpoint-contract](./checkpoint-contract.md#residentanswer-v1)) and records `relayed_by` from
the caller. Refused: an empty or over-long quote (> 4,000 characters), input carrying rights,
workspaces, status or any field other than those listed, or a mandate in `ended` state. A relay is
a wake event.

## `orbit.resident.control`

Input: `{task, action: pause|resume|stop|wake|takeover|release, reason: string, status_request?: bool, by_session?: string}`.
Output: `{state, effective_at}`.

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

`show {task}` → mandate, effective authority with grant history, state, live cycle (if any), latest
checkpoint parsed, open requests, next due estimate, and the last five outcomes.
`list {workspace?, state?}` → one row per mandate: task, goal, state, last checkpoint time and
outcome, open requests, stalled flag. Both are read-only and federated like other reads.

## The `resident` toolset

`orbit mcp serve --toolset resident` advertises the tools above. The default toolset does not,
because a leaf agent has no use for them. A resident cycle's own grant includes
`orbit.resident.checkpoint` and `orbit.resident.show` for its own mandate regardless of toolset.

## What residents report through

| Signal | Where | Attention kind |
|---|---|---|
| Progress, decisions, costs | `resident.checkpoint` on the assignment | — |
| An escalation | decision request on a human-crew task, plus a `resident.request` index comment | `decision_open` |
| Handoff | assignment in `review` with execution summary | `review_waiting` |
| Stalled | no checkpoint for 2 × heartbeat, or three `incomplete` outcomes in a row | `resident_stalled` (new) |
| Something broken | friction, or an `incident` notice (agent class) | `friction_untriaged` |

## Agent Signature

Designed by claude (claude-opus-5-5) for [ORB-21229]; built by [ORB-21242] and [ORB-21245].

## Task References

- [ORB-21229] — designed the manager channel
- [ORB-21242] — implements `orbit.resident.grant`, `show` and `list`
- [ORB-21245] — implements `relay`, `control`, the `resident` toolset and `resident_stalled`

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
