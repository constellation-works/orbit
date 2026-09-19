---
type: design
summary: "Spec: orbit.task.pull — pop the next conflict-free task from the owner-maintained ready queue"
last_validated: 2026-09-18
title: Spec — orbit.task.pull
owner: claude
status: Draft
feature: distributed-drain
tags: [distributed-drain, pull, queue, spec]
related_features: [distributed-drain, federated-mcp, host-registry]
related_artifacts: [ORB-12488]
---

# Spec: `orbit.task.pull`

`orbit.task.pull` hands the caller **one** task: the first entry of the owner's ready queue whose
lock footprint overlaps no held task lock, with that footprint reserved and the task moved to
`in-progress` in the same store transaction. Two concurrent callers are never handed the same
task. The queue's order is decided by the owner alone; the caller neither filters nor reorders it.

## Why This Exists

Without an owner-side queue, every host would recompute dependency readiness and priority order
from its own view of the backlog and race the others on admission. One queue, one pop, one
transaction is the smallest contract under which a second host can take work without becoming a
second control plane.

## The ready queue

The owner maintains one ordered queue per workspace. Membership and order are the owner's
existing readiness rules, not new ones:

- **Members:** `backlog` tasks whose every dependency is terminal-successful (`done`). Tags do not
  affect membership; `epic` is a size hint for crew selection, not a queue class.
- **Order:** the order `orbit run readiness` and `classify_workspace_auto_tasks` already produce —
  priority, then age, with the same tag-driven adjustments those paths apply today.
- **Maintenance:** the queue is a projection of the store, recomputed whenever a task's status,
  priority, dependencies, tags, or parent change. It is not a separately editable table; an
  operator changes the order by changing the tasks.

Crew is not a queue input in v1. A pulled task carries its own `crew` field if one was set; crew
selection for tasks without one is the follower's ordinary resolution, and auto-assignment by
complexity is future work alongside any crew-aware pulling
([3_vision.md](../3_vision.md#1-open-questions)).

## Class and routing

- Tool class: `control_plane`. A replica destination refuses it with `capability_refused`; only the
  owner checkout serves it.
- Callers reach it through federated MCP with the host-qualified selector. A caller must hold the
  `agent` capability for the workspace in the owner's callers file. `agent_invoke` is not required.

## Input

| Field | Type | Meaning |
|---|---|---|
| `workspace` | selector | host-qualified `hm_<owner>/ws_*`; required |
| `caller_version` | string | Orbit binary version of the caller |
| `caller_schema` | integer | caller's `ORCHESTRATION_SCHEMA_VERSION` |
| `run_context` | object | `run_id`, `job_name`, `host_id` of the caller's drain run, stamped onto the task history entry |

There is no count, no slot declaration, no crew filter, and no scan bound. A caller that wants
more than one task calls again.

## Pop

1. Walk the ready queue from the head.
2. Skip an entry whose lock footprint (its own canonicalized `context_files`) overlaps a lock
   held by an `in-progress` or `review` task or an active reservation.
   Each skip is recorded in `deferred_conflicts` with the blocking task ids and the overlapping
   selectors.
3. Refuse, rather than skip, an entry whose `blocked_by` target is archived, rejected, or dangling,
   with the same diagnostic `reserve_locks` emits today; such an entry should not be in the queue,
   and the refusal surfaces the inconsistency.
4. The first entry that passes is the result. In **one store transaction**: reserve its footprint
   with the gate TTL default, set `backlog → in-progress`, and append a history entry recording
   `pulled_by` with the caller `machine_id` and `run_context`.
5. An exhausted queue, or a queue whose every entry conflicts, returns `idle`.

Serialization is the store's existing cross-process transaction; concurrent pops observe each
other's commits and never admit an overlapping footprint.

## Output

| Field | Meaning |
|---|---|
| `task` | the pulled task summary (id, title, complexity, crew, context selectors), absent when `idle` |
| `ship` | owner-resolved ship inputs: `mode`, `base_branch`, `landing_branch`, `completion` |
| `deferred_conflicts[]` | skipped queue entries with reasons |
| `idle` | true when nothing was pulled |
| `queue_depth` | entries remaining in the queue after this pop, for the follower's sleep choice |

## Refusals

Ordered; the first that applies is returned.

| Error | When |
|---|---|
| `unknown_selector` | selector is not host-qualified |
| `capability_refused` | destination is not the owner checkout, or the caller lacks `agent` on the workspace |
| `version_mismatch` | `caller_version` or `caller_schema` differs from the owner's |
| `invalid_input` | missing `run_context` |
| `ship_mode_unsupported` | workspace `ship_mode` is `local` and the caller is not the owner |
| `queue_inconsistent` | the head entry's `blocked_by` target can never reach `done` (step 3) |

`idle` is a success, not a refusal.

## Invariants

- A task is returned by at most one successful pull, ever, unless it returns to `backlog` through
  an ordinary status transition.
- Pull never returns a task with an unsatisfied dependency.
- Pull never returns a task out of queue order except by skipping a conflicting entry.
- Pull writes nothing on `idle` or on a refusal.
- Pull does not create a run, a worktree, or a branch. Those are the caller's.
- The owner's own drain admits through pull. No other path moves a backlog task to `in-progress`
  on behalf of a drain.

## Agent Signature

claude, 2026-09-18, [ORB-12488].
