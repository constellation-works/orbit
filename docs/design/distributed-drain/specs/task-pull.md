---
type: design
summary: Spec for idempotent owner-side task admission, request receipts, execution claims, and lifecycle invariants.
last_validated: 2026-10-05
title: Spec — orbit.task.pull
owner: claude
status: Draft
feature: distributed-drain
tags: [distributed-drain, pull, queue, spec]
related_features: [distributed-drain, federated-mcp, host-registry]
related_artifacts: [ORB-12488, ORB-12616, ORB-12500, ORB-13625, ORB-13941, ORB-13992, ORB-13908, ORB-14149, ORB-14192]
---

# Spec: `orbit.task.pull`

`orbit.task.pull` creates at most one claim for an intended admission. The owner selects the first
ready, valid, conflict-free task in its canonical order and atomically records the reservation,
attempt identity, `in-progress` transition, history, and request receipt. Retries replay the same
receipt. The internal store foundation now implements admission receipts, claims, replay,
and compaction, and [ORB-12495] exposed the owner's read-only half — the
[preflight probe](../2_design.md#41-read-only-admission-probe) and the
[receipt lookup](#read-only-receipt-reconciliation) below — on the managed MCP and registered CLI
surfaces. [ORB-13625] registered the executor's mutating half — `orbit.task.pull`,
`orbit.drain.claim.bind` and `orbit.drain.claim.settle` — and opened the one named gate
(`orbit-core`'s `application::distributed`) that every mutating entry point still names, so the
feature is closed again only by a source change. Approval, revocation and recovery are not
registered tools.

## Why This Exists

One authoritative admission transaction lets multiple executing hosts share work without
independently scheduling it. Durable request identity handles lost responses; claim identity
separates an execution attempt from later retries of the same task. Neither requires tracking
host capacity, heartbeats, or automatic reassignment.

## The ready queue

The queue is a logical owner-side query, not a required maintained table:

- **Members:** `backlog` tasks whose every dependency is `done` (or archived after it reached
  `done`). After epic retirement, the `epic` tag and parent/child hierarchy introduce no special
  admission path. Sequencing uses dependencies.
- **Order:** the canonical automatic-dispatch comparator, including corrective tag bands, priority,
  age, and task-ID tie-breaker. Readiness reporting and admission share it.
- **Validation:** selection, dependency checks, current status, canonicalized own `context_files`,
  and conflicts are checked within the admission transaction. Cached projections cannot authorize
  admission. Ordinary task and reservation mutations must participate in the same serialization.
- **Invalid entries:** dangling/rejected dependencies, dependencies archived before reaching
  `done`, or invalid lock surfaces are excluded with diagnostics. An empty surface is not invalid: it is admitted without a context lock. They do not prevent unrelated valid work from being admitted.
  Missing filesystem targets are valid declarations, not grounds for pruning: retain canonical
  selectors for new files and symbols, and freeze the full footprint on the claim through review.
  All task context read/write and status-lock paths use this non-pruning rule. A truly empty
  declaration requires operator correction before admission; execution cannot expand its scope.

V1 has no platform filter. Each participant must meet all workspace execution requirements.
Crews are the one owner-evaluated eligibility rule [ORB-13941]: a request declares the crews its
executor can run (`crews`, below), and the owner skips a ready candidate whose crew the executor
cannot run — its own `task.crew`, or the executor's `default_crew` for a task naming none. The
skipped task keeps its place in the owner's order for the owner or another follower. The owner
still orders every admission; the declaration only narrows what this executor is offered.

## Class and routing

Only the owner serves this `control_plane` tool. A caller must have the workspace's `agent`
capability. `agent_invoke` is not needed because execution starts locally. Workspace selection uses
the host-qualified selector. SSH login establishes owner access; session agent/operator capability
and caller-side managed-run restrictions remain. There is no destination callers file, key-bound
proof, forced-command acceptance requirement, or replacement identity registry. Trusted runtime
invocation context supplies attempt ownership; remote machine labels alone are attribution, not
credentials. Owner-local drains use trusted local
runtime identity and the same logical admission contract. With the owner's `review.before_pr` on,
admission refuses, before creating a claim, an executor that does not declare `review_gate` and a
local ship mode, where no gate runs; the executor's own `caller_before_pr` never refuses
[ORB-13908]. After-landing review (the owner's `delivery-code-review` auto-task) never affects
admission [ORB-13992]. The read-only preflight response is
defined in [design §4.1](../2_design.md#41-read-only-admission-probe).

## Input

| Field | Type | Meaning |
|---|---|---|
| `workspace` | selector | Host-qualified owner/workspace selector |
| `request_id` | string | Durable unique ID for one intended admission; reused unchanged after uncertainty |
| `caller_version` | string | Caller binary version |
| `caller_schema` | integer | Caller distributed-drain wire-protocol schema version |
| `caller_fingerprint` | string, optional for historical requests | Type-derived request fingerprint, checked before request deserialization |
| `caller_before_pr` | bool | The `review.before_pr` the calling drain captured at submission; diagnostic only, since a claimed leaf runs the review the `ship` contract captures [ORB-13908] |
| `review_gate` | bool, optional | Whether the executor's claimed PR leaf runs the before-PR gate; an owner with `review.before_pr` on admits only an executor that declares it. Absent: `false` |
| `run_context` | object | Calling drain's `run_id`, `job_name`, and diagnostic `host_id` |
| `crews` | object, optional | Executor crew capability: `runnable` (crew names its window preflight found runnable; absent means unrestricted), `default_crew` (what a task naming no crew runs as there; absent admits no crew-less task) and `excluded` (`{crew, source, reason}` crews it will not run for the rest of its window). Absent: every crew is admissible |
| `os` | enum, optional | Executor host OS: `linux`, `macos` or `windows`. A task carrying `os:` tags is admitted only to an executor whose OS one of them names. Absent (an OS outside that set): only tasks without an `os:` tag are admissible |

The caller persists the request before sending it. One drain run uses many request IDs. There is
no count, slot declaration, or caller scan bound. The crew capability is part of the immutable
request, so a replay is judged by the capability it was first sent with, and so is the
declared OS. Protocol revision 2 adds `crews`, revision 4 adds `os`, and revision 5 replaces
`caller_review_policy` with `caller_before_pr` and the ship contract's `review_policy` with
`before_pr`: an older owner rejects the new field even though it is optional, and a revision-4
caller still sending `caller_review_policy` is answered `protocol_mismatch`. Revision 6 adds the
ship contract's `review` (below) and the typed handoff's before-PR evidence [ORB-13895]; revision 7
sends `review_gate`, which a revision-6 owner rejects as an unknown field [ORB-13908]. Revision 8
captures the owner's `required_validation_commands` in the before-PR `review` contract [ORB-14192].
An explicit empty list means no required checks; a missing legacy field is unknown authority,
never an admitted-empty list. Revision 9 adds the typed `NoDiff` claim delivery and clean-base settlement [ORB-14259].
Revision 10 adds the settlement's typed failure class, the `leaf_released` crew exclusion source
and the receipt's `resume_candidate` [ORB-14257].
The current protocol revision is 10. Before persisting a new request,
the follower negotiates the type-derived fingerprint as described below and
reports typed `protocol_skew` before sending any pull. Binary-version equality is insufficient
because wire changes can land between releases. Completion authorization is resolved
from durable owner-side grants; the input does not grant merge rights.

Followers must match the owner's pull request schema, independently of `orbit --version`.
The read-only probe reports `protocol_fingerprint`, a SHA-256 fingerprint of the JSON schema
derived from the running build's `AdmissionRequest` and all its nested types. The follower
first probes with legacy-compatible fields, checks that fingerprint and `protocol_schema`,
then declares `caller_fingerprint` on a second probe. A different or missing fingerprint,
including a legacy owner, refuses with typed `protocol_skew` before any `orbit.task.pull`.
A protocol identity containing `[REDACTED_ENV]` is a corrupted transport reply,
not evidence of schema skew. The follower treats it as typed `OwnerNegotiation`,
records a transient pass error and retries on the next pass. The owner also
checks redacted drain replies, including nested fingerprints, commit/tree IDs
and evidence hashes: read-only calls fail negotiation; mutating calls report
`OutcomeUnknown` so the follower reconciles or replays the same request.
Credentials remain scrubbed even when they overlap an identity. Known
`XDG_SESSION_*`, `DBUS_SESSION_BUS_ADDRESS`, `SESSION_MANAGER` and
`TERM_SESSION_ID` metadata is excluded from session-name matching; credential
words still take precedence, and other session names retain conservative
handling. Purely numeric environment values shorter than 12 digits are
excluded from substring substitution.
The integer revision remains for persisted requests and lifecycle semantics; request field
changes no longer depend on a manual bump. Deploy matching builds on both hosts and restart
long-lived processes.

`orbit run show <drain-run>` exposes a pull drain's latest pass error and consecutive failure
count. JSON carries `last_pass_error_code`, `last_pass_error`, `consecutive_pass_failures`, and `degraded` under
`pipeline_state.drain_last_pass`. Three consecutive failed passes latch a visible degraded
warning and stop new admissions for that drain. A successful pass before the threshold resets
the streak. Protocol skew on the current probe or a current-build request immediately latches
degradation and ends the drain **failed** with `protocol_skew`,
even with an open window. A refused retry carrying an obsolete persisted fingerprint (including
a request from before fingerprints) is reconciled through receipt lookup first. A found receipt
is carried forward; an absent or expired receipt closes the old record without failing the
drain, so a matching current probe can admit a fresh request in the same pass. Skew on a
current-build request remains fatal. `orbit doctor` reports the latest skewed pull drain, and the dashboard
keeps its pass health and failure code visible after it ends. Its durable admissions and settlement
records remain available to leaf workers, the settle-only pass, and the clock sweep. Other
degraded drains keep retrying settlements and outlive their window until nothing
is unsettled; successful settlement does not clear the warning. Fix the reported cause, run
`orbit run auto --stop` to close the window, and start a new drain once this one ends. An unreadable or unwritable run-state record fails the activity visibly.

A pull drain also records what its owner kept off this host. When a request is answered idle,
the receipt's diagnostics fill `drain_last_pass` as a local drain's classifier does: `queued` is
the receipt's `queue_depth`; `deferred` lists footprint holds (`context_lock_conflict`, the holder
in `blocked_by`) and other owner holds (`owner_hold`); `excluded` lists unmet dependencies
(`dependency_not_done`, the unfinished tasks in `blocked_by`), `os:` waits (`host_os_mismatch`) and
unrunnable crews (`crew_unavailable`), bounded to 20 with `excluded_total` the full count; and
`waiting_by_reason` counts every kept-off task by code. `waiting_recorded_at` dates the owner's
answer. A pass that sends no request (throttled, settlement held, breaker open, window closed,
owner unreachable), or whose requests all claim, keeps the previous diagnostics and their date
rather than recording an empty backlog. `consecutive_idle_passes` counts the idle answers in a row
that found tasks waiting; from three, `orbit run show` and the dashboard add an `idle:` line
saying how many tasks were kept off this host and why. Both print the same `Still waiting` lines
for a pull drain as for a local one.

## Idempotency and admission

1. Apply pre-admission refusals in the table order below: selector, current authorization,
   trusted invocation context, input shape, version/schema, ship mode, then before-PR review: an
   owner with `review.before_pr` on admits only a PR-mode request whose executor declares
   `review_gate`. The executor's `caller_before_pr` is not checked.
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
   The exclusive section stalls every task write on the host, so it costs one decision, not the
   partition [ORB-14724]. Candidates — the `backlog`, `in-progress` and `review` tasks and the
   statuses of the backlog's dependencies — are selected from the generated task index before
   the section, under the ordinary shared boundary, as are pilot operator-validation holds for
   every backlog task. Inside, the section reads the in-flight tasks and computes their
   footprints once, then re-reads a candidate the selection did not rule out, its dependencies
   and its pilot hold, and judges it again on that read. A candidate that changed after
   selection is deferred or skipped, never admitted from the selection. When selection could
   not prove the index fresh, the section lists every bundle for the in-flight tasks instead.
3. For a new request, select from current ready tasks in canonical order. Exclude invalid entries
   and report diagnostics. Skip candidates conflicting with status-derived locks of `in-progress`
   or `review` tasks or active reservations; record `deferred_conflicts`. Also defer a task a live
   owner-local delivery run carries in its `input.task_ids` (any job declaring
   `spec.task_delivery`, such as a local drain's wrapper and its gate). A gate waiting for
   context locks has not yet moved the task out of `backlog` or reserved its footprint, so
   status and reservations alone would hand it out a second time [ORB-13918]. Skip a candidate
   whose crew the request's `crews` says the executor cannot run and record it in
   `crew_unavailable` [ORB-13941]. A malformed capability (a blank crew name) is `invalid_input`.
   Also skip, into `crew_unavailable`, a task the requesting machine's same drain run
   (`run_context.run_id`) released for an `environment`, `transient`, `owner_route` or `provider`
   failure, and every task when that run released one for an `environment` or `owner_route`
   failure — the host itself is suppressed for the window; another drain may take them
   [ORB-14257]. An admitted task carries `resume_candidate`, the candidate the owner kept from
   the task's last claim, unless its spec changed or an operator discarded it since. Before the crew check, skip a candidate whose `os:` tags name no OS the request's `os`
   declares, and record it in `os_unavailable` with the wait (`waits for a macos host
   (os:macos); the executor runs linux`). An `os:*` tag outside the reserved namespace, which
   task writes reject but an older stored task may carry, is satisfied by no executor.
   `no-diff-expected` tasks are claimable by a remote executor: its claimed leaf hands off
   `NoDiff` with a verified report, files findings on the owner through the claimed-owner
   broker and opens no PR [ORB-14474]. Admission defers one to the owner, recording the reason
   in `deferred_conflicts`, only for a caller below revision 9, the first with the NoDiff
   handoff. The schema check refuses such a caller first, so the deferral guards a relaxed check.
   The tag itself supplies no verified report; the leaf must still write one.
   `context_files` are optional: an otherwise eligible backlog task with empty context is
   admitted on this pass with an empty footprint and holds no context lock. A live pilot
   preparation checkpoint still defers its tasks until that run settles. Undeclared edit
   conflicts are handled by rebase and conflict repair at landing.
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
| `ship` | Owner-resolved mode, base/landing branches, `before_pr`, completion policy, optional durable authorization reference and, only when `before_pr` is on, the captured `review` contract (`contract_version`, `crew`, `budget`, `required_validation_commands`, and `baseline_commands` when the owner lists any) |
| `deferred_conflicts[]` | Conflict exclusions with blocking tasks/reservations and selectors; `blocked_by` names the holder when known |
| `crew_unavailable[]` | Ready candidates skipped because the executor cannot run their crew, with the reason; omitted when empty |
| `os_unavailable[]` | Ready candidates skipped because their `os:` tags name no OS the executor runs, with the wait; omitted when empty |
| `invalid_candidates[]` | Invalid dependency or lock-surface exclusions with reasons; `blocked_by` names the unfinished dependencies |
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
| `version_mismatch` | Caller binary version differs from owner |
| `protocol_skew` | Caller and owner request fingerprints differ (or the owner predates fingerprints); refused before pull, with both fingerprints in the diagnosis |
| `protocol_mismatch` | Legacy probe report for differing integer revisions; current followers surface typed `protocol_skew` |
| `ship_mode_unsupported` | A remote caller targets a local-only ship workspace |
| `before_pr_unsupported` | Owner has `review.before_pr` on and the executor does not declare `review_gate`, or the ship mode is local (stored receipts may spell it `review_policy_unsupported`) |
| `request_mismatch` | Existing request ID is reused with different input |
| `request_expired` | An old request is represented only by a non-reusable tombstone |
| `ship_contract_mismatch` | A *new* request carries a ship contract other than the one the owner resolves now; replays keep their stored contract |
| `stale_claim` | Bind or settle names a claim this owner workspace does not hold |

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
| Accept handoff | Persist candidate/base SHAs, validation evidence, the typed review disposition the claim's contract requires (`not_required`, or verified before-PR evidence whose certificate the owner records), and any completion-authority reference; promote to review, close execution writes, release only this reservation atomically; authorized acceptance also records the landing-start request |
| Approve handoff | Owner operator only: deduplicate mutation ID, verify current review handoff and exact candidate/base, persist scoped authorization with approver/revocation state, and record landing-start request atomically; agent access cannot approve |
| Revoke completion authorization | Owner operator only: invalidate pending landing permission atomically; reconcile any uncertain merge intent before reassignment |
| Fail | Persist failure evidence and its typed `candidate` or `task_input` class (or an `operator_cancel` whose operator asked to block the task), keep the failure's committed candidate for the next claim, block the task, invalidate execution authority, release only this reservation atomically |
| Release | Executor gives back unfinished work it did not fail: never launched, or a launched leaf whose typed failure class does not block (`operator_cancel`, `provider` including a model at capacity, `environment`, `owner_route`, `baseline_red`, `transient`, `base_conflict`). Revoke the claim, return the task to `backlog`, release only this reservation atomically. A blocking class, or any typed failure class after two such releases of the task within 24 hours, blocks the task instead, with one comment listing every counted reason |
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
- A task executes once at a time: pull admission skips a task a live owner-local delivery run
  carries, and a local gate whose task came under a live claim while it waited skips dispatch as
  a `claimed_elsewhere` no-op.
- Transactional admission never admits an unsatisfied dependency or overlapping protected footprint.
- The owner alone orders work; only invalid or conflicting candidates, and candidates whose crew
  the executor declares it cannot run, are skipped in v1.
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
under [ORB-12495], 2026-09-19; claude recorded the executable owner-local claimed leaf under
[ORB-12616], 2026-09-20; claude recorded the owner's published-delivery acceptance and the retained
entry points' shared admission decision under [ORB-12500], 2026-09-20. claude reconciled the stale status wording under the live follower drain, 2026-09-29. claude added
executor crew capability and provider-unavailable release under [ORB-13941], 2026-10-04. The feature remains Draft while it is live (the same status as [2_design.md](../2_design.md)).

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

A claim whose ship contract captured a before-PR `review` must hand off typed before-PR evidence:
verdict, reviewed head and base SHAs, the reviewer's fix commit when it made one, reviewer crew and
run, and digest-pinned certificate and reviewer artifacts. Acceptance re-reads the certificate from
the owner task bundle and refuses, with a typed reason, missing or unexpected evidence
(`review_evidence_missing`, `review_evidence_unexpected`), a verdict that does not pass
(`review_not_passed`), a reviewed head or reviewer commit other than the handed-off candidate
(`reviewed_head_mismatch`), a reviewed base the owner's Git does not find under the candidate base
(`reviewed_base_not_ancestor`), a certificate that disagrees with the evidence, candidate, task or
repository (`review_certificate_mismatch`), and a crew, certificate schema or required-command list
other than the captured contract's (`review_contract_mismatch`). A legacy review contract without
`required_validation_commands` is also refused with fresh-claim guidance. Acceptance requires the
owner's current required-command list to equal the captured list: a change since admission refuses
the handoff as `review_contract_mismatch` instead of rewriting the claim. Approval and landing
recheck the pinned evidence. An
accepted certificate is written to the owner's review store, so after-landing coverage excludes the
reviewed tree instead of reviewing it again. A claim without a captured `review` refuses before-PR
evidence.

The claimed PR leaf produces that evidence itself [ORB-13908]. The pull store creates the leaf with
a review admission seeded from the claim's captured `review`, never from the follower's settings,
and the leaf runs `review_gate_admit` → `review` → `review_gate_settle` between base
synchronization and push. The gate reads the claimed task through the worker binding, keeps its
attempt ledger in the follower's review store keyed to the claim, and sends the manifest, the
reviewer's report, the certificate and the verdict comment to the owner task as claim evidence;
the claim footprint does not widen until acceptance. `claim_handoff` carries the settled
evidence. A non-passing verdict fails the leaf before push, and the failure settlement blocks the
task. A follower that cannot run the captured reviewer crew requests no claim
(`before_pr_reviewer_unavailable`).

`OrbitRuntime::accept_task_handoff`, `approve_task_handoff`, `revoke_task_handoff`,
`accepted_task_handoff` and `landing_start_requests` are internal owner-domain seams, not registered
distributed tools. Trusted observations must come from provider/Git state and owner validation
policy. No-diff observations additionally require the existing already-landed Git checks; their
report shape, scope projection, criteria and log requirements are shared with the local verifier.
The durable pending outbox survives restart without an active drain or sweep.
Generic tool/friction omitted-context fencing and transport propagation are enforced on the owner
(a claim-scoped write that omits its claim context is refused), and the one source switch,
`DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED`, is now `true`.

Journal intent schema 2 carries replayable evidence. The reader still accepts schema 1 intents;
older executors refuse schema 2 rather than silently applying a transition without its evidence.

### Owner landing consumer

Recording completion authority dispatches the owner-local `task_landing_pipeline` from the outbox in
the same call, so authorized work lands with no drain, ship sweep, routine or schedule running.
Handoff identity keys one durable landing attempt and the dispatch key: a merged handoff refuses
re-dispatch, a live owner job is not dispatched twice, and a pending request whose job never started
or whose job is terminal is recovered as the next attempt by an explicit dispatch pass.
`OrbitRuntime::dispatch_landing_requests`, `land_handoff` and `landing_attempts` are internal
owner-domain seams alongside the handoff ones; landing a named handoff is how a stopped attempt is
retried and an uncertain one reconciled.

| Operation | Required owner behavior |
|---|---|
| Dispatch landing | Operator context, current unrevoked authorization and no landing invalidation; open exactly one attempt per handoff and attach only the job that owns it |
| Publish merge intent | Recorded durably before the external call, after rechecking authority, candidate observation and pinned validation evidence |
| Reconcile merge intent | Resolve against the provider's actual state or the owner-local target ref; an unresolved intent blocks completion, revocation, recovery and reassignment |
| Complete landing | Requires an open attempt, a resolved intent, current authority and verified merge evidence; moves `review -> done`, settles the attempt and the outbox request atomically |
| Stop landing | Durable evidence for changed identity, conflict, refused protection or an exhausted check budget; the task stays in review and a repair needs fresh validation and a new handoff |

The landing step observes the pull request, the owner checkout's landing ref or the no-diff covering
commit itself; it reuses the pinned `pr_complete` delivery identity and merged-with-merge-commit
evidence without a follower run or path, checks the head on every poll, resolves candidate and base
objects locally, and requires the validated base to remain reachable from the landing ref. An
observation that is not the accepted candidate is refused before any external merge. Owner-local
candidates fast-forward the local landing branch and are verified from the ref, after a direct
landing intent naming the handoff's task is retained; no-diff delivery makes no external call and still requires typed evidence and completion authority. The owner never
rebases unvalidated code and no administrative bypass exists.

### Worker coordination transport

Internal seeded-claim execution binds `WorkerInvocation` at runtime. Task/dependency reads and
coordination mutations route to its owner destination, never a follower-local fallback. SSH login
provides destination access; the claim transaction independently fences task, machine, bound run
and phase. Neither tool arguments nor editable job input can replace the invocation or elevate a
managed proxy to operator. Protected process and Linux PID-namespace bindings carry it through
subprocesses, detached workers and same-bound-run retries; missing required context refuses.
Binding resolution is identity discovery, not authorization: a `/proc` entry the caller cannot
read leaves the process unbound, so an unrelated host process still opens a runtime on a machine
that holds binding rows, and only a child that requires a worker context refuses.

Generic task evidence/document updates and claim-scoped friction use the owner commit journal.
An omitted friction task inherits the bound task; conflicting arguments refuse. Friction allocation
and its deduplication receipt commit with the claim fence. Deliberate recovery prevents a late
attempt from publishing, including after reassignment. Identical accepted retries return the
recorded mutation result. Generic review transitions still require typed handoff acceptance;
executor-local Git checks are not forwarded as remote filesystem operations. Artifact bytes are
read locally, with origins and task run links derived from runtime claim provenance.

These internal seams carry no public entry point of their own; the registered executor lifecycle
([ORB-13625]) reaches them. Recovery and approval stay owner-operator actions. No schedules
change.

### Caller checkpoint status

The job-store caller checkpoint preserves request identity, unique leaf
binding, launch uncertainty and disconnected settlement. Its refill loop is the
follower's pull drain: `orbit run auto --pull <selector>` runs it as the
`workspace_pull_pipeline` job against the registered `orbit.task.pull`,
`orbit.drain.claim.bind` and `orbit.drain.claim.settle` tools (see
[ORB-13625] below). The owner exposes no other pull endpoint.

[ORB-12616] made the owner-local half executable. A claim's leaf is one of two
internal handoff-only definitions chosen by the owner-resolved ship mode —
`task_claimed_pr_pipeline` or `task_claimed_local_pipeline`. Both bypass
rediscovery and reservation, carry no completion input, contain no merge or
completion step, and end at the typed handoff. The owner-local one publishes
nothing at all, so the no-origin scenario needs no remote and no PR
credentials. Its refill loop now runs against real adapters: admission on the
owner's commit boundary, binding and settlement on the owner's claim journal,
and the leaf launched through the existing worker supervisor under the trusted
process binding.

The owner declares what a claim must pass in `workflow.required_validation_commands`.
With before-PR review on, admission freezes that list in `ship.review.required_validation_commands`;
the claimed leaf inherits it into its review admission, manifest and certificate. Every captured
command needs a required passing review record. Review settlement uses this owner-admitted
snapshot, never a later config value or the follower's own list. An explicit `[]` is a known
no-check contract; an absent legacy field cannot establish the validation contract and requires
a fresh claim under the current protocol.

Admission also freezes the owner's `review.baseline_commands` in
`ship.review.baseline_commands` [ORB-14684], omitted when empty. The leaf's settlement reruns only
these and the required commands to check a red-base claim, and refuses a failure of either filed as
a `diagnostic`. The certificate records the list, and acceptance refuses a certificate whose list
differs from the captured one as `review_contract_mismatch`. A missing field reads as no baseline
commands, so an older contract or certificate means what it meant when it was written; a leaf
that does not carry the list cannot deliver a before-PR claim from an owner that lists any.

The separate deterministic candidate validation still reads the executor's current
`workflow.required_validation_commands`, and the owner verifies its exact-run, exact-head logs
against the owner's current list at acceptance. Configure the executor to supply that evidence;
its local config cannot replace the captured review requirements. For a before-PR claim, acceptance
also requires the owner's current list and the certificate's list to match the admitted snapshot,
so owner-policy drift fails closed with fresh-claim guidance. An explicit empty required list runs
no candidate-validation command and permits a handoff without validation logs; the other handoff checks still
apply. Each captured log is attached to the owner's task as a digest-pinned artifact. The typed
handoff is written as the claim's durable pending settlement before any owner call, so a disconnect
leaves one immutable settlement to retry.

[ORB-12500] delivered the owner's acceptance of a *published pull request*:
the owner reads the pull request from the provider, pins the reported head
branch, base branch and head commit against the submitted candidate, refuses a
closed-without-merge or self-contradictory state, and resolves the candidate
and base objects in its own checkout under the same tree-identity and ancestry
rules the executor applied. Accepted revisions go through one shared rule that
handoff acceptance and the landing attempt both call.

A claimed leaf that verifies a clean base instead delivers `NoDiff`, with a digest-pinned
clean-tree verifier checkpoint (`verified_no_diff` or `verified_already_landed`) and captured
required validation. The sandboxed implementer writes the report and every declared log beneath `.orbit/tmp/`
and returns `no_diff_artifacts` entries with artifact `path` and scratch `source_path`.
Commit confines and bounds those reads, imports only the report and its declared logs
through the claim, and reuses the existing no-diff/already-landed verifier; a tag or skip
flag alone refuses this route. Both claimed leaves support it, and the PR leaf skips branch
preparation, rebase, push and PR creation. No before-PR reviewer runs because no PR exists.
The owner resolves its live base independently, requires candidate and tested HEAD to equal
that base, and rechecks the report, its underlying evidence and logs at acceptance and completion.
An already-landed checkpoint also retains scope, criteria and covering-commit ancestry/marker checks,
including a covering sibling task. The executor's branch need not exist on the owner.
`NoDiff` completes through the same authorized `review → done` landing boundary without an
external merge. A review-only completion contract still waits for owner approval, and a moved
base or changed evidence refuses completion. The legacy `AlreadyLanded` delivery variant still
reads persisted handoffs; claimed leaves use `NoDiff` for this case.

[ORB-13625] delivered the follower half. The owner's `orbit.task.pull` input is
the caller's durable `AdmissionRequest` (request ID, caller version and schema,
review policy, run context, and the ship contract its probe reported); it
answers `{receipt, claim_state}`. A follower drain also sends its window's crew capability as `crews` [ORB-13941].
`orbit.drain.claim.bind` takes `claim_id`,
`run_id` and the receipt's `ship`; `orbit.drain.claim.settle` takes `claim_id`,
an optional `run_id`, and the executor's durable settlement (`AcceptHandoff` or
`Fail`, nothing else). Each resolves the caller machine from the trusted
session and replays under a per-claim mutation ID (`pull-bind:`, `pull-fail:`,
`pull-handoff:`). The follower's `RoutedPullPeer` calls them over the federated
transport, and `orbit run auto --pull <selector>` runs the refill loop as the
`workspace_pull_pipeline` job. A local request the owner refused and holds no
receipt for closes as `Refused` (a new terminal phase that releases its slot);
see the caller-side implementation status in design §3.
