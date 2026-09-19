---
title: Distributed Drain — Decisions
owner: claude
last_updated: 2026-09-18
last_validated: 2026-09-18
status: Draft
feature: distributed-drain
doc_role: decisions
type: design
summary: Pull-based admission, durable request and attempt identity, owner ordering, explicit landing authority, the epic and triage retirements, retained ship sweep, none-only review, and non-pruning footprints.
tags: [distributed-drain, multi-host, decisions]
paths: ["crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml", "crates/orbit-core/src/runtime/task/locks.rs"]
related_features: [distributed-drain, federated-mcp, host-registry, resident-orchestrator]
related_artifacts: [ORB-12488]
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
the authenticated execution machine on each claim. Capacity stays local; it is not declared per
call. There is no liveness protocol. A follower that disappears stops taking new work; existing
claims remain until settlement or deliberate recovery.

### Consequences

- The owner has no worker table, no placement logic, and no host-specific configuration beyond the
  callers-file row that authorizes the caller.
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
over federated MCP, by the owner from the authenticated caller identity. Fields are additive and
nullable; absent means unknown.

### Consequences

- Cross-host references (`job_run_id` + `job_run_host`) are resolvable without a fleet table.
- Provenance cannot be spoofed by a follower payload.
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
- `docs/design/resident-orchestrator/` is archived; its drain-window and slot-refill work lives on
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

## Requests identify admissions and claims identify attempts

**Recorded:** 2026-09 · design-review revision of the contract authored by [ORB-12488].
**Code anchors:** `crates/orbit-core/src/runtime/task/locks.rs`, `crates/orbit-engine/src/executor/automation/vcs/handoff.rs::load_handoff_context`

### Context

Status and a reservation prevent some duplicate admission, but a lost response can strand a claim
and a returning worker can encounter the same task status under a replacement attempt. Existing
run ownership checks are valuable but must be carried across hosts and checked inside mutations.

### Decision

Give every intended pull a durable request ID and every admitted attempt a distinct claim ID.
Record the request receipt with admission. Bind the claim to an authenticated execution machine
and one leaf run. Check it atomically on worker mutations and revoke it before reassignment.
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
**Code anchors:** `crates/orbit-core/assets/routines/ship_sweep.yaml`, `crates/orbit-core/assets/jobs/workspace_ship_pipeline.yaml`, `crates/orbit-core/src/application/routine.rs`

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
**Code anchors:** `crates/orbit-core/src/application/review/gate.rs`, `crates/orbit-core/assets/jobs/task_pr_pipeline.yaml`

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
- Cost: workspaces configured for before-PR or after-landing review must explicitly change policy
  or wait for a later version; pull never silently downgrades their policy.

## Declared context survives missing filesystem targets

**Recorded:** 2026-09 · Daniel requested removal of context-file pruning after review of [ORB-12488].
**Code anchors:** `crates/orbit-core/src/runtime/task/locks.rs::existing_envelope_context_files_at_root`, `crates/orbit-core/src/application/task/paths.rs`

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
  already pruned from stored tasks need history-backed restoration or operator repair.

## Task References

- [ORB-12488] — authored this design folder for the pull-based multi-host drain.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
