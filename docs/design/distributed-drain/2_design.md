---
title: Distributed Drain — Design
owner: claude
last_updated: 2026-09-24
last_validated: 2026-09-20
status: Draft
feature: distributed-drain
doc_role: design
type: design
summary: "One owner, multiple execution hosts: idempotent claims, routed authority, manual recovery, explicit landing, retained ship sweep, none-only review, and non-pruning context footprints."
tags: [distributed-drain, multi-host, pull, federated-mcp]
paths: ["crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml", "crates/orbit-core/assets/jobs/task_pr_pipeline.yaml", "crates/orbit-core/assets/activities/classify_workspace_auto_tasks.yaml", "crates/orbit-core/src/runtime/task/locks.rs", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-mcp/**"]
related_features: [distributed-drain, federated-mcp, host-registry, activity-job, policy-sandbox]
related_artifacts: [ORB-12488, ORB-12516, ORB-12582, ORB-12616]
---

# Distributed Drain — Design

> **Status: Draft, partly live.** Live on the owner: the claim/admission substrate, owner-local
> claimed leaves, handoff acceptance and approval, the landing consumer, the probe and claim
> listing, dashboard actions, and shared capacity and entry-point admission. **Not live:** the
> routed follower peer, `orbit run auto --pull`, and every mutating distributed entry point,
> refused by `application::distributed::DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED` (`false`).

This doc covers v1: one owner checkout, any number of replica checkouts on other hosts, each
replica running the drain in pull mode against the owner, and the machinery the shape retires.
Deferred to [3_vision.md](./3_vision.md): a cloud-offloaded owner store, crew auto-assignment,
and follower-side merge. Rationale lives in [4_decisions.md](./4_decisions.md).

## 1. Roles: one owner, N followers

Roles are the host-registry catalog roles, unchanged. The owner checkout has no checkout-level
`owner_machine_id` (its logical owner is the local `machine_id`); a follower is a replica checkout
whose `owner_machine_id` names the owner machine. `RegisteredRuntimeFactory` carries the replica
owner into Core's coordination-write guard
(`crates/orbit-cmd/src/registry_runtime.rs::replica_owner_for_checkout`), and
`automation/ownership.rs` refuses delivery automation on a replica. No role, fleet table or host
list is added.

**Operator precondition: one control plane per repository.** A repository initialized as owner on
two hosts is collapsed first by re-registering one checkout as a replica and migrating or draining
its tasks. The federated mux cannot detect the collision; this is a runbook step.

## 2. The ready queue and `orbit.task.pull`

The **ready queue** is a logical owner-side query over `backlog` tasks whose dependencies are all
`done`, ordered by the automatic-dispatch comparator (corrective tags, task-ID tie-breaker). No
materialized table is required; any projection is advisory until admission revalidates it.
Followers never compute readiness or priority.

`orbit.task.pull` is an owner-only `control_plane` tool; its contract is
[specs/task-pull.md](./specs/task-pull.md). Candidate selection, readiness and footprint
validation, reservation, claim creation, `backlog → in-progress`, history and the request result
commit in **one store transaction**, and every other task-update and reservation path shares that
serialization boundary. Pre-checking outside the transaction is insufficient.

**Substrate (live, [ORB-12528]).** `orbit-store`'s task/reservation commit boundary publishes a
task transition, its history, a reservation and dependent coordination rows as one durable
decision ([task_commit_boundary.md](../../design-patterns/task_commit_boundary.md)). Every runtime
constructor uses `compose::workspace_coordinated_backends`. The internal owner admission API builds
immutable receipts and frozen claims on that journal and shares its ordering with readiness
reporting; a host admission lock serializes cross-workspace dependency checks.

**Request identity.** A caller durably allocates a `request_id` before each pull, scoped to the
owner workspace and runtime execution-machine namespace. A retry with the same input returns the
stored result, never another task; the drain run ID is context, not an idempotency key.

- `idle` results are stored too; a later poll uses a new ID. Refusals create no claim. A refill
  pass stops at its first idle response.
- No deletion may let an old ID become a new admission: v1 keeps compact tombstones forever and
  full receipts for unsettled claims, and exposes their counts and bytes. Bounded retention needs a
  protocol that rejects retired namespaces, not a time-based DELETE.

The response carries the task, resolved ship inputs, and a **claim handle** (`claim_id`,
`reservation_id`, reservation expiry, runtime execution machine). A claim identifies one attempt,
including the interval before a leaf run exists. Owner and follower drains use the same path.

**Footprints.** There is no epic path ([§7.1](#71-epic-machinery)); an `epic`-tagged task is an
ordinary entry using its own `context_files`, and sequencing is expressed with dependencies.

- An empty or invalid lock surface is reported ineligible, never admitted as a claim protecting no
  files. The legacy v2 dispatch path still admits an empty surface as a compatibility no-op, while
  `orbit.task.locks.reserve` refuses an empty task-scope reservation; diagnostics name which path
  and the remedy.
- Context is never pruned by filesystem existence. Selectors are canonicalized and held to
  repository boundaries, but selectors for not-yet-created files and symbols are preserved;
  `allow_missing_context` governs explicit operator existence checks only. Admission, reservation,
  status locks and task reads share `runtime/task/mod.rs::declared_context_files` [ORB-12490].
- The full canonical footprint is frozen on the claim and used through execution and review,
  including after reservation expiry; checkout contents cannot shrink it.
- Previously pruned declarations are restored from task history where possible
  (`application/task/context_repair.rs`, via `orbit task lint --restore-pruned`) or reported for
  operator repair, never guessed.

**Eligibility.** Pull has no crew or platform filter in v1: every participant must execute every
eligible task, with task and fallback crews resolving equivalently.

## 3. Pull-mode drain and the pulled leaf pipeline

### Caller-side implementation status

The job store's `local_pull` migration persists immutable requests, receipts, unique
claim-to-leaf bindings, launch intent and pending settlement; capacity allocation and leaf
creation share SQLite writer transactions with job state. An unacknowledged launch intent is
uncertain and refuses replay; a terminal leaf holds capacity until settlement is acknowledged.
Stopping parent admission leaves children intact. Queued-leaf cancellation is reconciled before
binding or launch, and the launch-intent transaction requires the bound run to still be pending,
so it cannot overwrite a cancellation.

**Claimed leaf definitions** ([ORB-12616]). A claim selects `task_claimed_local_pipeline` or
`task_claimed_pr_pipeline` by owner-resolved ship mode, never the merge-capable legacy pair. Both
skip rediscovery and reservation (the owner froze the footprint; settlement releases it), expose
no `completion` input, end at `claim_handoff`, and contain no `git_merge`, `pr_complete` or
`task_complete`. The local variant also has no `git_push`, `pr_prepare` or `pr_open`, so it needs
no origin or PR credentials.

- `claim_validate` resolves candidate and base from Git in the executor's worktree, refuses a
  candidate that does not descend from the base, runs the owner's
  `workflow.required_validation_commands` on that exact candidate, and attaches one
  `HandoffValidationLog` per command to the owner's task through the routed coordination
  transport. An empty owner requirement list fails closed on both sides.
- `claim_handoff` re-observes the same identity, refuses a worktree that moved, and records the
  typed `TaskHandoff` as the claim's durable pending settlement *before* any owner call, so a
  disconnect leaves one immutable settlement the next refill retries idempotently.

**Execution authority** is the trusted worker binding plus the durable admission, never a
payload: `execute_pipeline_run_worker` admits a claimed leaf only when the binding's task, claim,
execution machine, bound run and owner match the admission. The supervisor records the binding
against the child PID and sets `ORBIT_WORKER_CONTEXT_REQUIRED`. A bound leaf's worktree admission
reads the owner's admitted task instead of re-admitting; generic workers and generic resume refuse
these leaves.

**One capacity ceiling** ([ORB-12617]). The legacy classifier and the pull allocator read one
occupancy, `JobRunStoreBackend::drain_leaf_occupancy`, which the allocator commits against inside
its transaction:

- a wrapper is replaced by the descendant carrying its work (a live run of any of the four leaf
  definitions, or a pull admission whose bound leaf went terminal before settling), not counted
  beside it;
- an admission with no live run holds its own slot;
- each definition's `max_active_runs: 10` is checked against the same reading;
- a slot returns when the claim settles, not when its leaf terminalizes.

`classify_workspace_auto_tasks` and the readiness diagnostic report `wrapper_leaf_runs` and
`leaf_occupancy_by_pipeline`.

**Crash cuts.** Request, create, bind and launch cuts leave one claim and one leaf. A failed
launch leaves one settlement retried idempotently; a killed launch leaves a `Launching` record every
generic path (drain, `orbit job resume`) refuses, pending deliberate recovery.

**Published delivery** ([ORB-12500]). The owner accepts a PR handoff from its own observation
(`orbit_engine::observe_published_candidate`): it reads the PR from the provider, pins head
branch, base branch and head commit against the submitted candidate, refuses closed-without-merge
or self-contradictory state, and resolves candidate and base in *its own* checkout with the
executor's tree-identity and ancestry rules. A merged PR is observable, not refused (completion
still needs authority). Only `landing_branch` (owner-resolved ship configuration) is taken from
the submission. Acceptance and landing resolve revisions through one shared rule.

**Not yet executable.** Only an owner-local destination is served. The routed follower
`PullPeer` over federated SSH does not exist (only `OwnerPullPeer` in
`adapter/engine_host/v2_host/pull_adapters.rs`), no mutating distributed entry point is a
registered tool, and `orbit run auto --pull` is not implemented. `run auto` / `run ship` still
render a legacy pipeline name for `pr` and `local` modes after taking the shared admission
decision ([§7.3](#73-ship-sweep)).

### Target pull-mode contract

`orbit run auto --pull <selector>` binds a local replica checkout to the owner's host-qualified
selector from federated discovery, verifies the checkout belongs to that workspace and repository,
and persists owner machine, workspace identity, selector and claim in run inputs, which detached
children and in-run step retries inherit. A renamed or unavailable destination never falls back to
a local coordination store. The drain keeps its window, sleep controls and detached execution;
admission becomes:

1. Reconcile pending local pull requests and claimed-but-not-launched work first.
2. Count live leaf runs **and pending admissions without a live run** against local capacity,
   across all four leaf definitions, under the configured ceiling and each `max_active_runs`.
   Queued bound runs count until terminal settlement. Persist a new request ID per free slot
   before sending it.
3. Persist the handle. Create or recover exactly one local leaf per claim (durable local
   uniqueness), then bind its host-qualified run ID on the owner. Binding is idempotent and never
   replaces another run for that claim.
4. Launch only after binding. A crash between steps resumes the same request, claim or
   not-yet-started run. Stopping the drain stops new admissions, not live children.

**Interrupted execution** is left for deliberate recovery. `orbit job resume` refuses claimed
leaves (`resume_job_run` creates a new run that cannot inherit the binding). Recovery fences the old
claim before admitting a new claim/run; preserved branches may seed it, but validation and handoff
are fresh. Step retries keep the bound run; pre-launch recovery may reuse the queued run, but an
uncertain run is never restarted.

**Claimed dispatch.** A claimed task never goes through `task_auto_pipeline` backlog discovery.
The claimed path verifies the claim and selects the claimed leaf for the owner-resolved mode; a
bare `pulled: true` flag is not authority. Followers cannot execute local mode. The local variant
keeps local base sync, stops before `git_merge`, and hands off a local candidate (repository,
branch, candidate/base SHAs, validation evidence) for the owner's authorized local merge.
Review-only local work stays an unmerged candidate.

**Settlement.** `reserve_locks` / `release_reservation` live in `task_gate_pipeline`, so the
claimed path settles explicitly on success, launch failure, cancellation and terminal failure.
Settlement is an idempotent owner mutation scoped to that claim's reservation and never releases
a newer attempt's. A terminal failure before review atomically records evidence, moves the task to
`blocked`, invalidates execution authority and releases the reservation. Disconnected, the
settlement is persisted locally and retried; the owner holds the claim until settlement or
recovery. TTL is not settlement.

**Branches** carry attempt identity: the run-derived branch suffices because each claim binds
one run; `orbit/<task-id>-<claim-id>` is also valid. The follower implements, validates, pushes and
opens the PR, then hands off; it never runs merge completion.

### 3.1 Attempt ownership and recovery

Claim phases are `claimed`, `running`, `handed_off`, `failed`, `revoked` and `landed`
(`ExecutionClaimPhase`). Only `claimed` and `running` authorize execution writes; `claimed`,
`running` and `handed_off` protect the footprint. Current claim ID, trusted runtime machine, bound
run and allowed phase are checked **inside the same transaction as every claim-scoped mutation**:
task summaries, artifacts, comments, friction, run binding, failure settlement, promotion and
cleanup. Mutation request IDs deduplicate append/create retries. Operator edits remain separately
authorized and audited.

- Ownership is the claim plus machine/run pair, enforced on the owner; the local run-ownership
  read in `vcs/handoff.rs::load_handoff_context` is not sufficient alone. Worker task writes must
  carry claim context; omitting it cannot bypass fencing.
- Changing a claimed task's status, run binding, dependencies or footprint must preserve the claim
  invariant or revoke it through recovery. Footprint expansion during execution is refused.

**Manual reclamation only**: no heartbeat or failure inference. The claim listing shows age,
phase, reservation expiry, execution machine/run and last event; none of these proves death.
`scan_unresolved_work` is not a remote-claim detector. Status locks on `in-progress` and `review`
tasks survive reservation expiry using the frozen footprint.

**Recovery** preserves branch, PR and failure evidence; an authorized operator or supervised
orchestrator revokes the claim and picks the transition (usually `blocked` or `backlog`).
Revocation, landing-authority invalidation, reservation release and transition are atomic. A
replayed pull cannot reactivate the attempt, and a returning worker gets `stale_claim` even if a
newer attempt made the task `in-progress`; its candidate can never become task state or land.

The substrate is `ClaimInvocation` (non-deserializable trusted context), `ClaimMutation` and
read-only `ClaimInspection` (which refuses pending journal repair rather than writing), committed
through the task journal.

### 3.2 Durable review and landing handoff

**Handoff contents.** An idempotent submission of claim and execution run identity, repository
and PR identity, source branch, published candidate head SHA, validated base SHA, intended base
and landing branch, execution summary and validation artifact references.

- V1 admits only `review_policy = none`, captured at admission ([V1 review policy is
  none](./4_decisions.md#v1-review-policy-is-none)). Review evidence is the typed
  `{ policy: none, disposition: not_required }`, with no reviewed SHA, verdict or review artifact;
  the PR pipeline's `gate: not_required` is adapted into it. Task status `review` means a delivery
  handoff awaiting completion authority, not that a review occurred.
- Validation runs on the exact candidate/base pair. Artifacts must live in the owner's store; a
  follower-local path is not evidence.
- No-diff/already-landed work carries its typed evidence instead of a PR; the owner observer
  itself runs the Git ancestry, delivery-marker, unchanged-scope and clean-tree checks.

**Acceptance** (`application::review`, `TaskCommitBoundary`). `TaskHandoff` binds workspace, task,
claim, execution machine/run, repository, delivery variant, branches and candidate/base
commit/tree identities. Owner observations and required validation commands are trusted runtime
context, never deserialized from worker input. Each validation reference pins the digest of an
owner task artifact holding the exact identity, command, zero exit code and captured output;
acceptance, approval and merge-intent publication each re-read it. The summary-only handoff shape
stays readable but refuses new writes. Handoff, `review` transition, claim closure, reservation
release, and any grant-backed authorization and pending landing-start record commit together; the
task's review lock keeps protecting the footprint. A lost response replays the same result.

**Completion authority.** Completion defaults to `review`; pull eligibility or `agent` access
never authorizes a merge. `completion: done` requires durable, explicitly granted task/workspace
authority, persisted with the handoff and rechecked at landing.

- **Approve handoff** is owner-only, operator-authorized and idempotent. Input: workspace, current
  handoff/claim, exact candidate/base, mutation request ID. One transaction verifies `review`
  state and current evidence, persists an immutable candidate-scoped authorization (ID, scope,
  approver, time, revocation state), and creates one deduplicated landing-start request. A
  follower's `agent` grant cannot approve.
- **Revocation** writes a separate immutable audit row and cancels the pending request; recovery
  does the same while revoking the claim. An unresolved merge intent blocks both until reconciled.
- Acceptance and merge-intent publication recheck the grant (including revocation) inside the
  commit; a replay is never renewed permission.

### Landing consumer implementation status

The landing consumer is the durable owner job `task_landing_pipeline`, one attempt per handoff
at a time, keyed by handoff identity. Recording completion authority (accepting a
completion-authorized handoff, or approving a review-only one) dispatches it from the outbox in the
same call; no drain, ship sweep, routine or schedule is involved. A merged handoff refuses
re-dispatch, a live owner job is left alone, and a pending request whose job never started or died
is taken by the next explicit dispatch pass. Landing a named handoff is also an explicit owner
operation (retry or reconcile).

The merge intent is persisted before the external call; after a crash or lost reply the next
attempt reconciles the provider's state or the local target ref for the same pinned candidate
before retrying, and the store refuses revocation, recovery and reassignment until then. Repairs
are a new authorized attempt with fresh validation and handoff; the owner never silently rebases.

`handoff_land` reuses `pr_complete`'s pinned delivery identity (branch, base and head-commit pins,
merged-with-merge-commit evidence, provider-state classification) with no follower run or path.
The owner checks the head on every poll, resolves candidate and base in its own checkout, and
verifies the validated base is reachable from `origin/<landing branch>`, fetched once from `origin`
per attempt so a commit landed from another machine is not misread as missing. Changed identity, a
conflict, unsatisfied protection or an exhausted check budget records a durable stop and leaves the
task in `review`. Owner-local candidates fast-forward the local landing branch and are verified
from the ref; no-diff delivery verifies its covering commit on the landing ref with no external
call. Every completion re-runs authorization, candidate and validation checks inside the
`review → done` transaction, and the activity's observation must equal the accepted candidate.

### Owner dashboard surface

[ORB-12516] added claim state and actions to the owner's existing task and run views. There is no
distributed tab or route; the panel appears on a task detail only when this workspace holds a
claim for it, and a replica says the owner holds that state.

- **Read:** one owner projection (`application::review::handoff`) of execution machine, claim
  phase and bound run, frozen footprint, reservation expiry, the accepted handoff and its
  authority and landing state. Wording is load-bearing: an elapsed reservation is *not*
  revocation or death; `review` awaits authority, not a passed code review; merged means into the
  landing branch, not deployed; a remote run names the host to inspect; absent provenance is
  *unknown*.
- **Act:** `approve_task_handoff`, `revoke_task_handoff` and `ClaimMutation::Recover` behind the
  operator-only dashboard governed rows `handoff.approve`, `handoff.revoke`, `claim.recover`. Each
  action carries the identity shown plus a replay identity; approval rebuilds its observation from
  the owner's record. Stale identity, replica checkout and unresolved merge intent are distinct
  typed refusals.
- `ClaimInvocation` and `HandoffObservation` are not constructible outside trusted runtime code,
  so the HTTP layer holds no lifecycle state. These are owner-local actions; the follower gate is
  unaffected.

## 4. Follower preconditions

Before new admission, check execution requirements and local capacity; a failed check records a
diagnostic and sleeps. Probes reduce failures but guarantee nothing after pull.

| Check | Source of truth |
|---|---|
| Required crews and providers available and authenticated | Resolved workspace/task execution requirements and provider-specific probes |
| Binary version and orchestration schema match the owner | Owner read-only capability/version response; pull enforces parity again |
| Workspace identity, SSH owner access, and session capability match | Federated discovery and the read-only probe below; never call pull as a health check |
| Review policy is `none` on owner and executor | Owner policy captured at admission; executor verifies the same policy before binding |
| Sandbox and required OS/toolchain capabilities available | Existing doctor checks plus workspace execution prerequisites |
| Repository readable and credentials configured for push and PR operations | Git transport checks and provider authentication; `gh auth status` alone does not prove Git push permission |

The owner resolves ship mode, base/landing branches and completion authority. Follower execution
always stops at handoff; local ship mode is refused for followers. Equal binary/schema versions do
not imply equal crew, policy or toolchain configuration; v1 requires both.

### 4.1 Read-only admission probe

The owner serves a read-only probe ([ORB-12495]). Input: the host-qualified workspace selector.
Response: owner/workspace, binary version, distributed-drain protocol schema version, effective
session capabilities, diagnostic caller machine, resolved ship mode and review policy. It creates
no receipts, reservations, claims or tasks. A caller may declare its version, protocol schema and
review policy, and the probe reports the first refusal admission would raise by running the same
ordered ladder (`orbit_store::admission_refusal`).

- The protocol schema starts at `1` and versions pull, probe and lifecycle shapes; it is not the
  scoreboard's `ORCHESTRATION_SCHEMA_VERSION`, and MCP initialization metadata is insufficient.
- The owner's read-only surface is `orbit.drain.probe`, `orbit.drain.receipt.lookup` and the
  operator-only `orbit.drain.claims` listing ([§3.1](#31-attempt-ownership-and-recovery)). MCP and
  `orbit tool run` reach them through one registry and `application::distributed`. The read-only
  tools are `control_plane`, so a replica refuses them.
- Their governed-operation rows allow `agent` or `operator` — an identification floor, not an
  operator gate ([ORB-12582]). Cross-attempt receipt inspection requires `operator`, resolved by
  `runtime::authorization::resolved_caller_capabilities`, not read off the session.
- SSH login establishes owner access; there is no destination callers file, forced-command
  requirement, key-bound proof or identity registry. Managed callers cannot gain operator
  capability by changing tool input or launching a privileged child.

## 5. Transport and authority routing

SSH login to the owner is the admission; session agent/operator capabilities and caller-side
managed-run restrictions still apply; execution-machine labels are attribution; trusted invocation
context fences a claim and its run ([SSH login is the admission; machine labels are
attribution](./4_decisions.md#ssh-login-is-the-admission-machine-labels-are-attribution)).
Followers initiate federated MCP over SSH stdio, locating the owner through their destinations
file. SSH connection reuse is an optimization, not a delivery guarantee; every coordination
mutation has an idempotency or reconciliation contract before step recovery retries it.

| Data or operation | Authority and execution location |
|---|---|
| Tasks, dependencies, comments, history, coordination artifacts, claim state | Owner for reads and writes; no replica-local fallback |
| Ready ordering, lock admission, claim settlement, handoff acceptance | Owner transactions |
| Worktree, Git operations, agent execution, build/test, local run/step state and logs | Executing host |
| Review policy | `none` only in v1; typed not-required disposition and validation evidence live on owner |
| Completion authority, landing intent, merge verification, task completion | Owner |

**Worker invocation.** `WorkerInvocation` carries owner destination/workspace, task, claim,
execution location and immutable bound run. Runtime composition installs it; `ToolSessionContext`
JSON cannot set it. Core routes task/dependency reads and task/friction writes through
`OwnerCoordinator` over the federated SSH or in-process transport, with no fallback to a follower
store; owner identity and workspace are rechecked at dispatch. SSH initialization carries the
binding apart from tool arguments, and bound sessions and their proxies lose operator capability.

- Detached workers and provider subprocesses recover their invocation from the host
  recovery-authority database, keyed by kernel process identity (plus namespace-init identity in
  Linux PID namespaces), never from env labels or job inputs. A managed child without a binding
  refuses to start; unsupported platforms fail closed.
- Only task activity/automation writes cross the owner seam (run identity checked first); Git and
  worktree checks stay local. Generic updates cannot promote or complete a task. Source-file
  artifacts are materialized on the executor and routed path-free.
- Evidence, document and friction writes share one SQLite commit with the claim comparison and
  mutation receipt. Omitted `during_task` means the bound task; conflicting arguments refuse;
  canonical digests (excluding friction timestamps) deduplicate retries.

Payload host labels are diagnostic; session capability and attempt context are revalidated on
retries. An uncertain request stays pending until its receipt is reconciled and never licenses a
replacement. Receipt lookup preserves the original namespace and input across compatible upgrades
without granting execution authority; workers see their own namespace, owner operators all of
them. No inbound follower connection or fleet registry is required.

## 6. Execution provenance

Execution identity is required for claim fencing and handoff acceptance. The key is the stable
`machine_id` in an `ExecutionLocation { machine_id, machine_name }` (`machine_name` is display-only
and reads the legacy `host_id` spelling). Nothing is inferred from hostname, cwd, SSH target or
audit label, and absent historical identity reads as *unknown*, never as "the owner".

| Record | Field | Set by | Notes |
|---|---|---|---|
| Job run | `executed_on` | the runtime that inserts the run | immutable; steps inherit; nullable |
| Task | `job_run_machine` beside `job_run_id` | the pipeline that links the run, via the owner | a pulled task's run lives in the follower's store; the legacy `job_run_host` name is still read |
| Task history | `pulled_by` event | `orbit.task.pull` | request and claim identity |
| Task artifact | `origin` | the owner, at put time | from trusted invocation context; remote labels alone leave it unknown |

Until federated run inspection ([3_vision.md](./3_vision.md#1-open-questions)) exists, these
fields only say where to inspect a run manually.

## 7. Retirements and retained ship sweep

Epic execution and failed-run triage are retired; ship sweep is retained behind the shared
admission boundary.

### 7.1 Epic machinery

Retired by [ORB-12491] ([Epic is a tag, not a
pipeline](./4_decisions.md#epic-is-a-tag-not-a-pipeline)). Removed:

| Piece | Anchor |
|---|---|
| `epic_pipeline` job and its `list_epic_descendants` / drain loop / finisher steps | `crates/orbit-core/assets/jobs/epic_pipeline.yaml` |
| `epic_orchestrator` activity | `crates/orbit-core/assets/activities/` |
| `start_epic` step and `has_epic` / `epic_task_id` / `active_epic_run_id` outputs | `workspace_auto_pipeline.yaml`, `classify_workspace_auto_tasks.yaml` |
| Epic-tag exclusion from leaf admission and the refusal to ship an `epic`-tagged root | `list_backlog_tasks`, `classify_workspace_auto_tasks`, ship admission |
| Descendant-union footprint for `epic`-tagged roots | `crates/orbit-core/src/runtime/task/locks.rs::lock_context_files_for_task` — the `tags.contains("epic")` branch |
| Epic-specific worktree identity/GC handling | `crates/orbit-engine/src/executor/automation/vcs/worktree/mod.rs::WorktreeIdentity::from_input`, `crates/orbit-core/src/application/gc.rs::delivery_job_owns_worktree`; decoding needed to reap historical worktrees is preserved |
| `docs/design/resident-orchestrator/` | removed (history in git) |

Kept:

- **Parent/child relations**, for reading a backlog only; a child is an ordinary leaf.
- **The `epic` tag** as a size hint (one large task a top-tier crew takes whole). Admission
  ignores it except for one carve-out: a tagged root with no `context_files` of its own *while a
  descendant has some* is withheld, since nothing inherits the union it used to reserve. A tagged
  root with its own surface, or a tagged family with none anywhere, is an ordinary leaf.
- **`workspace_auto_pipeline`'s drain window, slot refill and detached leaves.**

**Migration check.** `application::epic_retirement::assess_epic_retirement` is a pure decision over
a tasks/runs/reservations snapshot; `OrbitRuntime::epic_retirement_readiness` is its read-only
gatherer. It refuses while any old epic execution, child execution, reservation or uncertain
landing is unreconciled, regardless of root status (a root in `review` can still have a live
`complete_pr` step), and reports inherited-only roots and the historical runs GC must still find.

**Enforcement.** `orbit_types::task::inherited_only_epic_roots` is the one definition of the
withheld population: `backlog_snapshot` excludes such roots (`inherited_only_epic_root`, naming
descendants and repair), `list_backlog_tasks` applies it to explicit ship overrides, and
`reserve_with_index` refuses them before the `EmptyTaskSurfacePolicy` compatibility `Admit`.

### 7.2 Failed-run triage

Retired ([Blocked tasks wait for a reader, not a
classifier](./4_decisions.md#blocked-tasks-wait-for-a-reader-not-a-classifier)): the owner cannot
see a follower's failed run, and automatic re-backlogging would hide host-specific failures.
Removed: `task_triage_pipeline.yaml`, the `list_triage_candidates` / `triage_failed_runs` /
`apply_triage_dispositions` activities, the triage recursion guard in
`application/automation/incidents.rs`, and the seed entry. The routine survives only as
`routines/retired/task_triage.yaml`, a provenance shape so `orbit workspace sync` can tell a
seeded copy from an operator's (`RETIRED_ROUTINE_FILES` in `application/routine.rs`).

Nothing automatic replaces it: a failed run parks its task in `blocked` with the failure and
`job_run_machine` attached, and re-backlogging is a deliberate transition by whoever read it.

### 7.3 Ship sweep

Retained ([Ship sweep remains an admission entry
point](./4_decisions.md#ship-sweep-remains-an-admission-entry-point)): the seeded `ship_sweep`
routine, `workspace_ship_pipeline`, and the registry-driven `orbit run ship-sweep` CLI with its
`workflow.auto_ship` opt-in and external scheduler support. Existing enablement and schedules are
preserved; nothing is enabled or scheduled by this design. Scheduled invocation confers no
completion authority, and landing never depends on a sweep.

[ORB-12500] put every retained entry point behind one decision,
`OrbitRuntime::drain_entry_admission`, taken before dispatch: `orbit run ship` (and the MCP and
dashboard surfaces behind `submit_ship_run`), `orbit run auto`, and `orbit run ship-sweep`. The
seeded routine and `workspace_ship_pipeline` reach it through the drain beneath them. It reads and
reports:

- **Destination authority.** A replica serves no owner coordination from any entry point; the
  unattended sweep reports this before reading a backlog. Followers execute through pull.
- **One capacity reading.** `JobRunStoreBackend::drain_leaf_occupancy`, as in §3. Saturation
  stands the *unattended* sweep down, not an operator's explicit invocation, which its own leaf
  definition bounds.
- **The claim ledger.** A claimed task is never admitted again: `submit_ship_run` and explicit
  overrides in `list_backlog_tasks` consult the ledger. Automatic discovery need not, since a
  claimed task is `in-progress` or `review` (a status lock holder) and the journal refuses ordinary
  mutations of it.

It also reports the owner's review policy and the verdict of `orbit_store::admission_refusal`,
without raising it: v1 admits only `none` through the claim contract, while a workspace configured
for `before-pr` or `after-landing` keeps shipping through its legacy leaf and review gate.

## 8. Required validation scenarios

Acceptance criteria, not reported as passing.

| Scenario | Required result |
|---|---|
| Concurrent pulls and ordinary task/reservation writes | One current claim per task; no overlapping admission; readiness revalidated transactionally |
| Pull commits, response lost | Same request returns the same claim; no second task consumed |
| Idle result replayed after new work arrives | Same request stays idle; a new poll may claim |
| Crash before/after local run creation or before binding response | Same claim reconciled; at most one leaf per claim; pending admission holds capacity |
| Invalid dependency or empty lock surface | Diagnostic exclusion; other eligible tasks progress |
| Reservation expires during valid execution | No automatic revocation or duplicate admission; status lock remains |
| Old worker returns after recovery and reassignment | Cannot bind, mutate evidence, promote, settle, or release the new reservation |
| Failure/cancellation while owner disconnected | Settlement stays pending locally; later idempotent settlement or explicit recovery |
| Detached child or in-run step retry reads a task | Owner routing and claim context survive; no local fallback |
| Generic resume of an interrupted claimed leaf | Refused; recovery creates a fenced new claim/run, preserving branch evidence |
| Handoff commits, response lost | Exactly one handoff and review transition |
| Review-only handoff reaches the landing consumer | No merge without recorded authorization |
| PR head/base changes or conflicts | Stop with evidence; fresh validated repair required |
| Merge succeeds, owner crashes before completion | Reconcile pinned PR and merge evidence before done or retry |
| Recovery with an uncertain external merge | Reassignment waits for merge-intent reconciliation |
| No-diff/already-landed delivery | Typed evidence and completion authority still required |
| Authorized handoff with no drain or ship sweep running | One pending landing-start request survives restart and is dispatched once; review-only work has none |
| Retained routine, wrapper, CLI ship-sweep, explicit owner drains | All take common admission; enablement retained; none grants merge rights or bypasses slot accounting |
| Epic retirement with active old runs, including roots in review | Migration refused until execution and reservations are reconciled |
| Missing file selector, then reservation expiry | Full declared footprint stays protected |
| Truly empty legacy task/epic context | Diagnostic with repair; no guessed or inherited surface |
| `none`, `before-pr`, `after-landing` policies | Only `none` admits; typed not-required handoff needs no reviewed SHA or artifact |
| Owner-local task without origin | Local candidate handoff and authorized local landing; no PR or remote credentials |
| SSH session and managed worker invocation | Session capability gates operator actions; managed runs never propagate operator authority; payload labels cannot replace claim/run authority |
| Revocation during a live attempt | Revoked attempt cannot bind, mutate, settle or promote; its receipt reports the revoked phase |
| Probe and receipt lookup after binary upgrade | No admission side effects; original outcome found without rewriting input; incompatible protocol refuses; `not_found` licenses no replacement |
| Approve twice, or revoke before merge | Operator required; one immutable authorization/start request under concurrent retries; revoked or stale candidate cannot publish merge intent |
| Missing, failed, replaced or wrong-candidate validation artifact | Acceptance, approval and merge-intent publication rejected with no partial effects |
| Idle polling and receipt compaction | One idle request per pass; tombstones cannot re-admit; unsettled receipts kept; growth metrics visible |
| Mixed legacy and claimed admission under one ceiling | One occupancy reading; wrapper → gate → queued claimed leaf is one slot; terminal leaf holds its slot until settlement; `max_active_runs` per definition |

## 9. Concerns & Honest Limitations

- **Receipt metadata grows** as permanent tombstones (§2).
- **No automatic review.** Only `review_policy = none`; other policies need a protocol extension.
- **Manual recovery limits availability.** A dead or unreachable follower can hold its task's
  footprint indefinitely; claim age and TTL are diagnostics, not failure detectors.
- **Revocation cannot stop remote compute or retract external writes.** An old attempt may push
  or open an orphan PR; fencing keeps it out of delivery, and GC does not delete remote branches
  or close PRs.
- **No heterogeneous eligibility.** Every participant must meet the workspace's full execution
  requirements; auth probes alone cannot establish that a Mac and a Linux host are
  interchangeable.
- **Coordination requires the owner.** Task reads, evidence writes, settlement and handoff stall
  during partitions; large tasks hold their slots for as long as they run.
- **One owner is an operator prerequisite** (§1): two stores can admit overlapping work, so the
  demoted host's drains and coordination routines must be quiesced before replica pull.
- **Throughput is not guaranteed to scale**: shared CI, provider limits, file conflicts, serial
  landing and per-host compiler caches.
- **Auto-task minting stays owner-only.** Claim-scoped friction writes route to the owner.

## Task References

- [ORB-12488] — authored this design folder.
- [ORB-12490] — replaced context pruning with `declared_context_files`.
- [ORB-12491] — retired epic machinery.
- [ORB-12495] — added the probe, receipt lookup and claim listing.
- [ORB-12500] — added published-PR observation and the shared entry-point admission decision.
- [ORB-12516] — added owner dashboard claim state and actions.
- [ORB-12528] — landed the task/reservation commit boundary.
- [ORB-12582] — moved the probe's capability floor into the governed-operation registry.
- [ORB-12616] — made the owner-local claimed leaf executable.
- [ORB-12617] — unified capacity accounting and added fault-injection acceptance fixtures.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
