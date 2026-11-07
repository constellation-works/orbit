---
type: design
summary: "Spec: resident cost events, the evolution record kind (separate from friction), and the manager digest; the scheduled evolution pass was removed in ORB-22844"
last_validated: 2026-11-02
title: Spec — Evolution ledger
owner: claude
status: Draft
feature: resident-orchestrator
tags: [resident-orchestrator, spec, evolution]
related_features: [resident-orchestrator, routines, project-learnings]
related_artifacts: [ORB-21229, ORB-21248, ORB-21249]
last_updated: 2026-11-07
---

# Spec: Evolution ledger

Residents record what their work cost them as **cost events** in every checkpoint. Events that name
a capability gap carry a **signature** and corroborate one **evolution record** per signature per
workspace. A **manager digest** surfaces the top pains to the human. The scheduled **evolution
pass** that ranked records and drafted `proposed` design tasks was removed in [ORB-22844];
nothing files evolution proposal tasks automatically now. Nothing in the ledger builds, promotes or approves anything; deciding
what the system becomes stays with the human.

## Why This Exists

When orchestration ran in chats, the human felt the pain of every workaround and turned the
recurring ones into features (`orbit attention`, fleet notices). A resident works around gaps
silently. Without a ledger, abstraction removes the signal that drives Orbit's evolution, and
friction cannot carry it, because friction is shaped for repairs
([4_decisions.md](../4_decisions.md#evolution-records-are-separate-from-friction)).

## Cost event

One entry in a checkpoint's `costs[]`
([checkpoint-contract](./checkpoint-contract.md#residentcheckpoint-v1)):

| Field | Rule |
|---|---|
| `kind` | `workaround` (did X instead of the proper path), `manual_step` (did by hand what could be automatic), `retry` (repeated an action that should have worked), `wait` (idle on something that should not block) |
| `what` | One line: what the cycle did |
| `minutes` | Estimated agent or wall minutes spent, integer ≥ 0 |
| `retries` | Count, ≥ 0 |
| `slot_minutes` | Drain-slot minutes consumed by the workaround (reruns, rescues), ≥ 0 |
| `wished` | One line: what capability would have removed it; required for `workaround` and `manual_step` |
| `signature` | Optional kebab-case key, 3–64 characters. The cycle reuses an open record's signature when one fits (`orbit.evolution.list` is in its brief) |

Interactive sessions (probes, takeovers) record the same shape with `orbit.evolution.add`, so the
ledger is not resident-only.

## Evolution record

A new record kind, stored by the workspace owner beside friction but not as friction.

```json
{
  "id": "EV-…",
  "workspace": "orbit-office",
  "signature": "manual-pilot-hold-clear",
  "title": "Pilot holds cleared by hand after the cause task lands",
  "wished": "Pilot re-checks a hold when its cause task lands",
  "state": "open",
  "occurrences": [{"at": "…", "mandate": "ORB-21300", "cycle": "rcy-…", "kind": "manual_step", "minutes": 4, "slot_minutes": 0}],
  "proposal_task": null,
  "declined_until": null
}
```

- **Dedupe.** One non-terminal record per `(workspace, signature)`. A new event with that signature
  appends an occurrence; the first event creates the record from its `what`/`wished`.
- **Derived on read.** Occurrence count, distinct cycles, distinct mandates, summed minutes and slot
  minutes over a window.
- **States.** `open` → `proposed` (the pass filed a task) → `resolved` (that task is `done`) or
  `declined` (that task was rejected; `declined_until` = now + `decline_suppress_days`). A declined
  record keeps collecting occurrences, and returns to `open` early if its summed cost in the window
  reaches twice the cost at decline.
- **Not friction.** Not listed by `orbit friction list`, not resolvable by a `resolves` relation,
  not counted as `friction_untriaged`.
- **Tools.** `orbit.evolution.add` (record an event outside a checkpoint), `orbit.evolution.list`
  (`--workspace`, `--state`, `--since`, sorted by summed cost), `orbit.evolution.show`. CLI
  `orbit evolution add|list|show`. No delete; an operator may set `declined`.

### Implemented ledger boundary

The owner stores evolution records and all cost occurrences in separate SQLite tables. A
checkpoint's comment remains its recovery source. Both `orbit.resident.checkpoint` and a
`resident.checkpoint` written through `orbit.task.update` ingest every cost after the comment
commits. The key `(mandate, cycle, cost index)` makes ingestion replay idempotent, even for
unsigned events. `OrbitRuntime::reconcile_evolution_checkpoint` repairs an interrupted ingestion explicitly.
Refused checkpoint submissions leave both the assignment and the ledger unchanged.
Signature keys are validated as normalized kebab-case; invalid keys are refused rather than
silently grouped under a different key. Concurrent writers corroborate a signature under one
immediate transaction and a unique non-terminal `(workspace, signature)` index.

The interactive tool takes `{event, mandate?, cycle?}`; `event` is the cost shape above. The CLI
uses the same tool:

```sh
orbit evolution add --event '{"kind":"manual_step","what":"Checked CI by hand","minutes":4,"retries":0,"slot_minutes":0,"wished":"Automatic CI recheck","signature":"manual-ci-recheck"}'
orbit evolution list --state open --since 2026-11-01T00:00:00Z --json
orbit evolution show <evolution-id> --json
orbit evolution decline <evolution-id> --days 30
```

Add returns `{event, record}` (`record: null` for unsigned costs). List returns `{count, records}`,
sorted by minutes plus slot minutes; each record includes `occurrence_count`, `distinct_cycles`,
`distinct_mandates`, `cycle_count`, `mandate_count`, `minutes`, `retries` and `slot_minutes`.
List's `since` bounds aggregates without dropping the retained occurrence history. Omitting it
returns all-time aggregates. The global `--workspace` selector binds the owner and workspace as
for other commands. These three MCP tools belong to the opt-in `resident` toolset.

Decline is operator-only and is not advertised over MCP. It captures the current window cost
(minutes plus slot minutes) as `cost_at_decline`, with the default 14-day window and 30-day
suppression. Zero cost must gain positive cost to reopen early. Reads derive resolution from a done proposal and decline
from a rejected proposal's actual transition time; subsequent cost writes materialize that state.
A resolved signature's next event starts a new record. No `resolves` relation handles evolution.
Recording a cost never files a task.

## Evolution pass (removed)

[ORB-21249] shipped a seeded, default-disabled weekly routine, `evolution-pass-<workspace>`
(Mondays 05:17 host-local), running `evolution_pass_pipeline`: a deterministic `evolution_rank`
step kept the top `max_proposals` open records meeting every `[resident.evolution]` threshold, a
read-only `evolution_draft` agent drafted proposals, and a deterministic `evolution_apply` step
filed them as `proposed` tasks tagged `evolution` and `evolution-record:<record-id>` with an
`evolution-evidence.json` artifact. [ORB-22844] removed the routine, the job and its three
activities. The thresholds `min_occurrences`, `min_distinct_cycles`, `min_distinct_mandates`,
`min_cost_minutes` and `max_proposals` load with a deprecation warning and are ignored until
2026-12-07. `window_days` (14) and `decline_suppress_days` (30) still apply: the digest's default
window and a decline's measured window, and the suppression a rejected proposal starts. Proposal
tasks the pass filed stay linked to their records.

## Manager digest

`orbit.resident.digest` / `orbit resident digest [--workspace …] [--since …] [--top N] --json`, derived on read (the default window is `resident.evolution.window_days`):

- residents: state, last outcome, open requests, stalled flag;
- open escalations, oldest first;
- reversible resident decisions in the window that the human can still veto;
- top N (default 5) evolution records by summed cost, with occurrence and mandate counts;
- open evolution proposals (filed by the removed pass) awaiting the human's `proposed → backlog` decision;
- the metrics headline ([2_design.md §9](../2_design.md#9-metrics-that-gate-widening)).

JSON keys are `residents`, `open_escalations`, `decisions_awaiting_veto`, `top_pains`,
`open_proposals`, `metrics`, `workspace` and `since`. Stalled means an active mandate has had no
checkpoint (or, before its first checkpoint, no checkpoint since task creation) for twice its
heartbeat, or its last three checkpoints were all `incomplete`; paused and taken-over mandates are
not marked stalled. Veto candidates are reversible checkpoint decisions in the window without
an answer naming them in `reverses`. Top pains exclude resolved records and include retained
occurrences plus occurrence/cycle/mandate counts; cost is minutes plus slot minutes. Open proposals
are linked or pass-tagged tasks still `proposed`, including duplicates linked to pre-existing tasks.
`--since` accepts a span or RFC 3339 instant; `--top` accepts 1–100, default 5.
The manager shows it at session start, weekly, or on request. A consumer hook may inject it.

## Agent Signature

Designed by claude (claude-opus-5-5) for [ORB-21229]; built by [ORB-21248] and [ORB-21249].

## Task References

- [ORB-21229] — designed the evolution loop
- [ORB-21248] — implements cost events and the evolution record kind
- [ORB-21249] — implements the evolution pass and the manager digest
- [ORB-22844] — removed the evolution pass

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
