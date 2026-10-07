---
title: Distributed Drain — Design
owner: claude
last_updated: 2026-10-06
last_validated: 2026-10-04
status: Draft
feature: distributed-drain
doc_role: design
type: design
summary: "One owner, multiple execution hosts: idempotent claims, routed authority, manual recovery, explicit landing, retained ship sweep, none-only review, and non-pruning context footprints."
tags: [distributed-drain, multi-host, pull, federated-mcp]
paths: ["crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml", "crates/orbit-core/assets/jobs/task_pr_pipeline.yaml", "crates/orbit-core/assets/activities/classify_workspace_auto_tasks.yaml", "crates/orbit-core/src/runtime/task/locks.rs", "crates/orbit-cmd/src/registry/runtime/mod.rs", "crates/orbit-mcp/**"]
related_features: [distributed-drain, federated-mcp, host-registry, activity-job, policy-sandbox]
related_artifacts: [ORB-12488, ORB-12516, ORB-12582, ORB-12616, ORB-12968, ORB-13625, ORB-13642, ORB-13663, ORB-13941, ORB-13992, ORB-14149, ORB-14247, ORB-14260]
---

# Distributed Drain — Design

> **Status: Draft, live.** Live on the owner: the claim/admission substrate, owner-local
> claimed leaves, handoff acceptance and approval, the landing consumer, the probe and claim
> listing, dashboard actions, shared capacity and entry-point admission, and — since [ORB-13625]
> — the registered `orbit.task.pull`, `orbit.drain.claim.bind` and `orbit.drain.claim.settle`
> tools. Live on a replica: the routed follower peer and `orbit run auto --pull`
> (`workspace_pull_pipeline`). `application::distributed::DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED`
> is `true`; it remains the one source switch every mutating entry point names.

This doc covers v1: one owner checkout, any number of replica checkouts on other hosts, each
replica running the drain in pull mode against the owner, and the machinery the shape retires.
Deferred to [3_vision.md](./3_vision.md): a cloud-offloaded owner store, crew auto-assignment,
and follower-side merge. Rationale lives in [4_decisions.md](./4_decisions.md).

## 1. Roles: one owner, N followers

Roles are the host-registry catalog roles, unchanged. The owner checkout has no checkout-level
`owner_machine_id` (its logical owner is the local `machine_id`); a follower is a replica checkout
whose `owner_machine_id` names the owner machine. `RegisteredRuntimeFactory` carries the replica
owner into Core's coordination-write guard
(`crates/orbit-cmd/src/registry/runtime/selection.rs::replica_owner_for_checkout`), and
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
immutable receipts and claim lock surfaces on that journal and shares its ordering with readiness
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

- `context_files` are optional for local auto, ship (including explicit selection), and owner
  pull admission. A backlog task with no selectors is admitted on the next pass without holding
  a context lock; it needs no `no-diff-expected` tag or pilot preparation. Conflicts from undeclared
  edits are handled at landing by rebase and conflict repair. Remote pull claims tagged
  `no-diff-expected` work like any other: the claimed leaf hands off `NoDiff`
  ([task-pull](./specs/task-pull.md)). Work that must stay on the owner is pinned with an
  `os:` tag or a crew, not by this tag. Invalid declared selectors still fail canonicalization.
  The v2 reservation path admits an empty surface as a no-op, while `orbit.task.locks.reserve`
  refuses an empty task-scope reservation.
- Context is never pruned by filesystem existence. Selectors are canonicalized and held to
  repository boundaries, but selectors for not-yet-created files and symbols are preserved;
  `allow_missing_context` governs explicit operator existence checks and records the exact
  creation intent task-pilot honours; it never affects admission. Admission, reservation,
  status locks and task reads share `runtime/task/mod.rs::declared_context_files` [ORB-12490].
- The original canonical footprint is immutable in the admission receipt. The live claim
  protects it through execution and review, including after reservation expiry; only owner-validated
  widening at handoff may add selectors, and checkout contents cannot shrink it.
- Previously pruned declarations are restored from task history where possible
  (`application/task/context_repair.rs`, via `orbit task lint --restore-pruned`) or reported for
  operator repair, never guessed.

**Preparation holds.** A successful prepare checkpoint of a pending, running, or
retrying task-pilot run withholds every task it prepared from local auto and ship
selection, readiness, and owner pull admission. Pull records the hold in
`deferred_conflicts` and excludes it from queue depth. A terminal pilot run releases
the hold, including after owner reconciliation. The checkpoint is an advisory
selection hold, so an admission racing its publication still settles the pilot's
old assessment as `superseded` without an ordinary write to the claimed task.

**Eligibility.** Pull filters on the executor's host OS and its crews. The OS filter
[ORB-14005]: each request carries the executor's OS (`AdmissionRequest::os`, protocol revision
4), and the owner skips a ready candidate whose `os:` tags (`os:linux`, `os:macos`,
`os:windows`; several mean any one) name no OS the executor runs, recording it in the receipt's
`os_unavailable`. The tags are parsed once, by `orbit_types::task::TaskOsRequirement`, which the
owner's local drain, ship discovery and `orbit run ship` read too, so a task no current host can
run stays in the backlog with the wait named rather than being claimed and failed. The crew
filter [ORB-13941]: each
request carries the executor's crew capability (`AdmissionRequest::crews`), and the owner skips a
ready candidate whose crew the executor cannot run — its own `task.crew`, or the executor's
`default_crew` for a task naming none — recording it in the receipt's `crew_unavailable`
diagnostics. A skipped task stays in the backlog for the owner or another follower; the owner's
order is otherwise unchanged. The capability comes from two sources, both scoped to the drain's
window (one `workspace_pull_pipeline` run):

- *Window preflight.* The first refill pass resolves every configured crew the way dispatch would
  — enabled, its provider's executor resolvable, and that executor's CLI found where a leaf would
  launch it — and stores the result on the drain's run state (`pull_crew_preflight`). It starts no
  provider process. No shipped provider has a side-effect-free authentication probe, so an
  unauthenticated CLI passes the preflight and is caught by its first claimed leaf (below).
- *Provider-unavailable exclusions.* A crew whose claimed leaf could not use its provider is
  excluded for the rest of the window. The exclusions are derived each pass from this drain's own
  admission records, so they survive a follower restart and end with the drain.

A request without `crews` (owner-local admission) is unrestricted, as before. Crews are matched
by registry name, so a crew both sides configure must use the same name.

*Operator restriction* [ORB-14174]. `orbit run auto --pull <selector> --allow-crew a,b` narrows the
declared capability for one drain. Submission canonicalizes the names against this host's
`[crews.*]` (an unknown or blank name refuses before the probe) and persists them as
`allowed_crews` in the `workspace_pull_pipeline` run input, so every refill pass and a resumed run
read the same set. Each pass declares only the preflight-runnable crews the allowlist permits (by
name or provider/model identity), minus exclusions; a pass with nothing left refuses
`no_runnable_crew` naming `--allow-crew` and requests nothing. The wire shape is unchanged: the
owner sees a smaller `runnable` list, never an exclusion. The restriction selects implementation
crews only. The owner's before-PR review crew is judged against the unrestricted window, so it
still has to run here (`before_pr_reviewer_unavailable` otherwise) but need not be named, and the
restriction is not forwarded to claimed leaves. No configuration, credential, owner pool or task
crew changes.

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
  candidate that does not descend from the base, and requires the checked-out branch and HEAD to
  match the candidate with no staged, tracked or untracked changes. It runs the owner's
  `workflow.required_validation_commands` on that exact candidate and checks the same Git state
  after each command. Passing logs are attached to the owner's task through the routed
  coordination transport only after all commands pass without changing the candidate. Git-ignored
  build output is allowed. An empty requirement list is no required check: `claim_validate` runs
  nothing and records `skipped_no_required_commands`, and the owner accepts the handoff with no
  validation logs, as `candidate_validate` does on the owner's own delivery path. Commands
  run in the executor's own resolved toolchain environment (`workflow.validation_env`); one that
  fails for lack of a tool is a typed `validation_environment` failure, which no step or final
  recovery repairs ([ORB-13987]). Before the claimed PR leaf pushes, `validate` runs the commands
  with no `pull_request`. That run attaches nothing and returns `publication: "pending"` with the
  captured results. A failing candidate therefore never publishes a PR. After `pr_open`,
  `pin_validation` passes the pending result back as `prevalidated`. It re-observes the
  candidate, now with its PR delivery, and requires the same commit, base and command list. It
  then attaches the logs without running the commands again. A command that fails the same way on
  the candidate's base is a typed `baseline_red` failure. The drain releases the claim with
  `ClaimEvidence.baseline_red`, and the owner records a `baseline_red_hold` in the task's history.
  Owner pull admission defers the task until the held command passes on a new
  base tip ([ORB-14258];
  [CONFIG.md](../../CONFIG.md#workflowvalidation_env--the-toolchain-required-validation-runs-with)).
- `claim_handoff` re-observes the same identity, refuses a worktree that moved or became dirty, and
  records the typed `TaskHandoff` as the claim's durable pending settlement *before* any owner
  call. The leaf's worker delivers it as the run terminalizes; a disconnect leaves one immutable
  settlement any later settle-only pass or refill retries idempotently ([ORB-13663]).

**Claimed-leaf crew.** The claimed task lives in the owner's store, so the leaf's run input carries
the owner's snapshot of it (`claimed_task: {id, crew}`, from the receipt's task summary). Crew
resolution reads the crew from that snapshot instead of the executor's local store, which lacks
the task or holds an unrelated record under the same id. Precedence matches a local run: an
explicit run `crew`, then the task's crew, then the executor's `workflow.default_crew` only when
the task names none. A task crew the executor does not configure, or has disabled, fails the leaf
at run start with an error naming the crew, before `implement_one`. It never falls back to the
default crew. System-crew activities such as the recovery hooks keep `workflow.system_crew`.

**Claimed-mode implementation** ([ORB-13642]). The implementer writes no owner task state. Both
claimed leaves pass `claimed: true` to `agent_implement`, and in that mode:

- The injected task envelope is the task. The claim is the authority to work on it, and the owner
  fences stale work when the claim settles (`stale_claim`). A read through `orbit.task.show` is
  scoped by the run broker to that claimed task.
- The CLI runner denies `orbit.task.update` on top of the activity's own
  list (`CLAIMED_MODE_DENIED_TOOLS` in `cli_runner/orchestrator/policy.rs`; an allowlisted activity has
  it removed instead). The prompt is not the only guard. The runner treats an invocation as
  claimed when the host carries the claim's trusted worker binding, or the step input says
  `claimed: true`. The binding covers every agent the leaf launches, so `step_failure_recovery`
  and `pr_conflict_recovery`, which the claimed leaves use as step recovery hooks and whose input
  never carries `claimed`, lose task updates too. A run outside a claim keeps its activity grant.
- The agent returns `execution_summary`, plus any `context_files_added` and `comment`, in the step
  output. The handoff step reads that output (`implementation: "{{ steps.implement_one.output }}"`,
  one iteration, since a claim binds exactly one task). `claim_handoff` composes the handoff's
  summary from it: an explicit `execution_summary` input, else the output's `execution_summary`,
  else its short `summary`, else a generic delivery statement. The implementer's comment and
  reported selectors are appended as prose; typed widening requests are independently derived
  from the final candidate rather than trusted from the implementer output, then a line naming the delivered candidate. Each part is bounded, and a summary whose
  first line is `Outcome: failed` is refused before it becomes a settlement. Acceptance writes it
  as the owner's `execution_summary`.
- A claimed implementer may change any path the work requires; the frozen footprint is a
  scheduling hint, not a delivery gate. Every added path the admission selectors do not cover
  becomes an owner-validated widening request for an exact `file:` selector. The follower
  recomputes `TaskHandoff.footprint_widening` from the final Git diff with rename detection
  disabled. Old payloads decode with an empty request; protocol revision 3 prevents newer peers
  sending the additive field to older endpoints.
  The owner independently reads the published candidate, refuses only paths no owner can track
  (Git or `.orbit` metadata, environment files, symlinks and malformed or redirected paths), and
  requires the request to equal its observed additions. Protected metadata names and environment
  patterns (including `.envrc`) ignore ASCII case on every host, so a Linux owner also refuses
  `.Orbit/`, `.GIT/`, `.ENV` and `.Env.local`. A competing live claim, in-progress or
  review selector, or reservation on an added path does not refuse it; a concurrent task meets
  the overlap as a rebase conflict instead. Acceptance journals the task selector additions, one
  `context_files_widened` history entry (step `implement`, activity `claim_handoff`), the enlarged
  live claim and handoff acceptance as one decision. The admission receipt remains unchanged.
  Refusals name exact paths and leave the task and claim unchanged. Scratch under `.orbit/tmp/`
  and gitignored output are never delivered. Owner-path runs widen the same way at delivery (activity-job
  design §7.6a, agent-changed paths).
- The delivery gate judges this attempt ([ORB-13755]). Until acceptance, the owner's stored
  summary is whatever an earlier attempt left, and after a failed attempt that is its
  `Outcome: failed` failure settlement. The `Outcome: failed` gate in `git_commit` and in the
  PR leaf's `pr_prepare`, `git_rebase`, `git_push` and `pr_open` (`load_handoff_context`) once
  read that stored summary, so every claimed retry of a once-failed task was refused at `commit`,
  however its new implementation went. Both leaves now pass those steps the same
  `implementation` output. Under the trusted worker binding, for the claim's own task, the gate
  judges that output's summary (the same text `claim_handoff` composes from), not the stored
  one. A current `Outcome: failed` is still refused before any Git mutation, and the claimed
  `git_commit` derives and writes no summary. An implementer that reported no summary is not
  refused, since its handoff carries the generic delivery statement. Without the binding, or on
  a step handed no implementer output, the durable ORB-10313 gate applies unchanged. The
  rejected alternative was having the owner clear the summary when it admits a claim. That
  erases the prior failure evidence, and it still leaves the leaf gating on owner state it
  cannot write.
- `step_failure_recovery` treats the implement step's output as the claimed task's summary of
  record and has only repair-and-retry: no direct delivery, resume or review transition.
- `pr_conflict_recovery` (the `sync_base` rebase hook) carries the same contract in its prompt: it
  identifies the task from `task_id` and `failed_step_input`, never reads the owner's task
  record, and treats an unreachable owner task store as no reason to stop.

This is what makes a follower leaf work under the agent sandbox, which denies `~/.ssh` and so
leaves a sandboxed agent no route to a remote owner. The sandbox is not relaxed and the agent is
given no owner transport of its own: the few claim-scoped owner calls it may make (the claimed
task's record and artifacts, a follow-up task `spawned_from` it, a friction) cross the run's
plugin broker outside the sandbox ([ORB-14260]; plugins
[2_agent_call_broker.md](../plugins/2_agent_call_broker.md) §3, "Claimed-owner calls").
Before this, every such agent finished its change, failed to save its summary through
`orbit.task.update`, and reported the step failed (`task_store_unreachable`), which blocked the
task with finished work uncommitted.

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
- the drain's worker limit is the only ceiling: the leaf definitions declare none of their own
  ([ORB-13893]), so `orbit run concurrency` retunes a pull drain exactly as it does an auto drain;
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

**Follower execution** ([ORB-13625]). `RoutedPullPeer`
(`adapter/engine_host/v2_host/pull/adapters.rs`) speaks the protocol to the owner's registered
tools over a composition-supplied `orbit_tools::DrainOwnerTransport` — in production the
federated mux over the host's registered hosts (`~/.orbit/hosts.toml`), opened as `agent` (`orbit-cmd`
`worker_coordination.rs`). The owner serves the same code the owner-local adapter calls:
`application::distributed::serve` resolves the caller machine from the trusted session, requires a
new request to carry the ship contract the owner resolves now (`ship_contract_mismatch`
otherwise), fences bind and settle through `ClaimInvocation` on that machine, and refuses a remote
`LocalCandidate` handoff. The follower's `LeafPullLauncher` routes its bound worker's coordination
through the same transport. Owner-local `run auto` / `run ship` still render a legacy pipeline
name for `pr` and `local` modes after taking the shared admission decision
([§7.3](#73-ship-sweep)).

**Refusal reconciliation.** An owner *answer* that refuses a request (a `RemoteTool` with
`invalid_input`, `capability_refused`, `capability_denied` or `policy_denied`) is reconciled
through `orbit.drain.receipt.lookup` before anything local changes: a found receipt is received
and carried forward (an earlier send committed, and a replay was refused, say after an owner
upgrade); an expired or absent one closes the local record as `Refused`, which releases its slot,
and the refusal ends that pass. A lost delivery, an unknown outcome or a store failure leaves the
request pending under the same ID. A refused *settlement* is reconciled the same way: when the
lookup shows the owner already ended the claim (revoked, failed or landed), or no longer holds it,
no settlement can ever be accepted, so the record settles locally with the refusal
(`LocalPullMutation::SettleObsolete`) and releases its slot [ORB-13639]. A claim the owner still holds keeps
its settlement pending. One record that cannot move forward does not stop the others from being
reconciled in the same pass, though its error still blocks fresh admission for that pass.

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
the streak. Protocol skew immediately latches degradation and ends the drain **failed** with `protocol_skew`,
even with an open window. `orbit doctor` reports the latest skewed pull drain, and the dashboard
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

### Pull-mode contract

`orbit run auto --pull <selector>` binds a local replica checkout to the owner's host-qualified
selector from federated discovery. Submission (`application::distributed::follower`) refuses unless
the checkout is a replica whose owner is the selector's machine and whose logical workspace is the
selector's, and the owner answers the probe as that machine and would admit this executor. An
empty `workflow.required_validation_commands` is noted, not refused. It persists the resolved `PullDestination` (owner
machine, the owner's workspace id as its probe reports it, selector, execution machine) in the
`workspace_pull_pipeline` run input. A renamed or unavailable destination never falls back to a
local coordination store. Each `pull_refill` iteration re-probes before allocating, so a changed
ship contract or version stops new requests without failing the drain. The run keeps iterating
after its window until every admission has settled. It is no longer the only process that settles
them ([Settlement belongs to the admission record, not to the drain that admitted
it](./4_decisions.md#settlement-belongs-to-the-admission-record-not-to-the-drain-that-admitted-it)):
each leaf delivers its own settlement as it terminalizes, and the drain's pass is one of several
idempotent deliverers.

A drain started without `--for` (or with `--for 0s`) has no window but one admission pass
[ORB-14174]. Its zero deadline still reads expired through the shared `drain_window`, which is not
special-cased; `pull_refill` instead takes the pass from run state. The first iteration that finds
no admission stop and no cancel atomically records `pull_single_pass` (one SQLite run-state
transaction) before it probes or requests, then tops up to the free slots; every later iteration,
a retry of that activity and a resumed run (which clones the run state) see the record and only
settle. The pass also ends early when a stop or cancel lands between two requests. Requests
allocated before a crash are carried under their recorded IDs, so a retried pass neither loses nor
duplicates a claim. A positive window that has expired never gains the pass, and a windowless
drain ends once nothing it admitted is unsettled, including at once over an empty backlog.

Admission:

1. Reconcile pending local pull requests and claimed-but-not-launched work first.
2. Count live leaf runs **and pending admissions without a live run** against local capacity,
   across all four leaf definitions, under the configured ceiling and each `max_active_runs`.
   Queued bound runs count until terminal settlement. Persist a new request ID per free slot
   before sending it.
3. Persist the handle. Create or recover exactly one local leaf per claim (durable local
   uniqueness), then bind its host-qualified run ID on the owner. Binding is idempotent and never
   replaces another run for that claim.
4. Launch only after binding. A crash between steps resumes the same request, claim or
   not-yet-started run. Stopping the drain stops new admissions, not live children. Cancelling it
   is graceful: it releases unlaunched admissions to the owner's backlog and waits for launched
   leaves to finish and settle before the drain ends; `--force` stops them instead (see
   **Cancelling a pull drain** below).

**Interrupted execution** is left for deliberate recovery. `orbit job resume` refuses claimed
leaves (`submit_resume_run` creates a new run that cannot inherit the binding). Recovery fences the old
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
`blocked`, invalidates execution authority and releases the reservation. That evidence names the
leaf run, its last failed step and the step's error (bounded), since the owner cannot read the
executor's run. The leaf's generic terminal hook (`block_on_run_failure`) leaves a claimed task
alone: the claim's failure settlement is the one failure transition the owner accepts for it, and a
generic blocked update carries no evidence (`failure settlement requires evidence`). Disconnected,
the settlement is persisted locally and retried; the owner holds the claim until settlement or
recovery. TTL is not settlement.

**Settlement ownership** ([ORB-13663]). The admission record is the outbox, and no single process
owns delivery:

- *Recording.* Run finalization (`finalize_job_run_with_cleanup_after_prior_read`), in whichever
  process terminalizes a claimed leaf — its worker, a cancel, orphan reconciliation — records the
  settlement a terminal leaf implies: a `Launching` or `Launched` leaf ran and failed; a `Bound`
  leaf never launched, so its claim is released (below). Success already recorded its handoff. A
  `Created` admission is left to a pass that binds it first, because the owner fences a settlement
  naming a leaf against the claim's binding.
- *Delivery.* The leaf's own bound worker delivers right after recording and, once its run is
  finalized, retries with backoff (15s, 60s, 240s) while the owner is unreachable and no live
  drain carries that owner. The worker is an
  unsandboxed Orbit process with the host's federated route; only the agent subprocess is
  sandboxed. Everything else is a settle-only pass (`OrbitRuntime::settle_pending_pulls`,
  `PullDrain::carry_settlement`) run by `orbit run cancel` / the dashboard's cancel (for a pull
  drain or a claimed leaf, including one already terminal) and by `orbit run auto --stop` / the
  dashboard's stop. A pass covers every owner and never requests work or launches a leaf. A live
  drain's refill pass delivers too, and first reconciles any launched leaf whose worker died, so
  an orphaned leaf's settlement is recorded and delivered without operator action. Once the
  worker and every drain are gone, the OS clock sweep retries: each tick also opens the host's
  replica checkouts (they fire only host-local worktree-GC routines; owner-work
  schedules stay inert) and runs a delivery-only pass
  (`OrbitRuntime::deliver_recorded_pull_settlements`). That pass delivers what is recorded and
  records a terminal leaf's settlement, but never ends unlaunched work.
  Whether it is the settle-only pass or the live drain's reconciliation, a pass costs at most one
  failed delivery per unreachable owner: after the first transport error to an owner it stops
  delivering to that owner for the pass, and the remaining admissions stay pending for the next
  one. A live drain's refill still carries every admission for its owner, whichever drain made it.
- *Abandonment.* For an admission no live drain will carry — its own drain ended and no live drain
  pulls from its owner — a pass also ends what was never launched: an unanswered request is reconciled against the owner's receipt, a claim
  with no leaf is released, and a queued leaf's release is recorded before the leaf is cancelled
  through ordinary run cancellation. Live leaves are never cancelled by this, and a settlement
  never closes a leaf that has started.
- *Release* ([ORB-13892]). Work a follower took but never ran is not a failure. The executor's
  `ClaimMutation::Release` (a summary naming the drain and why, and a comment) is accepted from
  `claimed` or `running`: the owner revokes the claim, releases its reservation, returns the task
  to `backlog` with the reason as its status note, and adds the comment to the task. Only a
  launched leaf that ended without its handoff settles as `Fail`; the breaker counts only those.
  The exception is a provider that could not be used [ORB-13941]: the CLI runner stamps a failure
  whose own stderr, terminal error, or structured provider failure reports an authentication failure with the typed
  `[provider_unavailable]` marker, and a launched leaf that ended on such a step settles as a
  `Release` carrying `provider_unavailable { crew, reason }`. The task returns to `backlog`, the
  breaker does not count it, and the drain excludes every configured crew that resolves to the
  same provider for the rest of its window — labels are parsed, so `anthropic` groups with
  `claude` — so the same login is not spent again on another crew of that provider. The named
  crew is the one the leaf resolved at start.
  Claude error results with HTTP 401/403 or authentication failure text, Codex error/failed-turn
  frames, and Grok/Gemini error objects are provider evidence, even when the CLI exits 0.
  Assistant transcripts, tool results, and Orbit work-failure envelopes are not provider evidence.
  A step ending with `provider_unavailable` skips step-failure recovery: signing in requires
  an operator, and a recovery agent cannot repair the provider credentials.
  A provider that reports its selected model at capacity on a failed exit (Codex's
  `Selected model is at capacity`, in its stderr, terminal error, or own failure frames) is a
  kind of unavailability [ORB-14149]: the runner stamps `[provider_capacity]`, which counts as
  `provider_unavailable` for the release, the failure class and recovery, so the leaf releases
  its claim and that crew is excluded for the window. It does not exclude the provider's other
  crews. Neither step recovery nor its post-recovery attempt reruns the same model, and
  final recovery is skipped too. Capacity reported mid-turn on a turn that then finishes is not
  provider evidence. An authentication failure stamped `[provider_unavailable]` also skips step
  recovery and final recovery; the final-recovery skip is audited as
  `job.final_recovery_attempted` with outcome `skipped`, because another agent cannot sign the
  provider in [ORB-14262].
  A claimed worker whose owner call could not cross the run's broker (missing or gone) gets the
  typed `owner_route_unavailable` and ends its step on that `error.code` [ORB-14260]. The runner
  stamps `[owner_route_unavailable]`, step and final recovery are skipped (nothing inside the
  sandbox can open the route), and the leaf settles as the `owner_route` class below. The code
  grants nothing an agent could abuse: declaring it only withholds repair and returns the task
  to the owner's backlog.
- *Typed failure class* ([ORB-14257]). Every launched leaf's settlement carries a
  `failure { class, reason, crew, candidate }`, and the class decides the owner's transition:
  `candidate` (the default: the candidate's implementation, checks or review failed) and
  `task_input` (final recovery decided `reject` or `archive`) settle as `Fail` and block the task.
  Every other class settles as `Release` and returns it to `backlog`: `operator_cancel` (an
  operator cancelled the launched leaf, with the cancel's reason; a cancel with `--block` settles
  as `Fail` and blocks the task [ORB-14274]), `provider` (the
  `provider_unavailable` case above), `environment` (`[validation_environment]`, a required
  command that lacked a tool), `owner_route` (`[owner_route_unavailable]`, which the worker
  route stamps on a call to the owner that never reached it — unreachable, owner unavailable,
  stale route or failed negotiation), `baseline_red` (the `[baseline_red]` failure with its hold,
  from `claim_validate`), `transient` (`[transient_failure]`, which `claim_validate` raises for a
  required command still network-inconclusive after its reruns on a base that is not red, or a
  leaf whose worker died and was reconciled `interrupted`) and `base_conflict` (a committed
  candidate that `sync_base` and its conflict recovery could not carry onto a base that moved).
  The class is read from the last failed step's typed marker, then any provider or red-base
  failure the run recorded, then the terminalizing diagnostic, then the leaf's progress.
  `operator_cancel` and `transient` exclude the leaf's crew for the rest of
  the drain's window (source `leaf_released`). `provider` uses source
  `provider_unavailable`: authentication excludes every configured crew of
  that provider, and capacity excludes only the leaf's crew. `environment` and `owner_route`
  are the host's own failures: they suppress the host for the window (`crews.host_suppressed`),
  so the drain requests nothing more whatever crew a task names, and the owner holds every task
  from that drain run. For any excluding class the owner's admission also holds the released
  task itself from that drain run (`crew_unavailable`), so a release that reaches the owner after
  the follower built its next request is not pulled straight back. `baseline_red` releases with
  its hold, and the owner's admission withholds the task until the base moves to a commit where
  the command passes. The owner bounds the releases: a task released twice within 24 hours for a
  budgeted class is blocked by its third, with one comment
  listing every counted reason; releases before such a block no longer count. A `Release` naming
  a class that blocks is applied as a block. `orbit run show <leaf>` prints the class on its
  `Claim:` line (`pull_claim.failure_class`).
- *Candidate continuation* ([ORB-14257], [ORB-14338]). When the leaf had committed a candidate,
  its failure names it (`failure.candidate`) with the source run and the step it stopped at: for
  a claimed PR leaf, the branch and head it pushed, with the pull request it opened
  (`published`), or before the push the branch tip its failure hook carried; for a claimed-local
  leaf, its worktree branch at the commit checkpoint. A claimed PR leaf's job-level
  `failure_activity`, `claim_candidate_carry`, runs when the leaf fails after its commit and
  before its own push: it pushes the tip of the branch the leaf synchronized or prepared (review
  fixes included) to `refs/orbit/candidates/<task>/<run>` on `origin`, and its checkpointed output
  says where the candidate is. The settlement names that ref (`durable_ref`), or, when the push
  failed, why (`carry_failure`); either way the candidate also stays on the follower that made it.
  A claimed-local leaf runs only on the owner, in the owner's repository, so its candidate needs
  no push. The owner keeps the reference with the task's spec digest and the machine that
  committed it, on a `Fail` as on a `Release`. Admission attaches the latest one to the claim
  (`task.resume_candidate`) unless the task's description or acceptance criteria changed (a
  selector edit keeps it), an operator discarded the candidate since, or the candidate is neither published nor
  carried and the claim runs on another machine, which could not fetch it. Each of those is
  recorded in the task's history in the admission transaction, as a `candidate_resume` event
  whose note starts `fresh:` and names the claim, machine, source candidate and typed reason
  (`spec_changed`, `discarded`, `not_durable`), so a fresh implementation is never silent. The
  follower passes the candidate to either claimed leaf. Its `resume_candidate` step
  (`candidate_resume` in claimed mode) fetches the candidate from `durable_ref`, or else its
  branch, when the object is not already local, squash-applies it onto the new base and hands the
  implementer a `continuation` repair (`review` when the before-PR review refused it, `conflict`
  when it no longer applies cleanly); the implementer always runs, and the leaf's own validation
  judges the result. A candidate that is not on `origin` or in the executing host's object store
  is implemented fresh with the reason in the step's output. Carried refs are not deleted by
  Orbit; the runbook covers pruning them.
  A `Release` for a leaf that is still running (not `pending`, not terminal) is *held*: no pass
  delivers it until the leaf is seen to stop. The task is never back in the backlog while its
  first executor may still be working.
- Two processes delivering the same settlement is safe: the first recorded value is immutable
  locally, and the owner's per-claim mutation IDs make a second delivery a replay.

**Cancelling a pull drain** ([ORB-13892]). `orbit run cancel` on a running pull drain records a
`drain_cancel` request on the drain's state (actor, source, reason, time), which also stops
admissions, and returns `cancelling` with the claimed leaves still running. The drain's own next
refill pass sees the request and runs a settle-only pass in place of a refill: unanswered requests
are reconciled (or withdrawn when the owner holds none), unlaunched claims and queued leaves are
released, recorded settlements are delivered. When nothing it carries is unsettled, the pass ends
the drain `cancelled`, audited under the requesting actor; the engine's own finalization leaves
that state alone. `--force` instead cancels the drain at once and releases everything unlaunched.
For each launched live leaf it records `Release`, then cancels the leaf, stopping its process
group. A leaf that already recorded its handoff keeps it. Force acts only on what the drain carries:
the admissions it made and, while it was live, those for its owner whose own drain had ended.
Never another live drain's. The set is read before the drain stops. Before recording anything,
force asks whether the leaf's worker can be signalled and seen gone. A worker in another PID
namespace, with an unverifiable identity, or in this process's own group is refused. Its claim
stays, and the leaf is reported in `unstopped_leaves`, which fails the CLI (exit 1) and the MCP
stop. The leaf's cancel accepts only a confirmed stop: an unconfirmed one fails before the run is
finalized, so its recorded release stays held, as above. The MCP stop control
(`orbit.workflow.auto`, `action: stop`) takes `force` and applies the same cancel to each live
drain after stopping its admissions. A queued drain, or one whose worker is gone, has
no pass to wait for and is cancelled immediately. The owner's local drain keeps detach-on-cancel;
its `--force` cancels the detached children too, accepting only confirmed worker stops.
An unconfirmed or failed child cancellation is reported in `unstopped_children` with its run
ID and reason, alongside the successfully stopped `forced_runs`; it fails the CLI (exit 1)
and the MCP forced-stop control.

**Branches** carry attempt identity: the run-derived branch suffices because each claim binds
one run; `orbit/<task-id>-<claim-id>` is also valid. The follower implements, validates, pushes and
opens the PR, then hands off; it never runs merge completion.

### 3.1 Attempt ownership and recovery

Claim phases are `claimed`, `running`, `handed_off`, `repair_pending`, `failed`, `revoked` and
`landed` (`ExecutionClaimPhase`). Only `claimed` and `running` authorize execution writes;
`claimed`, `running`, `handed_off` and `repair_pending` protect the footprint. Current claim ID, trusted runtime machine, bound
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
tasks survive reservation expiry using the live claim footprint. A task tagged `no-diff-expected`
is not one of those holders ([No-diff-expected work does not hold context locks](./4_decisions.md#no-diff-expected-work-does-not-hold-context-locks)).
Readiness, `list_backlog_tasks`, and `reserve_locks` treat its context as unlocked, so an
overlapping backlog task stays eligible and a drain can admit it. The tagged task still waits on
its own dependencies, on locks other tasks hold, and on its claim. Its own `reserve_locks` grant
checks the original context against persistent file reservations and frozen claim footprints at
the serialized store boundary, then records a reservation with no files. The file-reservation
conflict check and insert share one SQLite transaction. Release still has an id and the grant
does not serialize anyone else. An unexpected diff is not refused: `git_commit` commits a
non-empty stage and `sync_base` reports a recoverable rebase conflict the same way it does for
any other shipment.

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

- The owner captures its `review.before_pr` contract at admission. When that policy is on, the
  owner admits only PR-mode executors that declare `review_gate`; the executor's captured
  `caller_before_pr` is diagnostic. Local ship mode is refused because it has no claimed leaf on
  which to run the gate. After-landing review is the owner's `delivery-code-review` auto-task and
  never affects admission. Its `deliveries_landed` batches list a landed follower PR under the
  claimed task, which the owner reads from the handoff it accepted for that repository, landing
  branch and PR number [ORB-13894]. The handoff carries typed before-PR review evidence when the
  captured contract requires it, or `{ policy: none, disposition: not_required }` otherwise. Task
  status `review` means a delivery handoff awaiting completion authority, not that a review
  occurred. The claimed-leaf gate and its owner-captured policy are described in
  [the decision](./4_decisions.md#a-claimed-leaf-runs-the-before-pr-review-its-claim-captured).
- Validation runs on the exact candidate/base pair and refuses staged, tracked or relevant
  untracked candidate changes before checks, after each check and at handoff. Artifacts must live
  in the owner's store; a follower-local path is not evidence.
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
authority, persisted with the handoff and rechecked at landing. The one such authority is the
owner's `workflow.distributed_completion = "done"` [ORB-13637]. Admission pins it as the contract's
`authorization_reference`. Acceptance records it as an `owner_policy` authorization only while the
owner's own configuration, observed for that decision, still grants it, and dispatches landing.
Landing rechecks it before merge intent and completion.

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

A stop on a base conflict (`DIRTY`), a stale base (`BEHIND`) or a local candidate that can no
longer fast-forward is repairable ([ORB-14261]). The stop revokes the handoff's authority, moves the
claim to `repair_pending` and returns the task to `in-progress`, still holding its footprint. The
next pull by the claim's original executor — or, once 30 minutes have passed, the owner's own local
drain; never another follower — admits the repair before any backlog task: a new claim carrying
`ClaimRepair` (the superseded claim, its handoff, the preserved candidate and the stop evidence),
with the superseded claim settled as `revoked` and a `pulled_by` event naming both, in one commit.
The pull's ship contract must match the candidate's base, landing branch and delivery route. The
claimed leaf receives `claim_repair`; its `resume_candidate` step squash-applies the candidate onto
the leaf's fresh base and hands the implementer a `conflict` or `landing` repair, and the leaf then
commits, rebases, revalidates and hands off again under the same task. `claim_repair` takes
precedence over a kept `resume_candidate`, and a repair whose candidate cannot be restored fails the
leaf rather than implementing afresh. Only one automatic repair is
allowed: a repairable stop on a repair claim fails it and blocks the task with a comment carrying
both attempts' claim, handoff, candidate and stop evidence. A repair leaf publishes from its own
run branch, so the superseded pull request is left open for an operator to close.

`handoff_land` reuses `pr_complete`'s pinned delivery identity (branch, base and head-commit pins,
merged-with-merge-commit evidence, provider-state classification) with no follower run or path.
The owner checks the head on every poll, resolves candidate and base in its own checkout, and
verifies the validated base is reachable from `origin/<landing branch>`, fetched once from `origin`
per attempt so a commit landed from another machine is not misread as missing. Changed identity, a
conflict, unsatisfied protection or an exhausted check budget records a durable stop and leaves the
task in `review`, except that a repairable stop starts the automatic repair above. Owner-local candidates fast-forward the local landing branch and are verified
from the ref; before the branch moves, the owner retains a direct landing intent naming the
handoff's task, so delivery consumers attribute the fast-forward. No-diff delivery verifies its covering commit on the landing ref with no external
call. Every completion re-runs authorization, candidate and validation checks inside the
`review → done` transaction, and the activity's observation must equal the accepted candidate.

### Owner dashboard surface

[ORB-12516] added claim state and actions to the owner's existing task and run views. There is no
distributed tab or route; the panel appears on a task detail only when this workspace holds a
claim for it, and a replica says the owner holds that state.

- **Read:** one owner projection (`application::review::handoff`) of execution machine, claim
  phase and bound run, current lock footprint, reservation expiry, the accepted handoff and its
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
| Required crews and providers available and authenticated | The window's crew preflight (section 2, *Eligibility*); the first typed `provider_unavailable` authentication leaf excludes every crew of that provider |
| Binary version and type-derived pull request fingerprint match the owner | Owner read-only capability/version response; pull enforces parity again |
| Workspace identity, SSH owner access, and session capability match | Federated discovery and the read-only probe below; never call pull as a health check |
| Owner's before-PR policy and executor gate capability | The owner captures the review contract at admission; when before-PR is on, only a PR-mode executor that declares `review_gate` is admitted |
| Sandbox and required OS/toolchain capabilities available | Existing doctor checks plus workspace execution prerequisites |
| Repository readable and credentials configured for push and PR operations | Git transport checks and provider authentication; `gh auth status` alone does not prove Git push permission |

The owner resolves ship mode, base/landing branches and completion authority. Follower execution
always stops at handoff; local ship mode is refused for followers. Equal binary/schema versions do
not imply equal crew, policy or toolchain configuration. Crews may differ: a follower declares
what it can run and the owner admits only that. Policy and toolchain must still match.

### 4.1 Read-only admission probe

The owner serves a read-only probe ([ORB-12495]). Input: the host-qualified workspace selector.
Response: owner/workspace, binary version, type-derived request fingerprint, legacy protocol revision, effective
session capabilities, diagnostic caller machine, resolved ship mode and `review`: both review
switches with their sources (before-PR on/off and minutes; after-landing enabled and its next batch
due). It creates no receipts, reservations, claims or tasks. A caller may declare its version,
protocol schema and `caller_before_pr`, and the probe reports the first refusal admission would raise by running the same
ordered ladder (`orbit_store::admission_refusal`).

- Protocol revision `2` introduced executor crew capabilities. Request field compatibility,
  including optional fields and nested types, now uses the derived schema fingerprint. The follower
  compares the owner probe's fingerprint before sending admission fields; typed `protocol_skew`
  names both fingerprints.
  Matching crate versions alone are insufficient on development branches. This is not the
  scoreboard's `ORCHESTRATION_SCHEMA_VERSION`, and MCP initialization metadata is insufficient.
- The owner's read-only surface is `orbit.drain.probe`, `orbit.drain.receipt.lookup` and the
  operator-only `orbit.drain.claims` listing ([§3.1](#31-attempt-ownership-and-recovery)); its
  executor lifecycle is `orbit.task.pull`, `orbit.drain.claim.bind` and `orbit.drain.claim.settle`
  ([ORB-13625]). MCP and `orbit tool run` reach them through one registry and
  `application::distributed`. All six are `control_plane`, so a replica refuses them. Approval,
  revocation and recovery stay dashboard-only owner-operator actions.
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
| Review policy | The owner captures the before-PR contract; a declared claimed-leaf gate runs on the executor, and typed review evidence or a not-required disposition is verified and retained by the owner |
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
| Task | `job_run_machine` beside `job_run_id` | the pipeline that links the run, via the owner | a pulled task's run lives in the follower's store; run ids are unique per machine, so run-keyed task lookups match both fields; the legacy `job_run_host` name is still read |
| Task history | `pulled_by` event | `orbit.task.pull` | request and claim identity |
| Task history | `candidate_resume` event | owner admission | a kept candidate the claim implements fresh instead: claim, machine, source candidate, typed reason |
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

It also reports the owner's review switches and the verdict of `orbit_store::admission_refusal`,
without raising it: when the captured owner contract has `review.before_pr` on, only a PR-mode
claim whose executor declares `review_gate` is admitted; local ship mode is refused because it
cannot run the claimed-leaf gate. After-landing review does not change the verdict.

### 7.4 Host shutdown hold

[ORB-12968] A host with a shutdown or reboot scheduled kills every run started before it, so
unattended admission holds new work while one is pending. The signal comes from a host-signal
probe on `OrbitRuntime` (`runtime::host_signal`). On Linux it reads logind's
`/run/systemd/shutdown/scheduled` (`USEC=`, `MODE=`), which is unprivileged and is the state behind
the `org.freedesktop.login1.Manager.ScheduledShutdown` D-Bus property. `shutdown -c` removes the
file, and `/run` is a tmpfs, so the hold lifts on its own either way. A `dry-*` mode
(`shutdown -k`), a missing file, an unreadable file, and a malformed file all mean "nothing
scheduled". Other platforms have no probe and never hold. Tests inject a fixed probe
(`with_host_signal_probe`; the sweep takes one explicitly), and orbit-core's unit-test build never
reads the real host. There is no lead window: the hold starts as soon as a schedule exists.

- **Scheduler tick.** `run_sweep_at_with_providers` fires no routine (cron, delivery, or state) and
  mints no auto-task. Each routine row reads `skipped` with a `host_shutdown_scheduled: …` reason
  naming the mode and time, and every tick logs `sweep.host_shutdown_hold`. Cursors do not advance,
  so each routine's `missed_run` policy decides what happens to the held slots once the hold lifts,
  as after any other downtime.
- **Drain waves.** `classify_workspace_auto_tasks` sets `free_slots` to 0, reports
  `host_shutdown`, and admits no leaf until the schedule is gone. It does not signal or cancel live
  children.
- **Entry admission.** `drain_entry_admission` refuses an unattended caller (`orbit run
  ship-sweep`) with `host_shutdown_scheduled`. Explicit operator commands (`orbit run ship`,
  `orbit run auto`) are admitted with a warning and `host_shutdown` on the decision. That is the
  documented override: an operator who ships one task during the window chooses to risk it. An
  explicit drain started during a hold still holds its own waves.
- **Surfaces.** `orbit run readiness` names the schedule and gives every waiting backlog task the
  reason `host_shutdown_scheduled`. `orbit doctor` reports a `host-shutdown` warning.

## 8. Required validation scenarios

Acceptance criteria, not reported as passing.

| Scenario | Required result |
|---|---|
| Concurrent pulls and ordinary task/reservation writes | One current claim per task; no overlapping admission; readiness revalidated transactionally |
| Pull commits, response lost | Same request returns the same claim; no second task consumed |
| Idle result replayed after new work arrives | Same request stays idle; a new poll may claim |
| Crash before/after local run creation or before binding response | Same claim reconciled; at most one leaf per claim; pending admission holds capacity |
| Invalid dependency or invalid lock surface | Diagnostic exclusion; other eligible tasks progress |
| Reservation expires during valid execution | No automatic revocation or duplicate admission; status lock remains |
| Old worker returns after recovery and reassignment | Cannot bind, mutate evidence, promote, settle, or release the new reservation |
| Failure/cancellation while owner disconnected | Settlement stays pending locally; later idempotent settlement or explicit recovery |
| Pull drain cancelled while its leaves are live | Graceful: unlaunched claims return to `backlog` with a comment; launched leaves finish and deliver, then the drain ends `cancelled`. `--force`: the drain's own launched leaves are released and stopped, their tasks return to `backlog`; another live drain's leaves are untouched; a leaf whose stop is unconfirmed keeps its claim and fails the cancel. No claim is left `running` without a responsible follower process ([ORB-13663], [ORB-13892]) |
| Forced release while the owner is unreachable | The release stays recorded; the clock sweep delivers it once the owner answers, with no drain running ([ORB-13892]) |
| Detached child or in-run step retry reads a task | Owner routing and claim context survive; no local fallback |
| Claimed implementer with no SSH route to the owner (agent sandbox) | Scoped owner calls cross the run broker; task updates stay denied, and the output summary reaches `claim_handoff` as the owner's `execution_summary` |
| Claimed agent's owner call from inside the sandbox | Only the claim-scoped owner calls cross the run's broker; another call, task or relation is refused before the owner; a missing broker is `owner_route_unavailable`, settled as a release ([ORB-14260]) |
| Claimed run creates files under an admitted `dir:` selector | Committed and handed off with no exact `file:` selector; eligible additions outside the original module footprint request owner-validated widening; ineligible paths are refused before any index change; `.orbit/tmp/` scratch is never delivered ([ORB-13756]) |
| Claimed retry of a task whose previous attempt failed | The delivery gate judges this attempt's implementer summary, not the stored `Outcome: failed`; a current failure is still refused before any Git mutation ([ORB-13755]) |
| Generic resume of an interrupted claimed leaf | Refused; recovery creates a fenced new claim/run, preserving branch evidence |
| Handoff commits, response lost | Exactly one handoff and review transition |
| Review-only handoff reaches the landing consumer | No merge without recorded authorization |
| PR head/base changes or conflicts | Stop with evidence; fresh validated repair required. A base conflict or stale base gets one automatic repair claim that re-hands off; a second blocks with both attempts' evidence ([ORB-14261]) |
| Merge succeeds, owner crashes before completion | Reconcile pinned PR and merge evidence before done or retry |
| Recovery with an uncertain external merge | Reassignment waits for merge-intent reconciliation |
| No-diff/already-landed delivery | Typed evidence and completion authority still required |
| In-progress `no-diff-expected` task overlaps a backlog task | Backlog task stays eligible; no `context_lock_conflict` names the tagged task; ordinary overlaps still conflict ([ORB-14247]) |
| Critical or high-priority task waits on several locks that free one at a time | It reserves its surface: lower-ranked backlog work overlapping it waits as `surface_reserved` naming it, non-overlapping work admits, and it takes the wave once its locks free. At most two reserve per pass; nothing persists between passes ([ORB-14310]) |
| Authorized handoff with no drain or ship sweep running | One pending landing-start request survives restart and is dispatched once; review-only work has none |
| Retained routine, wrapper, CLI ship-sweep, explicit owner drains | All take common admission; enablement retained; none grants merge rights or bypasses slot accounting |
| Epic retirement with active old runs, including roots in review | Migration refused until execution and reservations are reconciled |
| Missing file selector, then reservation expiry | Full declared footprint stays protected |
| Truly empty legacy task/epic context | Diagnostic with repair; no guessed or inherited surface |
| `review.before_pr` on or off, after-landing auto-task on or off | With before-PR off, admit and hand off a typed not-required disposition without reviewed SHA or artifact; with it on, admit only a PR-mode executor declaring `review_gate` and require its typed review evidence. After-landing does not affect admission |
| Owner-local task without origin | Local candidate handoff and authorized local landing; no PR or remote credentials |
| SSH session and managed worker invocation | Session capability gates operator actions; managed runs never propagate operator authority; payload labels cannot replace claim/run authority |
| Revocation during a live attempt | Revoked attempt cannot bind, mutate, settle or promote; its receipt reports the revoked phase |
| Probe and receipt lookup after binary upgrade | No admission side effects; original outcome found without rewriting input; incompatible protocol refuses; `not_found` licenses no replacement |
| Approve twice, or revoke before merge | Operator required; one immutable authorization/start request under concurrent retries; revoked or stale candidate cannot publish merge intent |
| Missing, failed, replaced or wrong-candidate validation artifact | Acceptance, approval and merge-intent publication rejected with no partial effects |
| Idle polling and receipt compaction | One idle request per pass; tombstones cannot re-admit; unsettled receipts kept; growth metrics visible |
| Host shutdown or reboot scheduled | Sweep fires and mints nothing, drain waves and the ship sweep admit nothing, readiness and doctor name mode and time; in-flight runs untouched; no schedule, or a non-systemd host, admits unchanged |
| Mixed legacy and claimed admission under one ceiling | One occupancy reading; wrapper → gate → queued claimed leaf is one slot; terminal leaf holds its slot until settlement; `max_active_runs` per definition |

## 9. Concerns & Honest Limitations

- **Receipt metadata grows** as permanent tombstones (§2).
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
- **Undelivered settlements wait for the clock.** A leaf whose own delivery and short retries
  fail while no drain is running keeps its settlement recorded until the next clock-sweep tick
  delivers it ([ORB-13892]). On a follower without the OS clock installed, it waits for the next
  drain, cancel or `orbit run auto --stop` ([ORB-13663]). A graceful cancel waits on such a
  settlement for as long as the owner is unreachable; `--force` ends the drain without it.
- **A forced stop is only as good as the signal.** Force refuses a leaf it cannot signal and see
  gone, and holds the release of one whose stop was not confirmed. Such a leaf needs an operator
  on its host. Releasing it blind could let a second executor run the task beside the first.
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
- [ORB-13642] — added claimed-mode implementation and evidence-bearing failure settlement.
- [ORB-13663] — moved settlement ownership from the admitting drain to the admission record.
- [ORB-13755] — scoped the claimed delivery gate to the attempt being delivered.
- [ORB-13756] — let claimed runs deliver new files inside their frozen footprint.
- [ORB-13992] — narrowed review admission to the captured `review.before_pr`; after-landing review never refuses a pull.
- [ORB-13894] — attributed handoff landings to their owner tasks in `deliveries_landed` batches.
- [ORB-14247] — stopped `no-diff-expected` tasks holding context locks.
- [ORB-14257] — typed claimed-leaf failure classes; only candidate and task-input failures block, others release within a per-task budget; host failures suppress the host for the window; a claimed PR leaf continues a kept candidate it can fetch.
- [ORB-14338] — carried an unpublished claimed candidate to a durable ref on `origin` so any host resumes it, gave claimed-local leaves candidate continuation, and recorded a typed reason in task history when a kept candidate is set aside.
- [ORB-14261] — added one automatic repair of a handoff whose landing stopped on its base.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
