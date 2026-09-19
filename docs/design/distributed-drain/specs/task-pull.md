---
type: design
summary: Spec for idempotent owner-side task admission, request receipts, execution claims, and lifecycle invariants.
last_validated: 2026-09-19
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
receipt. The internal store foundation now implements admission receipts, claims, replay,
and compaction, and [ORB-12495] exposed the owner's read-only half — the
[preflight probe](../2_design.md#41-read-only-admission-probe) and the
[receipt lookup](#read-only-receipt-reconciliation) below — on the managed MCP and registered CLI
surfaces. The public pull tool and the mutating lifecycle operations remain unavailable behind one
named gate (`orbit-core`'s `application::distributed`), so no configuration can reach them; the
contracts for those entry points below are still proposed v1 behavior.

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
  Missing filesystem targets are valid declarations, not grounds for pruning: retain canonical
  selectors for new files and symbols, and freeze the full footprint on the claim through review.
  All task context read/write and status-lock paths use this non-pruning rule. A truly empty
  declaration requires operator correction before admission; execution cannot expand its scope.

V1 has no caller-selected crew or platform filter. Each participant must meet all workspace
execution requirements and resolve configured crews equivalently. This is a v1 restriction;
future owner-evaluated eligibility can preserve the same ordering authority.

## Class and routing

Only the owner serves this `control_plane` tool. A caller must have the workspace's `agent`
capability. `agent_invoke` is not needed because execution starts locally. Workspace selection uses
the host-qualified selector. SSH login establishes owner access; session agent/operator capability
and caller-side managed-run restrictions remain. There is no destination callers file, key-bound
proof, forced-command acceptance requirement, or replacement identity registry. Trusted runtime
invocation context supplies attempt ownership; remote machine labels alone are attribution, not
credentials. Owner-local drains use trusted local
runtime identity and the same logical admission contract. V1 admits only `review_policy = none`;
reject `before-pr` and `after-landing` before creating a claim. The read-only preflight response is
defined in [design §4.1](../2_design.md#41-read-only-admission-probe).

## Input

| Field | Type | Meaning |
|---|---|---|
| `workspace` | selector | Host-qualified owner/workspace selector |
| `request_id` | string | Durable unique ID for one intended admission; reused unchanged after uncertainty |
| `caller_version` | string | Caller binary version |
| `caller_schema` | integer | Caller distributed-drain wire-protocol schema version |
| `caller_review_policy` | enum | Executor's effective review policy; only `none` is supported |
| `run_context` | object | Calling drain's `run_id`, `job_name`, and diagnostic `host_id` |

The caller persists the request before sending it. One drain run uses many request IDs. There is
no count, slot declaration, crew filter, or caller scan bound. Completion authorization is resolved
from durable owner-side grants; the input does not grant merge rights.

## Idempotency and admission

1. Apply pre-admission refusals in the table order below: selector, current authorization,
   trusted invocation context, input shape, version/schema, ship mode, then review policy. Check both owner
   policy and the executor's declared `caller_review_policy`; neither may differ from `none`.
   These checks also apply to pull receipt replay; the separate read-only receipt lookup below
   is for reconciliation across configuration/upgrades.
2. Begin the owner store transaction. Its substrate is the task/reservation commit boundary
   [ORB-12528]: `with_admission` covers the readiness reads and `commit_task_transition`
   publishes the transition, history, reservation, and dependent coordination rows as one
   durable decision ([design pattern](../../../design-patterns/task_commit_boundary.md)).
   Receipts and claims are its dependent rows; their schema and replay rules are defined here,
   not by the boundary. Look up the receipt by workspace, runtime machine namespace, and
   request ID. An existing ID with different input yields `request_mismatch`; identical input
   returns its original outcome without new admission, history, or reservation.
3. For a new request, select from current ready tasks in canonical order. Exclude invalid entries
   and report diagnostics. Skip candidates conflicting with status-derived locks of `in-progress`
   or `review` tasks or active reservations; record `deferred_conflicts`.
4. For the first valid non-conflicting task, allocate an immutable claim ID. Reserve its own
   canonical non-pruned footprint with an explicit default TTL of 14,400 seconds (four hours),
   record its execution machine and drain context, transition `backlog → in-progress`, append a
   `pulled_by { machine_id, run_context, claim_id, request_id }` history record, and persist the
   response receipt atomically. The pulled path passes this TTL explicitly rather than inheriting
   `reserve_with_index`'s 1,800-second fallback.
   No local leaf run, branch, or worktree is created by this transaction.
5. If none is eligible, persist an `idle` receipt with diagnostics. This changes receipt state but
   creates no task transition, claim, or reservation. End this refill pass on the first idle
   response, sleep for the configured poll interval, and use a new request ID for the next poll.

The receipt stores owner-resolved ship configuration as of admission. Replays do not silently change
the execution contract. Exact request retries may return the same task repeatedly; only one
admission occurred. Full receipts may be compacted to non-reusable tombstones, in which case replay
returns `request_expired`, never a new task. Unsettled claims retain their receipts. Tombstones are
permanent in v1; neither random IDs nor age permit safe deletion. Expose counts and bytes, and
document the storage cost of idle polls. Bounded retention is deferred until a protocol can reject
retired request namespaces without retaining every individual ID.

A receipt is historical evidence, not current execution authority. Replay includes current claim
phase separately. A revoked or settled claim is never reactivated; local launch requires an
idempotent owner-side binding check and execution mutations require the current claim.

## Read-only receipt reconciliation

Add an owner-served lookup keyed by workspace, original caller machine, and request ID
(`orbit.drain.receipt.lookup`, live since [ORB-12495]). It returns
`found` (original receipt and current claim state), `expired` (tombstone), or `not_found` from an
owner transaction. It never creates a receipt, binds a run, or grants execution authority. The
original input remains immutable; a client upgraded to the owner's binary can look up an old request
without changing `caller_version` inside it. The lookup uses receipt schema `1`, versioned independently of admission, and
does not reapply original binary parity, ship mode, or review policy admission checks.

Current session capability is mandatory. Trusted worker invocations retain their original receipt
namespace — the machine their session is trusted to speak for, never a machine named in tool input
— and owner operators may inspect across attempts without a retired cross-caller ACL. Naming
another machine's namespace from a worker session is refused; reading one's own namespace returns
`not_found`, so a forwarded label buys nothing.
Claim revocation fences execution independently of SSH access. On an incompatible lookup protocol,
use owner claim inspection and deliberate recovery. `not_found` is not proof that an earlier transport request cannot still arrive: retry only
the original request ID while it remains admissible, or quiesce old sends and reconcile on the owner
before replacement. A found claim cannot launch if its saved ship/policy contract is incompatible
with the current executor; preserve it for explicit recovery rather than rewriting it.

## Output

| Field | Meaning |
|---|---|
| `request_id` | ID of the admission request |
| `task` | Task summary: ID, title, complexity, crew, context selectors; absent for idle |
| `claim` | `claim_id`, `reservation_id`, `reservation_expires_at`, runtime execution machine; absent for idle |
| `claim_state` | Current phase at response time, separate from the stored admission receipt |
| `ship` | Owner-resolved mode, base/landing branches, `review_policy: none`, completion policy and optional durable authorization reference |
| `deferred_conflicts[]` | Conflict exclusions with blocking tasks/reservations and selectors |
| `invalid_candidates[]` | Invalid dependency or lock-surface exclusions with reasons |
| `idle` | No claim created by this request |
| `queue_depth` | Remaining ready entries at original admission, diagnostic only |

Task contents needed for execution are read from the owner; the summary is not a replica store.
Queue depth and returned claim state are snapshots, not authorization for later writes.

## Refusals

Authorization and compatibility are checked before reading/replaying caller receipts. The remaining
checks run in the following table order; a malformed or absent version field is `invalid_input`, not
a compatibility comparison. Policy is rechecked at binding without rewriting the receipt.

Selector resolution and session capability belong to the calling surface, because only it knows
which workspace was addressed and what the destination served this session; the rest is one ordered
store-side ladder (`orbit_store::admission_refusal`) that admission and the read-only probe both
read, so a preflight cannot report a verdict admission would not reach.

| Error | When |
|---|---|
| `unknown_selector` | Selector cannot resolve to the named owner workspace |
| `capability_refused` | Destination is a replica or caller lacks required authority |
| `invalid_input` | Required request, version/policy declaration, or drain context is missing or malformed |
| `version_mismatch` | Caller binary/schema differs from owner |
| `ship_mode_unsupported` | A remote caller targets a local-only ship workspace |
| `review_policy_unsupported` | Owner/executor review policy is not `none` |
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
| Bind execution | Validate claim, machine, captured policy and mode; bind one host-qualified leaf run idempotently; move `claimed → running`; generic resume may not replace this run |
| Execution mutation | Check current claim, machine/run, and phase within the write transaction; deduplicate repeated mutation IDs |
| Accept handoff | Persist candidate/base SHAs, validation evidence, typed `{ policy: none, disposition: not_required }`, and any completion-authority reference; promote to review, close execution writes, release only this reservation atomically; authorized acceptance also records the landing-start request |
| Approve handoff | Owner operator only: deduplicate mutation ID, verify current review handoff and exact candidate/base, persist scoped authorization with approver/revocation state, and record landing-start request atomically; agent access cannot approve |
| Revoke completion authorization | Owner operator only: invalidate pending landing permission atomically; reconcile any uncertain merge intent before reassignment |
| Fail/cancel | Persist failure evidence, block the task, invalidate execution authority, release only this reservation atomically |
| Deliberate recovery | Reconcile any uncertain landing intent; revoke old claim, invalidate pending handoff, release reservation, and apply an authorized task transition atomically |

`stale_claim` rejects obsolete attempt mutations even when the task has since returned to
`in-progress`. Replay of a previously committed mutation may return its recorded outcome without
performing it again. All worker write routes carry claim context; generic task tools cannot bypass
these checks. Status/run/footprint edits affecting an active claim must preserve it or use recovery.
Interrupted claimed leaves cannot use generic `orbit job resume`, which creates a different run.
Deliberate recovery revokes the old claim before a new attempt; branch evidence may be reused but
validation/handoff must be fresh. Owner-local mode uses a local-candidate handoff and authorized
local merge evidence; it must not enter the PR pipeline or require an origin. See [design
§3](../2_design.md#3-pull-mode-drain-and-the-pulled-leaf-pipeline) for both dispatch variants.

The reservation TTL is not a claim lease. Expiry does not authorize another worker, revoke a claim,
or remove the frozen non-pruned footprint used by status-derived task locks. Manual
inspection/recovery is required when no worker settles the claim. See [2_design.md
§3.1](../2_design.md#31-attempt-ownership-and-recovery).

## Invariants

- At most one current execution claim exists per task, and a request ID never creates two claims.
- Transactional admission never admits an unsatisfied dependency or overlapping protected footprint.
- The owner alone orders work; only invalid or conflicting candidates are skipped in v1.
- A refusal creates no claim. Idle persists only its receipt and diagnostics.
- Trusted invocation context fences execution machine and bound run; host labels confer no rights.
- Reassignment invalidates former attempt writes and landing authority before a new admission.
- Reservation cleanup can affect only the reservation associated with the settling claim.
- Owner and follower drains use the same admission boundary; legacy local admission cannot bypass it.
- Pull and task promotion do not grant merge authority; explicit handoff approval does.
- `none` is the only review policy in v1; review status does not imply an automated review.
- Existing ship-sweep routines, wrapper, CLI, and owner-local drains cannot bypass this admission
  contract; queued/bound leaf runs and unrepresented admissions consume capacity exactly once.

## Required failure coverage

Implement the [validation matrix](../2_design.md#8-required-validation-scenarios), including lost
pull responses, concurrent mutations, duplicate local dispatch, reassignment with a returning
worker, settlement during partitions, and merge-intent reconciliation. Contract review is not a
substitute for these tests.

## Agent Signature

claude authored the initial contract under [ORB-12488]; codex revised it after design review,
2026-09-18; claude reconciled it with the authorization decision and the shipped read-only surface
under [ORB-12495], 2026-09-19. The feature remains Draft.

## Internal storage accounting

`TaskCommitBoundary::admission_storage_usage` reports full-receipt and permanent-tombstone
counts and logical UTF-8 payload bytes. SQLite page and index overhead is additional. An
idle receipt can compact immediately; a claim in claimed, running, or handed-off phase
retains its full receipt. At a 30-second poll, one idle drain leaves 2,880 request identities
per day even after compaction. No age-based tombstone deletion is supported.

Execution provenance is optional on persisted records for backward reading. Run origin is
captured from the trusted runtime backend at insertion and is not updated by run upserts.
The shared run JSON projection exposes `executed_on`, and task show exposes
`job_run_host`; both use null when historical identity is unknown.
Artifact origin is supplied separately from artifact bytes and actor attribution. Task run
location must accompany a trusted run binding; changing a legacy/unqualified link does not
infer a location from a matching local run ID.

The internal API is `TaskStoreBackend::mutate_execution_claim` with non-deserializable
`ClaimInvocation` and typed `ClaimMutation`. Missing context fails closed. Mutation receipts are
scoped to immutable claim and mutation ID, compare the complete input, and replay their original
outcome without changing a newer attempt. `inspect_execution_claims` reports provenance, phase,
age, expiry, last event, unresolved intent and landing invalidation; it does not repair journals.
The internal typed handoff operation validates exact claim/run/repository/delivery/candidate/base
identity against trusted owner observations and digest-pinned owner validation artifacts. Its
journal decision includes review transition, closed execution phase, reservation release and, for
an applicable completion grant, immutable authorization plus a pending landing-start request.
Explicit operator review-state approval creates the same authorization/outbox pair idempotently;
agent capability cannot approve. Revocation cancels pending authority atomically, and unresolved
merge intent must first reconcile. The merge-intent write rechecks current evidence and authority,
including grant revocation within the SQLite transaction. Historical receipt replay does not grant
new execution or landing permission. The summary-only legacy handoff variant refuses new writes.

`OrbitRuntime::accept_task_handoff`, `approve_task_handoff`, `revoke_task_handoff`,
`accepted_task_handoff` and `landing_start_requests` are internal owner-domain seams, not registered
distributed tools. Trusted observations must come from provider/Git state and owner validation
policy. No-diff observations additionally require the existing already-landed Git checks; their
report shape, scope projection, criteria and log requirements are shared with the local verifier.
The durable pending outbox survives restart without an active drain or sweep; dispatch and actual
external merge reconciliation belong to the dependent consumer slice.
Generic tool/friction omitted-context fencing and transport propagation remain integration work;
public distributed entry points must stay disabled until that proof passes.

Journal intent schema 2 carries replayable evidence. The reader still accepts schema 1 intents;
older executors refuse schema 2 rather than silently applying a transition without its evidence.

### Worker coordination transport

Internal seeded-claim execution binds `WorkerInvocation` at runtime. Task/dependency reads and
coordination mutations route to its owner destination, never a follower-local fallback. SSH login
provides destination access; the claim transaction independently fences task, machine, bound run
and phase. Neither tool arguments nor editable job input can replace the invocation or elevate a
managed proxy to operator. Protected process and Linux PID-namespace bindings carry it through
subprocesses, detached workers and same-bound-run retries; missing required context refuses.

Generic task evidence/document updates and claim-scoped friction use the owner commit journal.
An omitted friction task inherits the bound task; conflicting arguments refuse. Friction allocation
and its deduplication receipt commit with the claim fence. Deliberate recovery prevents a late
attempt from publishing, including after reassignment. Identical accepted retries return the
recorded mutation result. Generic review transitions still require typed handoff acceptance;
executor-local Git checks are not forwarded as remote filesystem operations. Artifact bytes are
read locally, with origins and task run links derived from runtime claim provenance.

These internal seams do not enable pull, claims, recovery or approval public entry points.
`DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED` remains false. No schedules or live hosts change.
