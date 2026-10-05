---
title: Distributed Drain — Decisions
owner: claude
last_updated: 2026-10-04
last_validated: 2026-09-19
status: Draft
feature: distributed-drain
doc_role: decisions
type: design
summary: Pull-based admission, durable request and attempt identity, machine-scoped run lookups, record-owned settlement, owner ordering, explicit landing authority, the epic and triage retirements, retained ship sweep, none-only review, and non-pruning footprints.
tags: [distributed-drain, multi-host, decisions]
paths: ["crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml", "crates/orbit-core/src/runtime/task/locks.rs"]
related_features: [distributed-drain, federated-mcp, host-registry]
related_artifacts: [ORB-12488, ORB-13992]
---

# Distributed Drain — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

## Followers pull; the owner never places

**Recorded:** 2026-09 · [ORB-12488]

### Context

Two hosts, different capacity, one of them a laptop that sleeps. The first sketch was a master that
round-robins tasks to followers. That requires the master to know each follower's slot count, to
notice when a follower dies, and to reclaim what it pushed there — a liveness protocol and a worker
table, which is the fleet control plane host-registry's vision explicitly forbids reviving.

### Decision

A host with a free slot asks the owner for work. The owner answers from its backlog and records
the execution machine its trusted invocation context names on each claim. Capacity stays local; it is not declared per
call. There is no liveness protocol. A follower that disappears stops taking new work; existing
claims remain until settlement or deliberate recovery.

### Consequences

- The owner has no worker table, no placement logic, and no host-specific configuration at all:
  after [ORB-12564] an SSH login to the owner is what admits a follower, and the destination
  keeps no per-caller row (see [SSH login is the admission; machine labels are
  attribution](#ssh-login-is-the-admission-machine-labels-are-attribution)).
- Joining or leaving the drain is a follower-side action.
- Cost: the owner cannot distinguish a dead follower from an unreachable one. Operators must
  inspect claim/run evidence and explicitly reclaim work; TTL expiry is not proof of death.

## `in-progress` plus a held task lock is the claim

**Superseded by:** [Requests identify admissions and claims identify attempts](#requests-identify-admissions-and-claims-identify-attempts). The original rationale below is retained as history.

**Recorded:** 2026-09 · [ORB-12488]
**Code anchors:** `crates/orbit-core/assets/activities/classify_workspace_auto_tasks.yaml`, `crates/orbit-core/src/runtime/task/locks.rs::lock_context_files_for_task`

### Context

A local drain's claim record is the live `task_auto_pipeline` run carrying a task; the task itself
stays `backlog` until the child moves it. That record is process-local and invisible to another
host. A lease-and-heartbeat scheme was drafted and rejected as machinery: it adds a row type, a
renewal timer, an expiry sweep, and a `stale_lease` error class to protect against a failure the
store already represents.

### Decision

`orbit.task.pull` moves each admitted task to `in-progress` and reserves its locks in one
transaction. That pair is the claim, on every host including the owner. No lease, no renewal, no
new table. Admission on any host treats an `in-progress` task with a held lock as carried and
nothing else as carried.

### Consequences

- One admission code path for local and pulled work; the "live wrapper run is the claim" special
  case in classify retires.
- Every existing status guard, TTL, and recovery path applies to pulled tasks unchanged.
- Cost: a pulled task is `in-progress` before any run exists for it, so the window between pull and
  the follower's first step has no run id to inspect. If the follower crashes in that window the
  task looks like an abandoned run with no run.

## Order lives in one owner queue, and a pull takes one task

**Superseded by:** [Owner ordering does not require a materialized queue](#owner-ordering-does-not-require-a-materialized-queue). The original rationale below is retained as history.

**Recorded:** 2026-09 · [ORB-12488]

### Context

The first draft of the pull tool took a `slots` count, an `allowed_crews` filter, and a scan bound,
and walked the backlog per call. That puts dependency readiness, priority order, and conflict
admission inside every call and lets each caller shape the answer — three hosts, three partial
schedulers. Dependencies were the tell: a per-call walk has to re-derive which tasks are ready
every time, on every host.

### Decision

The owner maintains one ordered ready queue per workspace (dependencies satisfied, priority/age/tag
order). `orbit.task.pull` pops the first conflict-free entry, one task per call,
with no count, no capacity declaration, and no crew filter. A follower that wants more calls again.

### Consequences

- Order is decided once, on the owner, by the readiness rules that already exist. Followers cannot
  disagree with it or with each other.
- The tool input shrinks to the selector, version parity, and run context.
- Crew-aware pulling is deferred until crews are auto-assigned by complexity; at that point crew is
  a property of the queued task, not a caller filter.
- Cost: a follower with N free slots makes N round trips per drain iteration, and a queue whose
  head conflicts is re-walked by every caller until the head clears.

## Validation runs where the work ran; the owner only lands

**Superseded by:** [Landing consumes durable evidence and explicit completion authority](#landing-consumes-durable-evidence-and-explicit-completion-authority). The original rationale below is retained as history.

**Recorded:** 2026-09 · [ORB-12488]

### Context

The constraint is Cargo. If followers implemented and the owner validated, every build would still
happen on the owner and the ceiling would not move.

### Decision

The follower runs the full leaf pipeline through push and `pr_open` in its own worktree, including
build, test, and the review gate. The owner performs only store writes and, through its existing
sweep, the merge.

### Consequences

- Follower capacity is real capacity: N followers give N times the build slots.
- The owner needs no warm target directory for follower tasks.
- Cost: the merge is authored on the owner from a branch it never built. CI on the PR is the check
  that the follower's validation was honest.

## The owner is the always-on host that followers can reach

**Recorded:** 2026-09 · [ORB-12488]

### Context

Pull means the follower initiates the connection, so only the owner must accept SSH. On the
operator's setup the Linux box accepts SSH over the tailnet and LAN; the Mac requires a
local-network grant per hosting app, sleeps, and has had its launchd drain clock die undetected
twice. Its CLI OAuth is also revoked by the desktop app's sessions.

### Decision

The owner checkout for a shared workspace lives on the host that is always on and already accepts
inbound SSH. Laptops are followers. This is an operator rule, not a code check.

### Consequences

- The single drain clock and the store live where they are least likely to stop.
- The Mac's current independent `ws_orbit` must be re-registered as a replica before pull mode is
  useful.
- Cost: the operator's interactive front-door sessions on the Mac now write task state over SSH
  rather than to a local store, and are blocked when the box is unreachable.

## Execution provenance is the destination's `machine_id`, stamped at write time

**Recorded:** 2026-09 · [ORB-12488]

### Context

Runs, task run links, and artifacts have never recorded where they were produced because there was
one host. A follower's run id is meaningless on the owner without the host it belongs to, and a
payload that names its own host can name any host.

### Decision

Every run, run link, and artifact carries the stable `machine_id` (with `host_id` for display) of
the host that produced it, set by the host that writes the record — or, for writes that arrive
over federated MCP, by the owner from the trusted runtime invocation context that fences the
claim. A forwarded machine label is attribution and never becomes provenance on its own, so a
remote write whose execution context is unknown reads as unknown rather than as the caller it
claims to be. Fields are additive and nullable; absent means unknown.

### Consequences

- Cross-host references (`job_run_id` + `job_run_host`) are resolvable without a fleet table.
- Provenance cannot be spoofed by a follower payload: a label in the payload is not the source.
- Cost: one schema bump across run, task, and artifact records, and every existing row reads as
  unknown until a run touches it.

## Epic is a tag, not a pipeline

**Recorded:** 2026-09 · [ORB-12488]
**Code anchors:** `crates/orbit-core/src/runtime/task/locks.rs::lock_context_files_for_task`, `crates/orbit-core/assets/jobs/epic_pipeline.yaml`, `crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml`

### Context

The epic path gave one large body of work a single worktree, a sequential child drain, a
descendant-union reservation, and its own finisher agent. It pinned a drain slot for the epic's
life, shadowed unrelated leaves through the union reservation, needed its own admission exclusions
in two activities, and cannot run anywhere but the owner. A second host made the cost visible; the
benefit — one review artifact — was never worth it against a task a strong crew can take whole.

### Decision

Remove the epic pipeline, orchestrator, reservation union, admission exclusions, and GC rule. Keep
parent/child relations as plain hierarchy. Keep `epic` as a tag meaning *one large task for a
top-tier crew*, read by crew selection and ignored by admission.

### Consequences

- One admission path, one footprint rule (a task's own `context_files`), one leaf pipeline.
- Large work is one leaf; its size shows up as slot time, not as special machinery.
- `docs/design/resident-orchestrator/` is removed; its drain-window and slot-refill work lives on
  in `workspace_auto_pipeline`.
- Cost: a big task no longer gets a stable, reattachable worktree across runs; a crash mid-epic
  restarts from the branch, like any other leaf.

## Blocked tasks wait for a reader, not a classifier

**Recorded:** 2026-09 · [ORB-12488]
**Code anchors:** `crates/orbit-core/assets/jobs/task_triage_pipeline.yaml`, `crates/orbit-core/src/application/automation/incidents.rs`

### Context

Failed-run triage had an agent classify each failed run and re-backlog the environmental ones. It
reads the run from the local store, so a task blocked by a follower's failure is invisible or
misread on the owner. Its value was saving a human a look; its risk was hiding host-specific
failures behind an automatic retry.

### Decision

Remove the triage pipeline, routine, activities, and recursion guard. A failed run parks its task
in `blocked` with the failure and `job_run_host`; re-backlogging is a human or orchestrate-skill
transition.

### Consequences

- Terminal failed-run classification and automatic re-backlogging are removed. In-run
  `step_failure_recovery` remains; it may invoke an LLM against failure output within its existing
  authorization and recovery budget. Retirement does not disable that separate mechanism.
- Environmental failures accumulate in `blocked` until someone looks.
- Cost: the 30-second-read diagnosis triage attached is gone; the reader gets the raw failure.

**Amended by [ORB-13897].** Final recovery now reads a terminally failed task before a human
does. It is not a classifier that re-backlogs: an agent proposes one typed decision, and a
deterministic applier re-checks it against the base branch, a 2-per-24 h requeue bound, and any
human change since the failure. A requeue needs evidence the environment changed, and anything
uncertain still parks the task in `blocked`, now with a diagnosis and a named human action. See
[Final recovery decides; a deterministic applier acts](../activity-job/4_decisions.md#final-recovery-decides-a-deterministic-applier-acts).

## Requests identify admissions and claims identify attempts

**Recorded:** 2026-09 · design-review revision of the contract authored by [ORB-12488].
**Code anchors:** `crates/orbit-core/src/runtime/task/locks.rs`, `crates/orbit-engine/src/executor/automation/vcs/handoff.rs::load_handoff_context`

### Context

Status and a reservation prevent some duplicate admission, but a lost response can strand a claim
and a returning worker can encounter the same task status under a replacement attempt. Existing
run ownership checks are valuable but must be carried across hosts and checked inside mutations.

### Decision

Give every intended pull a durable request ID and every admitted attempt a distinct claim ID.
Record the request receipt with admission. Bind the claim to the execution machine its trusted
runtime invocation context names, and to one leaf run. Check it atomically on worker mutations and revoke it before reassignment.
No heartbeat or automatic reclamation is introduced. Age and TTL support inspection only.

### Consequences

- Retries can recover the original admission without consuming another task.
- Stale workers cannot overwrite a replacement's authoritative evidence or release its reservation.
- Cost: receipts, claim phases, owner write fencing, and explicit recovery become required v1
  storage/API work. Reclamation remains a human or supervised orchestrator responsibility.

## Owner ordering does not require a materialized queue

**Recorded:** 2026-09 · design-review revision of the contract authored by [ORB-12488].
**Code anchors:** `crates/orbit-core/src/adapter/engine_host/v2_host/backlog_exclusion.rs::sort_tasks_for_automatic_dispatch`

### Context

The earlier contract coupled centralized priority to a maintained queue projection and treated
per-request readiness evaluation as a second scheduler. An authoritative query on the owner has
one scheduler too, without a second consistency boundary for cache invalidation.

### Decision

Use a logical ordered query and revalidate within admission. A projection is optional optimization.
Pull takes one task. V1 participants must execute every eligible workspace task; future eligibility
filters may remain owner-evaluated without transferring priority authority to callers.

### Consequences

- One ordering/comparison contract serves readiness reporting and actual admission.
- Invalid entries yield diagnostics without blocking unrelated work.
- Cost: transactional selection can scan a conflicting prefix repeatedly. If measured contention
  warrants caching or bounded internal scans, preserve exact readiness and ordering semantics.

## Landing consumes durable evidence and explicit completion authority

**Recorded:** 2026-09 · design-review revision of the contract authored by [ORB-12488].
**Code anchors:** `crates/orbit-core/assets/jobs/workspace_ship_pipeline.yaml`, `crates/orbit-engine/src/executor/automation/vcs/pr/complete.rs`

### Context

The existing ship sweep starts backlog execution; it does not complete arbitrary review tasks.
The existing completion action also depends on an execution run and local worktree. Treating
follower promotion as a complete handoff would leave delivery stuck or lose its evidence gates.

### Decision

Validate where execution happens, then submit a durable candidate, validation evidence, and typed
`review_policy: none` / `not_required` disposition to a new owner-side landing consumer. Admission
and review promotion never grant merge rights. Completion requires a recorded authorization and
pinned delivery evidence. Persist external merge intent and reconcile uncertain outcomes before task
reassignment or completion. Conflicts require a newly validated repair, not an owner-side
unvalidated rebase.

### Consequences

- The owner can land a candidate without resolving follower-local paths or rebuilding its code.
- Review-only delivery remains distinct from authorized merge and verified completion.
- Cost: a handoff store/consumer and adaptation of existing completion checks are required. Failed
  landing can hold a review lock until repair or explicit recovery, limiting throughput.

## Explicit drains replace the unused ship sweep

**Superseded by:** [Ship sweep remains an admission entry point](#ship-sweep-remains-an-admission-entry-point). The original decision below is retained as history.

**Recorded:** 2026-09 · Daniel requested retirement during revision of the design authored by [ORB-12488].
**Code anchors:** `crates/orbit-core/assets/routines/ship_sweep.yaml`, `crates/orbit-core/assets/jobs/workspace_ship_pipeline.yaml`, `crates/orbit-core/src/application/routines/`

### Context

The scheduled ship sweep exists to start a workspace backlog drain. Daniel has never used it and
has no intended use for it. Keeping it disabled would still retain its wrapper, seeded workspace
configuration, documentation, and migration burden alongside explicit drain invocation.

### Decision

Remove the ship-sweep routine, its seed, and the `workspace_ship_pipeline` wrapper. Keep explicit
owner/follower drain commands. Authorized landing is driven by durable requests for accepted
handoffs, with restart recovery; it must not depend on or recreate a scheduled backlog sweep.
The generic scheduler and unrelated routines remain.

### Consequences

- There is one explicit way to start the workspace drain, with local or pull execution mode.
- Landing can complete authorized work even when no drain is running.
- Cost: operators who configured ship-sweep instances must remove or retarget them and reconcile
  active wrappers during migration. The built-in periodic backlog-start feature is no longer
  available; this change installs no replacement schedule.

## Ship sweep remains an admission entry point

**Recorded:** 2026-09 · Daniel reversed ship-sweep retirement after review of [ORB-12488].
**Code anchors:** `crates/orbit-core/assets/routines/ship_sweep.yaml`, `crates/orbit-core/assets/jobs/workspace_ship_pipeline.yaml`, `crates/orbit-cli/src/command/run/sweep.rs`

### Context

The earlier removal proposal covered the seeded routine and wrapper but missed the independent CLI
dispatch path. Daniel now wants the feature retained.

### Decision

Keep all three entry points and their existing opt-ins. Adapt them to common claim admission; none
may rediscover and dispatch around owner serialization or imply completion authority. Preserve
schedules and enablement without enabling new ones. The handoff consumer remains independent of
whether a sweep or drain is running.

### Consequences

- Existing scheduled and explicit starts remain supported.
- Cost: every retained entry point needs claim-aware admission and capacity coverage.

## V1 review policy is none

**Recorded:** 2026-09 · Daniel narrowed v1 after review of the contract authored by [ORB-12488].
**Code anchors:** `crates/orbit-core/src/application/review/gate/`, `crates/orbit-core/assets/jobs/task_pr_pipeline.yaml`

### Context

The previous handoff required reviewed SHAs and review artifacts even though the default `none`
policy produces neither. Supporting multiple review timings would require distinct handoff and
completion contracts.

### Decision

V1 admits only `review_policy = none` on both owner and executor. Capture it on the claim and carry
an explicit `not_required` disposition alongside candidate/base SHAs and validation artifacts. Do
not fabricate reviewed SHAs or run an automatic review. Review task status remains the delivery
handoff state; completion still requires explicit authorization and verified landing evidence.

### Consequences

- Default no-review execution can produce a valid handoff without pretending a review happened.
- Cost: workspaces configured for before-PR review must explicitly turn it off or wait for a later
  version; pull never silently downgrades their policy.
- Narrowed by [ORB-13992]: `operation.review_policy` became the `review.before_pr` switch plus the
  `delivery-code-review` auto-task flag. Admission keys only on the `before_pr` each endpoint
  captured; after-landing review runs on the owner after landing and never refuses a pull. Protocol
  revision 5 carries `caller_before_pr` and `ship.before_pr`.
- Prepared by [ORB-13895]: revision 6 captures the owner's before-PR review contract on the claim
  and lets a handoff carry typed before-PR evidence the owner verifies and records. Admission still
  refuses `before_pr` until an executor declares the gate.

## Declared context survives missing filesystem targets

**Recorded:** 2026-09 · Daniel requested removal of context-file pruning after review of [ORB-12488].
**Code anchors:** `crates/orbit-core/src/runtime/task/mod.rs::declared_context_files`, `crates/orbit-core/src/runtime/task/locks.rs::TaskLockIndex::declared_lock_surface`, `crates/orbit-core/src/application/task/context_repair.rs` (landed in [ORB-12490], replacing `locks.rs::existing_envelope_context_files_at_root`)

### Context

Filesystem-existence pruning drops selectors for files a task intends to create. On a follower, that
also lets an owner-computed status lock lose declared scope after reservation expiry.

### Decision

Canonicalize and boundary-check declarations without dropping absent files or symbols. Apply the
same rule to task context storage/projections, reservations, and status locks. Freeze the admitted
canonical footprint through execution and review, independent of checkout contents. Empty declared
context remains a pre-admission diagnostic requiring operator correction, not a zero-file claim.

### Consequences

- New-file work retains its declared conflict protection before creation and after TTL expiry.
- Cost: obsolete selectors continue holding locks until deliberately corrected; declarations
  already pruned from stored tasks need history-backed restoration (`orbit task lint
  --restore-pruned`, which re-declares only what a `context_files_pruned` history entry recorded)
  or operator repair.

## SSH login is the admission; machine labels are attribution

**Recorded:** 2026-09 · Daniel's backlog-audit decision, 2026-09-19, reconciling this design with
the caller-authorization removal shipped in [ORB-12564]; applied to the code by [ORB-12495].
**Code anchors:** `crates/orbit-mcp/src/remote/identity.rs::mcp_server_identity`,
`crates/orbit-mcp/src/remote/proxy.rs::remote_serve_command`,
`crates/orbit-core/src/application/distributed/contract.rs`

### Context

This design was written while destination-side caller authorization still existed, so its §5 asked
for a `KeyBound` caller identity: a destination callers file, a forced-command acceptance
requirement, and a per-caller row the owner would check before admitting a pull. [ORB-12564]
removed that machinery — an SSH login to a destination is ownership of it, and a destination now
serves the authority its session's argv asks for — which left this folder requiring an
authorization mechanism with nothing behind it. A backlog audit parked the dependent work rather
than let an implementation quietly rebuild the retired path or reinterpret an audit label as proof
of identity.

### Decision

The removal stands, and this feature follows it. SSH login establishes owner access; there is no
destination callers file, forced-command acceptance requirement, key-bound proof, or replacement
identity registry, and none is to be reintroduced under another name. What still decides a
distributed call is the session's `agent`/`operator` capability, the caller-side rule that a client
running inside a managed run never propagates operator authority, and the trusted runtime
invocation context that fences a claim to its machine, bound run, and phase. A forwarded
`--remote-caller-machine-id` is attribution: it names a receipt namespace and appears in
diagnostics, and it grants nothing. Operator access is not narrowed by the retired cross-caller
ACL: an owner operator retains cross-attempt receipt inspection and deliberate recovery.

### Consequences

- Adding a host is an SSH-access decision on the owner, not an Orbit configuration step.
- Claim fencing, receipt namespacing, and revocation carry the whole weight of attempt ownership;
  there is no second identity check behind them to fall back on.
- Validation covers SSH session capability and claim revocation instead of caller-file contents and
  key revocation, which is what [design §8](./2_design.md#8-required-validation-scenarios) now lists.
- Cost: anyone who can log into the owner can ask it for work, and the accident guard is the
  session capability rather than a per-caller allowlist. That is the same boundary every other
  Orbit destination already has, and a second one here would have bought protection Orbit does not
  actually provide.

## An owner completion policy lands accepted handoffs without per-task approval

**Recorded:** 2026-09-27 · [ORB-13637], after the first live follower drain [ORB-13625].
**Code anchors:** `crates/orbit-store/src/repository/task/coordination/handoff.rs::accept_typed_handoff`,
`crates/orbit-core/src/application/distributed/contract.rs::owner_completion_authority`,
`crates/orbit-config/src/registry/settings.rs` (`workflow.distributed_completion`)

### Context

The owner always resolved `completion: review`. After operation grants were removed a `done`
contract had nothing to authorize it and failed closed at handoff. So every follower delivery
waited for a per-task **Approve handoff**, while the owner's own `orbit run auto --complete` drain
landed its tasks unattended. With a follower running several leaves, the operator merged follower
pull requests by hand instead. That skipped the owner's validation gate: one was merged while its
leaf was still running `make ci-fast`. The dashboard's task-level approve was refused outright for
claimed tasks, because a claim only accepts claim-scoped mutations.

### Decision

One owner key, `workflow.distributed_completion = "review" | "done"`, default `review`. With `done`:

- The probe and admission pin a `done` ship contract whose `authorization_reference` names the
  policy (`workspace-config:workflow.distributed_completion`). A follower re-probes when it changes.
- Accepting the handoff records a `HandoffAuthorizationSource::OwnerPolicy` authorization and the
  landing-start request in the same transaction that moves the task to `review`. It then
  dispatches `task_landing_pipeline`, the same consumer an operator approval starts.
- Authority comes only from the owner's own configuration, read by trusted owner code into the
  handoff observation for that decision. The ship contract alone authorizes nothing: a `done`
  contract accepted after the owner withdrew the policy is still accepted, and waits in review for
  an operator.
- Landing rechecks the owner's current configuration before merge intent and completion.
  Withdrawing the key stops every handoff that has not landed. Revocation works as it does for an
  operator approval.

This is not operation mode again. There is no grant table, scope, expiry or preset. It is one owner
setting with the same meaning as `--complete`, recorded per handoff so the audit trail names what
authorized each landing.

The dashboard's task-level approve on a review task with a handed-off claim now sends the
claim-scoped handoff approval instead of the refused status write.

### Consequences

- Follower deliveries land with the owner's checks, pinned candidate and verified merge. No human
  merges on the provider.
- The per-candidate operator approval remains the path whenever the key is `review`, and for any
  handoff accepted without the policy.
- Cost: an owner with `done` merges every validated follower delivery without a human seeing it
  first. The only gate is the owner's required validation commands, and an empty list is no
  gate. A handoff authorized by the
  policy and then fenced by withdrawing it cannot be re-approved by an operator (one
  authorization per handoff); restore the key, or revoke and recover the claim.

## A run is its id plus the machine that executes it

**Recorded:** 2026-09-28 · [ORB-13649], after the owner's drain and a follower leaf both minted
`jrun-20260928-0230-c1` and both commit steps failed.
**Code anchors:** `crates/orbit-core/src/application/task/query.rs::list_run_tasks`,
`crates/orbit-core/src/adapter/tool_host/worker_tools.rs` (`filtered` owner read),
`crates/orbit-engine/src/context/hosts.rs::RuntimeHost::list_run_tasks`

### Context

A run id (`jrun-<YYYYmmdd-HHMM>-<role><n>`) is minted from one machine's store, so it is unique
only there. The owner's store holds bindings from its own runs and from every follower's leaves, and
the run-keyed task lookups in the commit, merge, failure-blocking and resume paths matched on
`job_run_id` alone. Two machines starting a run in the same minute bound two tasks to one id, and
`commit_batch_changes` refused both runs (`expected exactly one task ..., got 2`).

### Decision

Scope every run-keyed task lookup to the pair (`job_run_id`, executing machine), option (a) in the
task. Run ids stay as they are.

- A local run is scoped to the machine its own run record names (`executed_on`). A binding with no
  recorded machine is local: only a claim binds a task for another machine, and a claim always
  records it.
- A claimed leaf's lookup goes to the owner, which keeps only bindings made by the leaf's trusted
  execution machine (`WorkerInvocation.execution`). The follower cannot name another machine.
- Automation reaches this through `RuntimeHost::list_run_tasks`: `git_commit` (all three scopes),
  `git_merge`, `merge_batch_pr`, blocking tasks when a run fails, and resume reclaiming a lineage's
  tasks.
- Checks that already hold a task id and compare its `job_run_id` with a run are not lookups. A
  colliding run could only pass them for a task it was handed, and a claimed task accepts only
  claim-scoped mutations. They are unchanged.

A machine marker in the run id (option b) would change a format that worktree paths, branch names,
role parsing and every stored run already depend on, and it would still leave the lookups matching
on a string. Refusing a colliding bind (option c) turns the collision into a failed claim instead of
removing it, and it cannot stop the owner's own drain binding an id a follower already used.

### Consequences

- The collision is harmless rather than rare: an id shared across machines resolves to each
  machine's own task.
- A runtime host that records no execution locations keeps the id-only lookup, which is correct for
  a single store.
- Cost: every run-keyed lookup reads the run record once more, and an owner running an older build
  still answers a follower's run-keyed read without the scope.

## Settlement belongs to the admission record, not to the drain that admitted it

**Recorded:** 2026-09-28 · [ORB-13663], after the Mac follower's drain `jrun-20260928-0242-t1`
was cancelled at 04:14Z and its six live leaves finished with four handoffs recorded but
undelivered, two failures recorded nowhere, and every owner claim left `running`.
**Code anchors:** `crates/orbit-core/src/adapter/engine_host/v2_host/pull/settle.rs`
(`OrbitRuntime::best_effort_settle_terminal_claimed_leaf`, `OrbitRuntime::settle_pending_pulls`),
`crates/orbit-core/src/adapter/engine_host/v2_host/pull/drain.rs::PullDrain::carry_settlement`,
`crates/orbit-core/src/runtime/task/reservation_cleanup.rs::finalize_job_run_with_cleanup_after_prior_read`,
`crates/orbit-core/src/application/job/run/actions.rs::cancel_job_run_with_reason`

### Context

Only the coordinator that admitted a claim settled it: its `pull_refill` loop saw the leaf
terminate, recorded the settlement and delivered it. `orbit run auto --stop` kept that loop alive,
but a *cancelled* coordinator — the dashboard's cancel and `orbit run cancel`, which is how every
recent Mac drain ended — took the only settlement path with it. The leaf itself could not simply
call the owner either: the agent inside it runs under the macOS sandbox, which denies `~/.ssh`
(the reason claimed leaves hand off through step output, [ORB-13642]).

### Decision

A follower's settlement does not depend on any one process staying alive. The admission record is
the outbox, and any unsandboxed follower process carries it, idempotently:

- **The leaf records its own settlement as it terminalizes.** Run finalization, in whichever
  process terminalizes the leaf, records the failure a terminal leaf implies (success already
  recorded its typed handoff in `claim_handoff`). The leaf's own worker then delivers it. The
  worker is an ordinary Orbit process with the host's federated route; only the agent subprocess
  is sandboxed, and nothing runs in the agent.
- **Any settle-only pass delivers what is recorded.** `orbit run cancel` and `orbit run auto
  --stop`, and the dashboard's cancel and stop, run one over every owner. A settle-only pass never
  requests work or launches a leaf. For an admission no live drain will carry it also ends what
  was never launched — a claim with no leaf, a queued leaf (cancelled first) — as a failure, so
  the owner never holds a claim no follower process is responsible for. An admission a live drain
  for the same owner will carry is left to it.
- **A new drain carries every earlier admission for its owner**, whichever drain made it.
- **Cancellation does not kill live leaves.** They finish and settle themselves; a cancelled
  drain's work is not wasted, and nothing is stranded.

The rule for future follower-side work: anything that must eventually reach the owner is recorded
durably first, and its delivery is an idempotent operation any follower process may repeat. Never
make delivery the private job of a long-lived coordinator, and never put it inside the sandboxed
agent.

The alternatives were narrower. Mapping the dashboard's cancel of a pull drain onto "stop
admissions" would keep today's coordinator alive but leave a killed, crashed or rebooted
coordinator stranding its leaves exactly as before. Cancelling every child and settling it as a
failure would strand nothing but would throw away finished work, and a leaf would still need
somewhere to deliver from. Delivering from a sweep alone would depend on the scheduler clock, which
is off on the Mac by design.

### Consequences

- Leaves deliver seconds after they end, whether or not a drain is watching, and `orbit run auto
  --stop` is the one command that flushes anything left, including settlements an older binary
  stranded.
- Two processes may deliver the same settlement at once — a leaf's worker and a live drain pass.
  The owner's per-claim mutation IDs make the second a replay, and a recorded settlement is
  immutable locally, so whichever process records first is the value both deliver.
- A `Created` admission (bind possibly lost) is bound before it is settled, because the owner
  fences a failure that names a leaf against the claim's binding.
- Cost: a settlement whose delivery fails while no drain is running waits for the next follower
  process to reconcile — a drain, a cancel or a stop. Nothing retries on a timer, and orphan
  reconciliation only records, since it runs while a workspace opens and must not wait on the
  owner.
- Cost: cancelling a pull drain or running `--stop` now makes owner calls, one delivery timeout at
  most per unreachable owner per pass, so the command can take seconds when the owner is down.
- Cost: a cancelled drain's unlaunched claims end as `blocked` with evidence rather than returning
  to the backlog; returning work to the backlog stays an owner-operator recovery.

## Task References

- [ORB-12488] — authored this design folder for the pull-based multi-host drain.
- [ORB-13625] — opened follower pull: owner mutation tools, routed peer, `orbit run auto --pull`.
- [ORB-13637] — added the owner completion policy ([An owner completion policy lands accepted handoffs without per-task approval](#an-owner-completion-policy-lands-accepted-handoffs-without-per-task-approval)).
- [ORB-13649] — scoped run-keyed task lookups to the executing machine ([A run is its id plus the machine that executes it](#a-run-is-its-id-plus-the-machine-that-executes-it)).
- [ORB-13663] — moved settlement from the admitting drain to the admission record ([Settlement belongs to the admission record, not to the drain that admitted it](#settlement-belongs-to-the-admission-record-not-to-the-drain-that-admitted-it)).
- [ORB-13992] — narrowed [V1 review policy is none](#v1-review-policy-is-none) to the `review.before_pr` switch.
- [ORB-13895] — prepared [V1 review policy is none](#v1-review-policy-is-none) for before-PR review on claims and handoffs.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
