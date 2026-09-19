---
title: Distributed Drain — Design
owner: claude
last_updated: 2026-09-18
last_validated: 2026-09-18
status: Draft
feature: distributed-drain
doc_role: design
type: design
summary: One owner store, N pulling followers — the ready queue, the pull tool, the pull-mode drain, the pulled leaf pipeline, follower preconditions, transport, execution provenance, the epic and triage retirements, and what breaks.
tags: [distributed-drain, multi-host, pull, federated-mcp]
paths: ["crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml", "crates/orbit-core/assets/jobs/task_pr_pipeline.yaml", "crates/orbit-core/assets/activities/classify_workspace_auto_tasks.yaml", "crates/orbit-core/src/runtime/task/locks.rs", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-mcp/**"]
related_features: [distributed-drain, federated-mcp, host-registry, resident-orchestrator, activity-job, policy-sandbox]
related_artifacts: [ORB-12488]
---

# Distributed Drain — Design

> **Status: Draft, proposed.** This is the target contract; no section is live. Each mechanism
> names the existing code it extends so the implementation tasks can be filed against real anchors.

This doc covers the v1 shape: one owner checkout, any number of replica checkouts on other hosts,
each replica running the drain in pull mode against the owner, and the two pieces of existing
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

The owner keeps one **ready queue** per workspace: the `backlog` tasks whose dependencies are all
`done`, in the order the owner's readiness rules already produce (priority, then age, with the tag
adjustments `orbit run readiness` applies today). It is
a projection of the store, recomputed when a task's status, priority, dependencies, tags, or parent
change, and it is the only place order is decided. Dependency order in particular lives here and
nowhere else: a follower never evaluates readiness, so it cannot disagree with the owner about it.

`orbit.task.pull` is one new `control_plane`-class tool on the owner that pops that queue. Its
contract is [specs/task-pull.md](./specs/task-pull.md); the mechanism is:

1. Walk the queue from the head. Skip an entry whose lock footprint (its own `context_files`,
   canonicalized) overlaps a lock held by an `in-progress` or `review` task or an active
   reservation; record each skip in `deferred_conflicts`.
2. The first entry that passes is the answer. In **one store transaction**: reserve its footprint
   (what `reserve_locks` does in `task_gate_pipeline`, same TTL, same `blocked_by` refusal), set
   `backlog → in-progress`, and append a history entry naming the caller machine and its drain run.
   Concurrent pops serialize on that transaction, so two hosts can never be handed the same task.
3. Return the task with the ship inputs the owner resolves for the workspace (`mode`,
   `base_branch`, `landing_branch`, `completion`) so the follower does not read workspace config it
   may not have. An empty or fully conflicting queue returns `idle` and writes nothing.

One task per call, deliberately. A follower with three free slots calls three times; each call
sees the reservations the previous ones committed, so the three tasks are conflict-free against
each other without any batch logic. There is no count, no capacity declaration, no crew filter,
and no scan bound in the input: anything that would let a caller shape which task it gets is a
second scheduler, and the point of the queue is that there is one.

The owner's own drain switches to the same path. `classify_workspace_auto_tasks` keeps its
prediction role for readiness reporting, but admission on every host — owner included — goes
through pull, so there is one admission code path and the "live wrapper run is the claim record"
special case retires. A task that is `in-progress` with a held lock is carried; nothing else is.

There is no epic path. A task tagged `epic` is an ordinary queue entry whose footprint is its own
`context_files`; the tag is a size hint for crew selection, nothing more ([§7](#7-retirements)).

Crew is not a queue input. A pulled task carries its own `crew` when one was set; otherwise the
follower resolves crew as it does today. Auto-assigning crews by complexity, and any crew-aware
pulling that would follow from it, is deferred to [3_vision.md](./3_vision.md#1-open-questions).

## 3. Pull-mode drain and the pulled leaf pipeline

`workspace_auto_pipeline` gains a `pull` mode (`orbit run auto --pull <selector>`), where
`<selector>` is the host-qualified `hm_<owner>/ws_*` token copied from federated
`orbit_workspace_list`. The loop body changes in one place: the `admissible` step calls
`orbit.task.pull` on the owner once per free slot (`max_active_leaf_runs − live leaf runs on this
host`), stopping early on the first `idle`, instead of classifying locally. Everything downstream — detached `invoke_detached`, the drain
window, `poll_sleep_seconds` / `idle_sleep_seconds`, `orbit run concurrency --set`, `--stop` — is
unchanged. The `start_epic` step is removed outright ([§7](#7-retirements)), not skipped.

Each pulled task is dispatched to `task_auto_pipeline` as today, with one difference: the child
gate is skipped. `task_gate_pipeline` exists to wait for a lock window and reserve it; pull already
did both. The leaf goes straight to `task_pr_pipeline` with a `pulled: true` input that (a) skips
`reserve_locks`, (b) keeps `release_reservation` at the end so the lock clears at terminal, and (c)
names the branch `orbit/<task-id>-<host_id>` so two hosts can never push the same ref.

The coordination writes inside that pipeline — `pr_promote`, artifacts, execution summary, task
comments, friction — are `control_plane` writes. On a follower they are refused locally by the
existing guard, so the follower's Core routes them to the owner over the federated selector the
drain was started with. Run state, audit, logs, and the worktree stay on the follower: that is the
split-authority rule of federated-mcp §4, applied to a job instead of a caller.

Delivery on a follower ends at `pr_open` + promote to `review`. `pr_complete` (the merge) is not
run by the follower; the owner's existing ship sweep completes PRs as it does now. `ship_mode:
local` workspaces are not drained by followers in v1 (there is no remote to land through).

## 4. Follower preconditions

A follower refuses to pull, logs why, and sleeps `idle_sleep_seconds` when any of these fail. Each
is checked once per drain iteration, before the pull call, so a broken host never takes a task it
cannot finish.

| Check | Why | Source of truth |
|---|---|---|
| Provider auth for every crew the workspace can resolve | Orbit workers 401 mid-task and park in `blocked` when the CLI OAuth is revoked | provider probe (`claude auth status` or family equivalent) |
| Orbit binary version and orchestration schema equal the owner's | a stale binary against a bumped schema has already broken sandbox runs; a newer follower must not write a schema the owner cannot read | `ORCHESTRATION_SCHEMA_VERSION`; state-compatibility rules |
| Owner reachable and `capability_refused` not returned for `orbit.task.pull` | callers file grants this machine `agent` on this workspace | federated probe budget |
| Sandbox prerequisite present (`sandbox-exec` / Bubblewrap) | policy-sandbox fails closed without it | existing `orbit doctor` check |
| Git remote reachable with push credentials | landing needs `git_push` and `pr_open` | `gh auth status` |

The owner side enforces the version check too: pull carries the caller's binary version and schema,
and the owner refuses with `version_mismatch` when they differ from its own. The follower check is
the fast path; the owner check is the correctness boundary.

## 5. Transport

Followers speak to the owner over federated MCP carried by SSH stdio, initiated by the follower.
The owner declares nothing about followers except a callers-file row granting the follower's
`machine_id` the `agent` capability on the workspaces it may drain. The follower's
`~/.orbit/mcp-destinations.toml` names the owner. SSH `ControlMaster` keeps one authenticated TCP
connection per follower for the life of the drain; a dropped connection fails the in-flight
coordination write, which the job's existing step-failure recovery retries.

Direction matters: the follower initiates, so only the owner needs to accept SSH. On the operator's
setup the always-on Linux box accepts SSH over the tailnet and the LAN; the Mac does not need to.

## 6. Execution provenance

Not required for the first pull to work, but every record it produces is wrong-by-omission without
it, so the fields are prepared with v1. Today a job run, a task's `job_run_id`, and a task artifact
say nothing about where execution happened; with one host that was implicit, with two it is a
dangling reference.

| Record | Field | Set by | Notes |
|---|---|---|---|
| Job run | `executed_on { machine_id, host_id }` | the runtime that inserts the run | immutable; steps inherit; nullable so pre-existing rows read as *unknown*, never as "the owner" |
| Task | `job_run_host` beside `job_run_id` | the pipeline that links the run, via the owner | a pulled task's run lives in the follower's store; without the host the owner's `orbit run show` cannot resolve it |
| Task history | `pulled_by { machine_id, run_context }` | `orbit.task.pull` | already in the spec |
| Task artifact | `origin { machine_id, host_id }` | the owner, at put time | over federated MCP the identity comes from the authenticated caller row, not from the payload |
| Agent envelope | `ORBIT_MACHINE_ID`, `ORBIT_HOST_ID` | the dispatching runner | advisory, for execution summaries and PR bodies; the store fields above are the truth |

The key is the stable `machine_id`; `host_id` rides along for display and may be renamed. Nothing is
inferred from hostname, cwd, SSH target, or audit label, per the host-registry rule. Federated run
inspection ([3_vision.md](./3_vision.md#1-open-questions)) is what eventually reads these fields
across hosts; until then they make single-host records honest and cross-host references resolvable.

## 7. Retirements

Two existing mechanisms do not survive contact with a second host, and neither earned its keep on
one. Both are removed as part of this feature, not deferred.

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
| Epic-worktree retention rule in `worktree_gc` | `worktree_gc_pipeline.yaml` |
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

Migration: no epic root may be `in-progress` when the change lands. Drain or park active epics
first; the removal refuses to start otherwise. Existing `epic`-tagged tasks keep their tag and
their children and simply become queue entries.

### 7.2 Failed-run triage

`task_triage_pipeline` and its seeded `task_triage` routine list blocked tasks whose `job_run_id`
points at a failed run in the local store, have an agent classify the failure, and re-backlog the
"environmental" ones. Under followers the run a task is blocked on may live on another host, so
the owner's triage either skips it or diagnoses the wrong thing, and an automatic re-backlog would
hide exactly the host-specific failures the operator needs to see.

What is removed: `task_triage_pipeline.yaml`, `routines/task_triage.yaml` (shipped `enabled:
false`), the `list_triage_candidates` / `triage_failed_runs` / `apply_dispositions` activities,
the triage recursion guard in `application/automation/incidents.rs`, the seed entry in
`application/routine.rs`, and the references in `CONFIG.md`, operation-mode, automation-triggers,
and the orbit-orchestrate recovery reference.

What replaces it: nothing automatic. A failed run parks its task in `blocked` with the failure and
`job_run_host` attached; a human or the orchestrate skill reads it. Re-backlogging is a deliberate
transition, made by whoever looked.

## 8. Concerns & Honest Limitations

- **A dead follower leaves a stale `in-progress` task.** There is no lease, so nothing on the owner
  notices that the host carrying a task went away. This is the same failure a crashed local run
  causes today and is handled by the same paths: the reservation TTL expires, `scan_unresolved_work`
  and the operator see an `in-progress` task with no live run, and recovery re-queues it. What is
  new is that the owner cannot inspect the dead run's logs; they are on the follower.
- **Duplicate work after recovery.** If a follower is merely slow (laptop asleep, not dead) and the
  operator re-queues its task, the follower may later push a branch for a task that is no longer
  `in-progress`. The promote write fails on the status guard, the branch is orphaned, and worktree
  GC on the follower reclaims it. Wasted compute, no corruption.
- **Coordination writes are now network calls.** Every `pr_promote`, artifact put, and comment from
  a follower crosses SSH. Latency is fine; a partitioned follower mid-pipeline stalls until recovery
  retries or gives up, and the task sits `in-progress` meanwhile.
- **Logs and audit are per host.** The audit trail for one task can span two hosts (pull and promote
  on the owner, run and steps on the follower). `orbit run` inspection on the owner does not show
  follower runs; `job_run_host` says where to look, but nothing follows the pointer yet. A federated
  run-inspection surface is future work.
- **Large tasks are large.** With the epic path gone, a huge `epic`-tagged task is one leaf that
  holds one slot and one reservation for as long as it takes. That is honest, but a follower that
  pulls it may hold its files for hours; nothing in the queue prefers to hand big tasks to the
  fastest host.
- **Friction and auto-tasks from follower runs.** Anything a follower run would mint locally is
  refused by the replica guard. V1 routes friction through the owner like other writes; auto-task
  minting on followers is out of scope.
- **Two control planes until the operator collapses them.** Nothing detects the current dual-owner
  state. Running pull mode against one owner while the other host still owns an independent store
  simply drains two disjoint backlogs; it does not corrupt, and it does not help.
- **Compiler cache is per host.** The opt-in sccache from
  [runbooks/compiler-cache.md](../../runbooks/compiler-cache.md) does not cross hosts. Each
  follower warms its own.

## Task References

- [ORB-12488] — authored this design folder for the pull-based multi-host drain.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
