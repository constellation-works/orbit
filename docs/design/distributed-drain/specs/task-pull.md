---
type: design
summary: Spec for idempotent owner-side task admission, request receipts, execution claims, and lifecycle invariants.
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

`orbit.task.pull` creates at most one claim for an intended admission. The owner selects the first
ready, valid, conflict-free task in its canonical order and atomically records the reservation,
attempt identity, `in-progress` transition, history, and request receipt. Retries replay the same
receipt. All contracts below are proposed v1 behavior, not existing implementation guarantees.

## Why This Exists

One authoritative admission transaction lets multiple executing hosts share work without
independently scheduling it. Durable request identity handles lost responses; claim identity
separates an execution attempt from later retries of the same task. Neither requires tracking
host capacity, heartbeats, or automatic reassignment.

## The ready queue

The queue is a logical owner-side query, not a required maintained table:

- **Members:** `backlog` tasks whose every dependency is `done`. After epic retirement, the `epic`
  tag and parent/child hierarchy introduce no special admission path. Sequencing uses dependencies.
- **Order:** the canonical automatic-dispatch comparator, including corrective tag bands, priority,
  age, and task-ID tie-breaker. Readiness reporting and admission share it.
- **Validation:** selection, dependency checks, current status, canonicalized own `context_files`,
  and conflicts are checked within the admission transaction. Cached projections cannot authorize
  admission. Ordinary task and reservation mutations must participate in the same serialization.
- **Invalid entries:** dangling/rejected/archived dependencies or invalid/empty lock surfaces are
  excluded with diagnostics. They do not prevent unrelated valid work from being admitted.

V1 has no caller-selected crew or platform filter. Each participant must meet all workspace
execution requirements and resolve configured crews equivalently. This is a v1 restriction;
future owner-evaluated eligibility can preserve the same ordering authority.

## Class and routing

Only the owner serves this `control_plane` tool. A caller must have the workspace's `agent`
capability. `agent_invoke` is not needed because execution starts locally. Workspace selection
uses the host-qualified selector; authenticated caller identity supplies `machine_id`, never a
payload assertion. Owner-local drains use the same logical admission contract.

## Input

| Field | Type | Meaning |
|---|---|---|
| `workspace` | selector | Host-qualified owner/workspace selector |
| `request_id` | string | Durable unique ID for one intended admission; reused unchanged after uncertainty |
| `caller_version` | string | Caller binary version |
| `caller_schema` | integer | Caller orchestration schema version |
| `run_context` | object | Calling drain's `run_id`, `job_name`, and diagnostic `host_id` |

The caller persists the request before sending it. One drain run uses many request IDs. There is
no count, slot declaration, crew filter, or caller scan bound. Completion authorization is resolved
from durable owner-side grants; the input does not grant merge rights.

## Idempotency and admission

1. Validate selector, current caller authorization, version/schema, and required input. Refuse a
   remote caller for a local-only ship workspace. These checks also apply to receipt replay.
2. Begin the owner store transaction. Look up the receipt by workspace, authenticated machine, and
   request ID. An existing ID with different input yields `request_mismatch`; identical input
   returns its original outcome without new admission, history, or reservation.
3. For a new request, select from current ready tasks in canonical order. Exclude invalid entries
   and report diagnostics. Skip candidates conflicting with status-derived locks of `in-progress`
   or `review` tasks or active reservations; record `deferred_conflicts`.
4. For the first valid non-conflicting task, allocate an immutable claim ID. Reserve its own
   canonical footprint with the gate TTL default, record its execution machine and drain context,
   transition `backlog → in-progress`, append history, and persist the response receipt atomically.
   No local leaf run, branch, or worktree is created by this transaction.
5. If none is eligible, persist an `idle` receipt with diagnostics. This changes receipt state but
   creates no task transition, claim, or reservation. A later poll must use a new request ID.

The receipt stores owner-resolved ship configuration as of admission. Replays do not silently
change the execution contract. Exact request retries may return the same task repeatedly; only
one admission occurred. Full receipts may be compacted to non-reusable tombstones, in which case
replay returns `request_expired`, never a new task. Unsettled claims retain their receipts.

A receipt is historical evidence, not current execution authority. Replay includes current claim
phase separately. A revoked or settled claim is never reactivated; local launch requires an
idempotent owner-side binding check and execution mutations require the current claim.

## Output

| Field | Meaning |
|---|---|
| `request_id` | ID of the admission request |
| `task` | Task summary: ID, title, complexity, crew, context selectors; absent for idle |
| `claim` | `claim_id`, `reservation_id`, `reservation_expires_at`, authenticated execution machine; absent for idle |
| `claim_state` | Current phase at response time, separate from the stored admission receipt |
| `ship` | Owner-resolved mode, base/landing branches, completion policy and optional durable authorization reference |
| `deferred_conflicts[]` | Conflict exclusions with blocking tasks/reservations and selectors |
| `invalid_candidates[]` | Invalid dependency or lock-surface exclusions with reasons |
| `idle` | No claim created by this request |
| `queue_depth` | Remaining ready entries at original admission, diagnostic only |

Task contents needed for execution are read from the owner; the summary is not a replica store.
Queue depth and returned claim state are snapshots, not authorization for later writes.

## Refusals

Authorization and compatibility are checked before reading/replaying caller receipts. The remaining
checks run in the order described above.

| Error | When |
|---|---|
| `unknown_selector` | Selector cannot resolve to the named owner workspace |
| `capability_refused` | Destination is a replica or caller lacks required authority |
| `version_mismatch` | Caller binary/schema differs from owner |
| `invalid_input` | Required request or drain context is missing or malformed |
| `ship_mode_unsupported` | A remote caller targets a local-only ship workspace |
| `request_mismatch` | Existing request ID is reused with different input |
| `request_expired` | An old request is represented only by a non-reusable tombstone |

An atomic commit failure returns no successful admission response; the caller retries the same
request because transport uncertainty cannot establish whether the transaction committed.
`idle` is success. Invalid tasks are diagnostics, not a queue-wide refusal.

## Claim lifecycle contract

The companion lifecycle mutations must exist before pull is enabled. Their public tool names and
store schema are implementation choices; their atomic behavior is required:

| Operation | Required owner behavior |
|---|---|
| Bind execution | Validate claim and machine; bind one host-qualified leaf run idempotently; move `claimed → running` |
| Execution mutation | Check current claim, machine/run, and phase within the write transaction; deduplicate repeated mutation IDs |
| Accept handoff | Persist evidence and completion-authority reference, promote to review, close execution writes, release only this reservation atomically |
| Fail/cancel | Persist failure evidence, block the task, invalidate execution authority, release only this reservation atomically |
| Deliberate recovery | Reconcile any uncertain landing intent; revoke old claim, invalidate pending handoff, release reservation, and apply an authorized task transition atomically |

`stale_claim` rejects obsolete attempt mutations even when the task has since returned to
`in-progress`. Replay of a previously committed mutation may return its recorded outcome without
performing it again. All worker write routes carry claim context; generic task tools cannot bypass
these checks. Status/run/footprint edits affecting an active claim must preserve it or use recovery.

The reservation TTL is not a claim lease. Expiry does not authorize another worker, revoke a
claim, or remove status-derived task locks. Manual inspection/recovery is required when no worker
settles the claim. See [2_design.md §3.1](../2_design.md#31-attempt-ownership-and-recovery).

## Invariants

- At most one current execution claim exists per task, and a request ID never creates two claims.
- Transactional admission never admits an unsatisfied dependency or overlapping protected footprint.
- The owner alone orders work; only invalid or conflicting candidates are skipped in v1.
- A refusal creates no claim. Idle persists only its receipt and diagnostics.
- The execution machine is authenticated; host labels are not authority.
- Reassignment invalidates former attempt writes and landing authority before a new admission.
- Reservation cleanup can affect only the reservation associated with the settling claim.
- Owner and follower drains use the same admission boundary; legacy local admission cannot bypass it.
- Pull and task promotion do not grant merge authority.

## Required failure coverage

Implement the [validation matrix](../2_design.md#8-required-validation-scenarios), including lost
pull responses, concurrent mutations, duplicate local dispatch, reassignment with a returning
worker, settlement during partitions, and merge-intent reconciliation. Contract review is not a
substitute for these tests.

## Agent Signature

claude authored the initial contract under [ORB-12488]; codex revised it after design review,
2026-09-18. The feature remains Draft.
