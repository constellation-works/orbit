---
title: Distributed Drain — Design
owner: claude
last_updated: 2026-09-18
last_validated: 2026-09-18
status: Draft
feature: distributed-drain
doc_role: design
type: design
summary: "One owner, multiple execution hosts: idempotent claims, routed authority, manual recovery, explicit landing, retained ship sweep, none-only review, and non-pruning context footprints."
tags: [distributed-drain, multi-host, pull, federated-mcp]
paths: ["crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml", "crates/orbit-core/assets/jobs/task_pr_pipeline.yaml", "crates/orbit-core/assets/activities/classify_workspace_auto_tasks.yaml", "crates/orbit-core/src/runtime/task/locks.rs", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-mcp/**"]
related_features: [distributed-drain, federated-mcp, host-registry, resident-orchestrator, activity-job, policy-sandbox]
related_artifacts: [ORB-12488]
---

# Distributed Drain — Design

> **Status: Draft, proposed.** This is the target contract; no section is live. Each mechanism
> names the existing code it extends so the implementation tasks can be filed against real anchors.

This doc covers the v1 shape: one owner checkout, any number of replica checkouts on other hosts,
each replica running the drain in pull mode against the owner, and the existing
machinery the shape retires. It deliberately leaves to [3_vision.md](./3_vision.md): a
cloud-offloaded owner store, crew auto-assignment, and follower-side merge.

## 1. Roles: one owner, N followers

Roles are the host-registry catalog roles, unchanged. The owner checkout has no checkout-level
`owner_machine_id` and its logical owner equals the local `machine_id`; a follower is a replica
checkout whose `owner_machine_id` names the owner machine. `RegisteredRuntimeFactory` already
carries the replica owner into Core's coordination-write guard
(`crates/orbit-cmd/src/registry_runtime.rs::replica_owner_for_checkout`), and
`automation/ownership.rs` already refuses delivery automation on a replica. The distributed drain
adds no role, no fleet table, and no host list.

The precondition this design imposes on the operator is **one control plane per repository**. The
current state — `ws_orbit` independently initialized as an owner on both hosts, minting `ORB-` on
one and `DANI-` on the other — must be collapsed first: the follower host re-registers its checkout
as a replica of the owner machine. Tasks minted on the demoted host are moved with task-migration
export/import or left to drain on that host before the switch. The federated mux cannot detect the
collision (spec: *single control-plane per repository is operator configuration*), so this is a
runbook step, not a code path.

## 2. The ready queue and `orbit.task.pull`

The **ready queue** is a logical owner-side query over `backlog` tasks whose dependencies are
all `done`, ordered by the existing automatic-dispatch comparator, including corrective tags and
its task-ID tie-breaker. It need not be a maintained table or cache. If implementation introduces
a projection, it is advisory until admission revalidates it against authoritative state.
Followers never compute dependency readiness or choose priority order.

`orbit.task.pull` is an owner-only `control_plane` tool. Its full contract is in
[specs/task-pull.md](./specs/task-pull.md). The owner serializes candidate selection, readiness and
footprint validation, reservation, claim creation, `backlog → in-progress`, history, and the
request result in one store transaction. Checking candidates before the transaction and only
serializing the final writes is insufficient. Task updates and other reservation/admission paths
must share that serialization boundary. The current reservation store transaction is an anchor,
not an existing transaction spanning all those records; implementing this boundary is v1 work.

A caller durably allocates a `request_id` before each intended pull. Its scope is the owner
workspace and authenticated caller machine. A retry with the same input returns the stored result,
never another task. The drain run ID is context, not an idempotency key: one drain makes many
legitimate pulls. Store successful `idle` results too; a later poll uses a new request ID. Refusals
do not create a claim. Request receipts must not be deleted in a way that permits an old ID to
become a new admission; retain a tombstone if full response retention is compacted. Stop a refill
pass after its first idle response rather than polling once per remaining slot. V1 retains compact
tombstones indefinitely: random IDs alone do not make deletion safe. At a 30-second idle poll this
is 2,880 receipts per drain per day before compaction, independent of its free-slot count. Expose
receipt/tombstone counts and bytes for storage planning; bounded retention requires a future
protocol that rejects retired request namespaces, not a time-based DELETE. Unsettled claims retain
full receipts. This storage cost is accepted explicitly for v1.

The response includes the task, resolved ship inputs, and a **claim handle**: `claim_id`,
`reservation_id`, reservation expiry, and authenticated execution machine. A claim identifies
one attempt, including the interval before a leaf run exists. Owner and follower drains use the
same path. Request receipts and claim state are durable coordination data, not a fleet registry.

There is no epic path after the retirement in [§7](#7-retirements-and-retained-ship-sweep). A task
tagged `epic` is an ordinary entry using its own canonicalized `context_files`; hierarchy does not
implicitly order execution. Required sequencing must be expressed with dependencies. Empty or
invalid lock surfaces are reported as ineligible rather than silently treated as a claim protecting
no files.

Remove filesystem-existence pruning from task context normalization/read projections and all
admission, reservation, and status-lock calculations used by this workspace. Canonicalize selector
syntax and enforce repository boundaries, but preserve valid selectors for not-yet-created files and
symbols. `allow_missing_context` still controls explicit operator existence checks; it must never
cause a stored selector to disappear later. This replaced the former
`locks.rs::existing_envelope_context_files_at_root` pruning behavior and the equivalent task read
paths with the shared non-pruning calculation
`runtime/task/mod.rs::declared_context_files` [ORB-12490]. Missing is not invalid. Freeze the full canonical footprint on the claim and use it through
execution and review, including after reservation expiry; current checkout contents cannot shrink
it. Unclaimed legacy status locks use the same non-pruning canonicalization. A truly empty declared
surface remains ineligible until an operator supplies context before admission; diagnostics name
that remedy. Restore previously pruned declarations from authoritative task history where possible
(`application/task/context_repair.rs`, reached by `orbit task lint --restore-pruned`), or report
them for operator repair; do not guess their intended scope.

V1 has no crew or platform filter in pull. Participating hosts must be able to execute every task
eligible for the workspace, including its configured crews and required toolchains. This is a
restrictive deployment prerequisite, not a claim that eligibility filtering would create a second
scheduler. Heterogeneous task eligibility is deferred. Explicit task crews and the workspace's
fallback crew configuration must resolve equivalently on each participant.

## 3. Pull-mode drain and the pulled leaf pipeline

`orbit run auto --pull <selector>` binds a local replica checkout to the owner's host-qualified
selector copied from federated discovery. Verify that the local checkout belongs to that logical
workspace and repository. Persist the owner machine, workspace identity, selector, and claim in run
inputs; detached children and in-run step retries inherit them. A renamed or unavailable destination
must not fall back to a local coordination store.

The drain retains its window, sleep controls, and detached execution model, but admission changes:

1. Reconcile pending local pull requests and claimed-but-not-launched work before requesting more.
2. Count live leaf runs **and pending admissions not yet represented by a live run** against local
   capacity. Count bound `task_pr_pipeline` and owner-local `task_local_pipeline` runs, not just
   `workspace_auto.rs::LEAF_JOB_NAME` (`task_auto_pipeline` today). Preserve the configured drain
   ceiling and each pipeline's existing `max_active_runs: 10`; queued bound runs count as pending
   capacity until terminal settlement. Persist a new request ID for each free slot before sending it.
3. Persist the returned handle. Create or recover exactly one local leaf run per claim using a
   durable local uniqueness constraint, then bind its host-qualified run ID to the claim on the
   owner. Binding is idempotent and cannot replace another run for that claim.
4. Launch only after binding succeeds. A crash between any two steps resumes the same request,
   claim, or not-yet-started run. Stopping the drain stops new admissions; it does not invalidate live children.

Once execution starts, a dead process leaves an interrupted claim for deliberate recovery. V1
refuses `orbit job resume` for claimed leaves: existing `resume_job_run` creates a new run and
cannot inherit an immutable claim/run binding. Recovery fences the old claim before admitting a new
claim/run; preserved branch contents may seed that attempt, but validation and handoff are fresh.
In-run step retries keep the same bound run. Crash recovery before launch may recover the same
queued run and idempotent binding; it must never restart a run whose execution became uncertain.

Do not send an already claimed task through ordinary backlog discovery in `task_auto_pipeline`. Add
a claimed-task dispatch path that bypasses rediscovery and lock acquisition, verifies the claim, and
selects `task_pr_pipeline` for PR mode or `task_local_pipeline` for owner-local mode, with the
handle. A bare `pulled: true` flag is not authority. The `start_epic` branch is removed by §7.
Followers cannot execute local mode. The claimed local variant retains local base sync and needs no
origin or PR credentials; it stops before `git_merge` and emits a local-candidate handoff
(repository, branch, candidate/base SHAs, validation evidence). The owner consumer performs the
authorized local merge and verifies its commit evidence. Review-only local work remains an unmerged
candidate. Both variants settle the same claim lifecycle; local success cannot bypass it.

`reserve_locks` and `release_reservation` currently belong to `task_gate_pipeline`, not
`task_pr_pipeline`. Bypassing the gate therefore requires explicit claim settlement and cleanup
on success, launch failure, cancellation, and terminal failure. Settlement is an idempotent owner
mutation scoped to that claim's reservation; it never releases a newer attempt's reservation.
A terminal failure before review atomically records evidence, moves the task to `blocked`,
invalidates execution authority, and releases the reservation. If disconnected, persist the
pending settlement locally and retry; the owner retains the claim until settlement or deliberate
recovery. TTL is not a substitute for settlement.

Branches include immutable attempt identity. The existing run-derived worktree branch scheme may
remain because each claim binds one unique run; record that association durably. A claim-derived
name such as `orbit/<task-id>-<claim-id>` is also valid, not a required second scheme. A display
host name alone does not distinguish successive attempts on the same machine. The follower executes
implementation, validation, push, and PR opening locally, then submits the durable handoff described
below. It does not run merge completion.

### 3.1 Attempt ownership and recovery

The owner records each claim as `claimed`, `running`, `handed_off`, `failed`, or `revoked`.
Only `claimed` and `running` authorize execution writes. The current claim ID, authenticated
machine, bound run where applicable, and allowed phase are checked **inside the same transaction
as every claim-scoped mutation**. Cover task summaries, artifacts, comments, friction creation,
run binding, failure settlement, promotion, and cleanup. Mutation request IDs deduplicate retries
of append/create operations. Ordinary operator edits remain separately authorized and audited.

The existing `vcs/handoff.rs::load_handoff_context` run-ownership guard is useful but insufficient:
it reads ownership before later mutation, and currently compares an unqualified run ID. Extend
ownership to the claim and machine/run pair and enforce it on the owner. Generic task-write paths
used by workers must carry the same claim context; they cannot bypass fencing by omitting it.
Changing an actively claimed task's status, run binding, dependencies, or lock footprint must
preserve the claim invariant or atomically revoke the claim through deliberate recovery. V1 refuses
footprint expansion during execution; it requires stopping and re-admitting with revised context.

V1 has **manual reclamation**, no heartbeat and no automatic failure inference. Add an owner-side
claim listing with age, phase, reservation expiry, execution machine/run, and last recorded event.
Those fields aid inspection; age, TTL expiry, and an absent owner-local run are not proof of death.
`scan_unresolved_work` currently excludes `in-progress` and `review` tasks and reads local failed
runs. It is not a remote-claim detector. Status-derived locks on `in-progress` and `review` tasks
also survive reservation expiry using the frozen, non-pruned claim footprint, not an
existence-filtered recomputation.

Recovery inspects the recorded run on its host when possible and preserves any branch, PR, or
failure evidence. An authorized operator or supervised orchestrator explicitly revokes the claim
and chooses the permitted task transition, usually to `blocked` for diagnosis or to `backlog`
for retry. Revocation, invalidation of pending landing authority, reservation release, and task
transition are atomic. A replayed pull response for that attempt must not reactivate it. A sleeping
worker that returns receives `stale_claim` even if a newer attempt has made the task `in-progress`
again. Its local compute and an already in-flight GitHub write cannot be undone, but its old
candidate cannot become authoritative task state or pass the owner landing gate.

### 3.2 Durable review and landing handoff

The current `workspace_ship_pipeline` launches the workspace backlog drain; it does not consume
arbitrary tasks already in `review`. V1 adds an explicit owner-side handoff consumer. This is
required implementation work, not reuse of an existing review sweep. The ship-sweep routine and its
wrapper remain as described in [§7.3](#73-ship-sweep); landing does not depend on either.

The follower submits an idempotent handoff containing the claim and execution run identity,
repository and PR identity, source branch, published candidate head SHA, validated base SHA,
intended base and landing branch, execution summary, and durable validation artifact references. V1
supports only `review_policy = none`, captured at admission. Include typed review evidence `{
policy: none, disposition: not_required }`; do not invent reviewed SHAs, an agent verdict, or a
review artifact. Neither `before-pr` nor `after-landing` is admitted. Validation still runs on the
exact candidate/base pair. The PR pipeline's existing `gate: not_required` result is adapted into
this evidence, not treated as an empty successful review. Task status `review` means a delivery
handoff awaiting completion authority; it does not assert that an automated review occurred.
Artifacts must be accessible in the owner's coordination store; a follower-local path is not landing
evidence. Owner-local candidates carry the local variant above. No-diff/already-landed work carries
its existing typed evidence instead of a PR. The owner validates required evidence and atomically
persists the handoff, promotes the task to `review`, closes execution writes, and releases the claim
reservation. The task's review lock continues to protect its footprint. A lost response replays the
same handoff result.

Completion defaults to `review`. Pull eligibility or `agent` access alone does not authorize a
merge. Any `completion: done` must reference durable, explicitly granted completion authority
with task/workspace scope; neither the follower nor a newly enabled consumer may invent it.
Review-only handoffs remain awaiting approval until that authority is recorded. Persist the
authority reference with the handoff and recheck it at landing.

Add an owner-only, operator-authorized, idempotent **approve handoff** mutation. Its input
identifies workspace, current handoff/claim, exact candidate/base, and a mutation request ID. In one
transaction it verifies `review` state and the current evidence, persists a completion authorization
scoped to that handoff and candidate, records who approved it and when, and creates the durable
landing-start request. Store an immutable authorization ID, scope, approver, creation time, and
revocation state; recheck revocation/currentness before external merge. A follower's `agent` grant
cannot approve. An existing applicable completion grant can authorize the same record at handoff
acceptance. Do not silently reuse `enable_operation_grant` for post-handoff approval: its current
scope validator accepts only proposed/backlog tasks. Implement the review-state approval path
explicitly. Revocation invalidates pending landing authorization; uncertain merge intents still
require reconciliation.

An owner-local durable job consumes authorized handoffs, one landing attempt per handoff at a time.
Accepting a completion-authorized handoff durably records the request to start that job; approving a
review-only handoff records the same request. Owner job recovery must reconcile pending start
requests after interruption, with handoff identity deduplicating job creation. This is a required
handoff-to-job delivery contract, not a periodic backlog scan. An explicit owner operation can retry
or reconcile a named handoff without starting a drain. Neither scheduled ship-sweep nor a running
owner drain is required for accepted, authorized work to land. It verifies that the handoff is still
current, checks the exact candidate head/base, validation evidence, and typed `none` review
disposition, respects GitHub checks/protection for PRs, and verifies `MERGED` plus merge evidence
before `review → done` (or verified local merge evidence for owner-local candidates). Persist the
merge intent before the external call. After a crash or lost reply, reconcile GitHub's actual state,
or the owner-local target ref, for the same pinned candidate before retrying. Recovery must not
reassign a task with an unresolved merge intent until that intent is reconciled; database revocation
alone cannot cancel a request already sent to GitHub.

Existing `pr_complete` depends on the execution run and a local worktree. Extract or adapt its
pinned-delivery and completion checks for an owner handoff consumer without pretending the
follower's run or path is local. Do not weaken those checks or copy a follower run into the owner
store. A changed head/base or a conflict stops landing with durable evidence. Repairs execute as
an explicitly authorized new attempt and require fresh validation and a new handoff; the owner
does not silently rebase unvalidated code. No-diff completion uses the existing typed evidence
checks and the same completion-authority boundary.

## 4. Follower preconditions

Before new admission, check the common workspace execution requirements and local capacity.
A failed check records a diagnostic and sleeps; these probes reduce failures but cannot guarantee
that credentials, network access, or tools remain usable after pull.

| Check | Source of truth |
|---|---|
| Required crews and providers available and authenticated | Resolved workspace/task execution requirements and provider-specific probes |
| Binary version and orchestration schema match the owner | Owner read-only capability/version response; pull enforces parity again |
| Workspace identity, key-bound caller identity, and owner authorization match | Federated discovery and the read-only probe below; never call pull as a health check |
| Review policy is `none` on owner and executor | Owner policy captured at admission; executor verifies the same policy before binding |
| Sandbox and required OS/toolchain capabilities available | Existing doctor checks plus workspace execution prerequisites |
| Repository readable and credentials configured for push and PR operations | Git transport checks and provider authentication; `gh auth status` alone does not prove Git push permission |

The owner resolves ship mode, base/landing branches, and applicable completion authority. Follower
execution always stops at handoff, even when that authority permits the owner to complete. Local
ship mode is refused for followers. Equal binary/schema versions do not establish equal crew,
policy, or toolchain configuration; v1 requires compatible workspace execution settings as well.

### 4.1 Read-only admission probe

Add an owner-served read-only probe before enabling pull. Its input is the host-qualified workspace
selector; its response names the owner/workspace, binary version, distributed-drain protocol schema
version, effective workspace capabilities, caller machine and identity proof, resolved ship mode,
and review policy. It creates no receipts, reservations, claims, or tasks. Require destination-bound
`KeyBound` identity for remote execution; a callers-file row selected by a self-asserted machine ID
is insufficient. Local owner execution uses trusted local runtime identity. Return explicit
selector/authorization errors without disclosing another caller's state. A policy/version mismatch
is observable in the response so a client can diagnose it before admission; pull enforces it again.
The distributed-drain protocol schema starts at `1` and versions pull, probe, and lifecycle
request/response shapes; incompatible changes increment it. It is not the scoreboard's
`ORCHESTRATION_SCHEMA_VERSION`. MCP initialization's binary/protocol metadata alone is insufficient.

## 5. Transport and authority routing

Followers initiate federated MCP over SSH stdio. The owner's callers file authorizes the machine
for the workspace; the follower's destinations file locates the owner. SSH connection reuse is a
transport optimization, not a delivery guarantee. Every coordination mutation has an idempotency
or reconciliation contract before step recovery retries it.

| Data or operation | Authority and execution location |
|---|---|
| Tasks, dependencies, comments, history, coordination artifacts, claim state | Owner for reads and writes; no replica-local fallback |
| Ready ordering, lock admission, claim settlement, handoff acceptance | Owner transactions |
| Worktree, Git operations, agent execution, build/test, local run/step state and logs | Executing host |
| Review policy | `none` only in v1; typed not-required disposition and validation evidence live on owner |
| Completion authority, landing intent, merge verification, task completion | Owner |

Add the routed task-read and coordination-write seam to `RuntimeHost` and associated artifact and
review operations. An activity such as `pr_promote` mixes local Git/worktree checks with owner
mutations: do not forward the entire activity and its local paths as though they exist remotely.
Worker subprocess tools inherit the persisted owner selector and claim context as well. On an
owner outage, dependent reads and mutations fail closed; local run diagnostics remain writable.

The destination authenticates machine identity through key-bound SSH identity, not Tier 1
self-assertion. Host display labels in payloads are diagnostic, not credentials. Revalidate
authorization for retries. An authorization or version failure after an uncertain request leaves a
pending admission to reconcile, not permission to mint another request and forget the first. A
separate read-only receipt lookup (specified with pull) can resolve an uncertain admission after a
compatible client upgrade without changing the original request's input. It requires current
authorization and identity proof, but does not reapply ship/review policy or original binary parity.
It grants no execution authority. If authorization is revoked or the lookup protocol itself is
incompatible, an authorized owner uses claim inspection and deliberate recovery; do not expose
receipts to the revoked caller. No inbound follower connection or fleet registry is required.

## 6. Execution provenance

Execution identity is required for v1 claim fencing and handoff acceptance, not deferred
observability. Today a job run, a task's `job_run_id`, and a task artifact
say nothing about where execution happened; with one host that was implicit, with two it is a
dangling reference.

| Record | Field | Set by | Notes |
|---|---|---|---|
| Job run | `executed_on { machine_id, host_id }` | the runtime that inserts the run | immutable; steps inherit; nullable so pre-existing rows read as *unknown*, never as "the owner" |
| Task | `job_run_host` beside `job_run_id` | the pipeline that links the run, via the owner | a pulled task's run lives in the follower's store; without the host the owner's `orbit run show` cannot resolve it |
| Task history | `pulled_by { machine_id, run_context, claim_id, request_id }` | `orbit.task.pull` | already in the spec |
| Task artifact | `origin { machine_id, host_id }` | the owner, at put time | over federated MCP the identity comes from the authenticated caller row, not from the payload |
| Agent envelope | `ORBIT_MACHINE_ID`, `ORBIT_HOST_ID` | the dispatching runner | advisory, for execution summaries and PR bodies; the store fields above are the truth |

The key is the stable `machine_id`; `host_id` rides along for display and may be renamed. Nothing is
inferred from hostname, cwd, SSH target, or audit label, per the host-registry rule. Federated run
inspection ([3_vision.md](./3_vision.md#1-open-questions)) is what eventually reads these fields
across hosts; until then they identify where to inspect a run manually. A host pointer alone does
not provide remote reachability or a lookup implementation.

## 7. Retirements and retained ship sweep

Two existing mechanisms are retired as part of this feature: epic execution and failed-run triage.
Ship sweep remains available; all entry points use the same owner admission boundary.

### 7.1 Epic machinery

The epic distinction — a root that owns one stable worktree and branch, drains its children into
it sequentially, reserves the union of its descendants' `context_files`, is excluded from leaf
admission, and is finished by a dedicated `epic_orchestrator` — was a way to give one large body
of work a single review artifact. In practice it fragmented the drain: one epic pinned a slot for
its whole life, its reservation shadowed unrelated leaves, and none of it splits across hosts.

What is removed:

| Piece | Anchor |
|---|---|
| `epic_pipeline` job and its `list_epic_descendants` / drain loop / finisher steps | `crates/orbit-core/assets/jobs/epic_pipeline.yaml` |
| `epic_orchestrator` activity | `crates/orbit-core/assets/activities/` |
| `start_epic` step and `has_epic` / `epic_task_id` / `active_epic_run_id` outputs | `workspace_auto_pipeline.yaml`, `classify_workspace_auto_tasks.yaml` |
| Epic-tag exclusion from leaf admission and the refusal to ship an `epic`-tagged root | `list_backlog_tasks`, `classify_workspace_auto_tasks`, ship admission |
| Descendant-union footprint for `epic`-tagged roots | `crates/orbit-core/src/runtime/task/locks.rs::lock_context_files_for_task` — the `tags.contains("epic")` branch |
| Epic-specific worktree identity/GC handling | `crates/orbit-engine/src/executor/automation/vcs/worktree/mod.rs::WorktreeIdentity::from_input`, `crates/orbit-core/src/application/gc.rs::delivery_job_owns_worktree`; preserve decoding needed to reap historical worktrees |
| `docs/design/resident-orchestrator/` | moves to `docs/design/_archive/` with a supersession pointer to this folder |

What stays:

- **Parent/child task relations.** Hierarchy is still useful for reading a backlog; it just no
  longer changes admission. A child is a leaf like any other, ordered by its own priority, age, and
  dependencies.
- **The `epic` tag**, redefined: a size hint meaning *one large task a top-tier crew takes on
  whole*. Crew selection reads it (today by hand; later through auto-assignment, where `epic`
  routes to the pools `fable` / `astra` serve). Admission ignores it.
- **`workspace_auto_pipeline`'s drain window, slot refill, and detached leaves** — the parts of
  the resident-orchestrator work that were actually about throughput.

[ORB-12491] implemented this section. The migration check is `orbit-core`'s
`application::epic_retirement`: `assess_epic_retirement` is a pure decision over a
tasks/runs/reservations snapshot, and `OrbitRuntime::epic_retirement_readiness` is its read-only
gatherer. It writes nothing — the refusal, the inherited-only roots, and the historical runs GC
must still discover are the product.

Migration refuses while any old epic execution, child execution, reservation, or uncertain landing
is unreconciled, regardless of root status. A root already in `review` can still have a live
`complete_pr` step; a status-only `in-progress` check is insufficient. Drain or deliberately stop
and reconcile those runs before removal. Preserve historical worktree discovery until cleanup is
verified. Existing epic-tagged tasks retain tags and hierarchy, but roots that relied solely on
inherited child context need operator-supplied own context or deliberate retirement before they
become eligible; migration reports them rather than silently converting an empty root.

### 7.2 Failed-run triage

`task_triage_pipeline` and its seeded `task_triage` routine list blocked tasks whose `job_run_id`
points at a failed run in the local store, have an agent classify the failure, and re-backlog the
"environmental" ones. Under followers the run a task is blocked on may live on another host, so
the owner's triage either skips it or diagnoses the wrong thing, and an automatic re-backlog would
hide exactly the host-specific failures the operator needs to see.

What is removed: `task_triage_pipeline.yaml`, `routines/task_triage.yaml` (shipped `enabled:
false`), the `list_triage_candidates` / `triage_failed_runs` / `apply_triage_dispositions`
activities, the triage recursion guard in `application/automation/incidents.rs`, the seed entry in
`application/routine.rs`, and the references in `CONFIG.md`, operation-mode, automation-triggers,
and the orbit-orchestrate recovery reference.

What replaces it: nothing automatic. A failed run parks its task in `blocked` with the failure and
`job_run_host` attached; a human or the orchestrate skill reads it. Re-backlogging is a deliberate
transition, made by whoever looked.

### 7.3 Ship sweep

Ship sweep is retained by Daniel's revised decision. Keep the seeded `ship_sweep` routine,
`workspace_ship_pipeline`, and the separate registry-driven `orbit run ship-sweep` CLI, including
its `workflow.auto_ship` opt-in and external scheduler support. Preserve existing enablement and
schedules; this design does not enable any routine or install a timer.

Adapt every entry point to the common claim admission contract. The CLI currently calls
`submit_ship_run` directly; retaining only the YAML wrapper is not sufficient coverage. Owner ship
sweeps may start owner work, but cannot use legacy backlog selection or reservation paths to bypass
claims, policy checks, or footprint serialization. Replica-host sweeps continue refusing owner-only
coordination work; followers execute through pull. Scheduled invocation confers no completion
authority. Landing is driven by durable accepted-handoff/approval requests and continues when no
ship sweep or drain is running.

## 8. Required validation scenarios

These are implementation acceptance criteria, not tests reported as passing by this draft.

| Scenario | Required result |
|---|---|
| Concurrent pulls and concurrent ordinary task/reservation writes | One current claim per task; no overlapping admission; readiness revalidated transactionally |
| Commit succeeds but pull response is lost | Same request returns the same claim; no second task is consumed |
| Idle result replayed after new work arrives | Same request remains idle; a new poll may claim work |
| Crash before local run creation, after creation, or before binding response | Reconcile the same claim; at most one local leaf per claim; pending admission occupies capacity |
| Invalid dependency or empty lock surface | Diagnostic exclusion; unrelated eligible tasks can still progress |
| Reservation expires during valid execution | No automatic revocation or duplicate admission; task status lock remains |
| Old worker returns after deliberate recovery and reassignment | Old claim cannot bind, mutate task evidence, promote, settle, or release the new reservation |
| Failure/cancellation while owner is disconnected | Local settlement remains pending; eventual idempotent settlement or explicit recovery |
| Detached child or in-run step retry reads a task | Owner routing and claim context survive; no local task-store fallback |
| Generic resume of an interrupted claimed leaf | Explicit refusal; deliberate recovery creates a fenced new claim/run, preserving branch evidence |
| Handoff commits but response is lost | Exactly one authoritative handoff and review transition |
| Review-only handoff reaches the landing consumer | No merge without recorded completion authorization |
| PR head/base changes or merge conflicts | Stop with evidence; fresh validated repair required |
| Merge succeeds but owner crashes before completion | Reconcile the pinned PR and merge evidence before marking done or retrying |
| Recovery requested with an uncertain external merge in flight | Reassignment waits for merge-intent reconciliation |
| No-diff/already-landed delivery | Typed durable evidence and completion authority still required |
| Authorized handoff accepted while no owner drain or ship sweep runs | Landing request is durably dispatched or recovered once; review-only work remains unmerged |
| Retained routine, wrapper, CLI ship-sweep, and explicit owner drains | All use common claim admission; existing enablement retained; none grants merge rights or bypasses slot accounting |
| Epic retirement with active old runs, including roots in review | Refuse migration until old execution and reservation ownership are reconciled |
| Missing file selector, then reservation expiry | Full declared footprint remains protected; no filesystem-existence pruning or overlapping admission |
| Truly empty legacy task/epic context | Diagnostic with pre-admission repair; no guessed or inherited surface |
| `none`, `before-pr`, and `after-landing` review policies | Only `none` admits; typed not-required handoff works without reviewed SHAs or a review artifact |
| Owner-local task without origin | Local candidate handoff and authorized local landing; no PR dispatch or remote credentials required |
| Remote caller with self-asserted identity and agent capability | Probe reports proof; pull and lifecycle execution writes refuse without key-bound identity |
| Read-only probe and receipt lookup after binary upgrade | No admission side effects; lookup finds original outcome without rewriting original input; revoked callers remain refused |
| Approve a review-only handoff twice, or revoke before merge | One durable authorization/start request; revoked or stale candidate cannot land |
| Idle polling and receipt compaction | One idle request per refill pass; tombstone replay cannot re-admit; unsettled receipt retained; growth metrics visible |
| PR/local leaves replace auto wrappers in capacity accounting | Configured local ceiling includes actual bound/queued runs and unrepresented admissions exactly once |

## 9. Concerns & Honest Limitations

- **Receipt metadata grows in v1.** Idle polling and settled requests leave permanent compact
  tombstones. Stop at first idle per pass and expose storage metrics; safe bounded retention is
  future protocol work, not deletion based only on age or UUID collision probability.
- **No automatic review in v1.** `review_policy` must be `none`; build/test validation and delivery
  evidence remain mandatory. Before-PR and after-landing review need a later protocol extension.
- **Manual recovery limits availability.** A dead or unreachable follower can retain its task's
  footprint indefinitely. Claim age and reservation TTL are diagnostics, not failure detectors.
- **Revocation cannot stop remote compute or retract external writes.** An old attempt can finish
  a push or open an orphan PR. Attempt-specific branches and owner fencing isolate authoritative
  delivery; local worktree GC does not delete remote branches or close PRs automatically.
- **No heterogeneous eligibility yet.** Every participant must satisfy the workspace's full
  execution requirements. A Mac and Linux host are interchangeable only for tasks whose required
  validation is supported on both. Auth probes cannot establish that by themselves.
- **Coordination requires the owner.** Task reads, evidence writes, settlement, and handoff stall
  during partitions. Logs remain local and full federated run inspection is future work.
- **Large tasks still hold slots.** The `epic` tag adds no special execution path after retirement;
  large tasks may hold their own footprints for hours.
- **One owner is an operator prerequisite.** Two independent stores can admit overlapping work in
  the same repository. Migration must quiesce old drains, reconcile active runs and PRs, move tasks,
  disable the demoted host's coordination routines, and only then enable replica pull mode.
- **Throughput is not guaranteed to double.** Shared CI, provider limits, file conflicts, and serial
  landing can become the next bottleneck. Compiler caches remain per host.
- **Auto-task minting stays owner-only.** Claim-scoped friction writes are routed to the owner;
  follower auto-task issuance is out of scope.

## Task References

- [ORB-12488] — authored this design folder for the pull-based multi-host drain.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
