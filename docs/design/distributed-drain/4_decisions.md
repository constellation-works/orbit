---
title: Distributed Drain — Decisions
owner: claude
last_updated: 2026-09-18
last_validated: 2026-09-18
status: Draft
feature: distributed-drain
doc_role: decisions
type: design
summary: Why followers pull instead of being assigned, why in-progress plus a held lock is the whole claim, why validation runs where the work ran, and why the always-on host owns the store.
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
nothing about the asker beyond the ordinary task fields. Capacity is declared by the caller per
call; liveness is never tracked. A follower that disappears simply stops asking.

### Consequences

- The owner has no worker table, no placement logic, and no host-specific configuration beyond the
  callers-file row that authorizes the caller.
- Joining or leaving the drain is a follower-side action.
- Cost: the owner cannot notice a dead follower. Stale `in-progress` tasks surface only through the
  reservation TTL and the unresolved-work scan, at that cadence.

## `in-progress` plus a held task lock is the claim

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

- No LLM call runs unattended against failure output.
- Environmental failures accumulate in `blocked` until someone looks.
- Cost: the 30-second-read diagnosis triage attached is gone; the reader gets the raw failure.

## Task References

- [ORB-12488] — authored this design folder for the pull-based multi-host drain.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
