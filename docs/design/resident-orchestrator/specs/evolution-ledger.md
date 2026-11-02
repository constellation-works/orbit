---
type: design
summary: "Spec: resident cost events, the evolution record kind (separate from friction), the scheduled evolution pass with recurrence and cost thresholds, and the manager digest"
last_validated: 2026-11-02
title: Spec — Evolution ledger
owner: claude
status: Draft
feature: resident-orchestrator
tags: [resident-orchestrator, spec, evolution]
related_features: [resident-orchestrator, routines, project-learnings]
related_artifacts: [ORB-21229, ORB-21248, ORB-21249]
last_updated: 2026-11-02
---

# Spec: Evolution ledger

Residents record what their work cost them as **cost events** in every checkpoint. Events that name
a capability gap carry a **signature** and corroborate one **evolution record** per signature per
workspace. A scheduled **evolution pass** ranks those records and, only above measured recurrence
and cost thresholds, drafts `proposed` design tasks with evidence. A **manager digest** surfaces
the top pains to the human. Nothing in the ledger builds, promotes or approves anything; deciding
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

## Evolution pass

Seeded, default-disabled routine `evolution-pass-<workspace>`, weekly (Mondays 05:17 host-local), job
`evolution_pass_pipeline`:

1. **`evolution_rank`** (deterministic). Selects `open` records whose window aggregates meet every
   threshold, ranks by summed minutes + slot minutes, and keeps the top `max_proposals`. Writes a
   ranking artifact on the job run. If nothing qualifies, it ends the job: no agent runs.
2. **`agent_loop`** (crew from config). For each kept record: searches open and recent tasks,
   friction and evolution proposals for duplicates; if none, files one `proposed` task in the owning
   workspace with tag `evolution`, a problem statement written from the occurrences, the `wished`
   capability as a starting shape, acceptance criteria, and an `evolution-evidence.json` artifact
   (record id, cycles, occurrences, costs). Links the record (`proposal_task`, state `proposed`).
   If a duplicate exists, links that task instead and adds the evidence as a comment.
3. The pass's tool grant allows reading, `orbit.task.add` (always `proposed`), comments, artifacts
   and evolution record updates. It does not allow status changes, dispatch or merges.

Thresholds (`[resident.evolution]`): `window_days` 14, `min_occurrences` 3, `min_distinct_cycles` 3,
`min_distinct_mandates` 1, `min_cost_minutes` 60, `max_proposals` 3, `decline_suppress_days` 30.
A record below any threshold is never proposed, however plausible the idea; that is the guard
against busywork.

## Manager digest

`orbit.resident.digest` / `orbit resident digest [--workspace …] [--since …]`, derived on read:

- residents: state, last outcome, open requests, stalled flag;
- open escalations, oldest first;
- reversible resident decisions in the window that the human can still veto;
- top N (default 5) evolution records by summed cost, with occurrence and mandate counts;
- open evolution-pass proposals awaiting the human's `proposed → backlog` decision;
- the metrics headline ([2_design.md §9](../2_design.md#9-metrics-that-gate-widening)).

The manager shows it at session start, weekly, or on request. A consumer hook may inject it.

## Agent Signature

Designed by claude (claude-opus-5-5) for [ORB-21229]; built by [ORB-21248] and [ORB-21249].

## Task References

- [ORB-21229] — designed the evolution loop
- [ORB-21248] — implements cost events and the evolution record kind
- [ORB-21249] — implements the evolution pass and the manager digest

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
