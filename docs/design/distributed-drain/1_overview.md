---
title: Distributed Drain — Overview
owner: claude
last_updated: 2026-09-18
last_validated: 2026-09-18
status: Draft
feature: distributed-drain
doc_role: overview
type: design
summary: Run the workspace drain on more than one host against one owner store — followers pull one task at a time from the owner's ready queue over federated MCP, validate where they built, and land through the owner.
tags: [distributed-drain, multi-host, pull, federated-mcp, resident-orchestrator]
paths: ["crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml", "crates/orbit-core/assets/activities/classify_workspace_auto_tasks.yaml", "crates/orbit-core/src/runtime/task/locks.rs", "crates/orbit-core/src/application/automation/ownership.rs", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-mcp/**"]
related_features: [distributed-drain, federated-mcp, host-registry, resident-orchestrator, activity-job, state-compatibility, task-migration, automation-triggers]
related_artifacts: [ORB-12488]
---

# Distributed Drain — Overview

> **Status: Draft, proposed.** Nothing in this folder is implemented. [ORB-12488] authored the
> contract; implementation tasks are to be filed from [§3](#3-at-a-glance).

The distributed drain lets multiple compatible hosts execute work for one owner workspace.
The owner holds authoritative task state and orders admission; followers pull when they have
capacity, implement and validate locally, then submit evidence for owner-controlled landing.
`orbit.task.pull` atomically records a task transition, reservation, execution claim, and durable
request receipt. Retries recover the same admission. Attempt ownership prevents an old worker
from changing authoritative state after recovery. V1 has no heartbeat, fleet registry, or automatic
reassignment; recovery is deliberate and merge completion requires explicit authority.

## 1. Motivation

Both of the operator's hosts saturate at roughly ten concurrent `task_auto_pipeline` runs on the
Orbit repository, and the ceiling is Cargo, not the agent. The build budget
([runbooks/build-budget.md](../../runbooks/build-budget.md)) bounds heavy phases per host; it
cannot create capacity. Buying a larger machine was rejected as premature. Hosted cloud sessions
were evaluated and rejected for v1: no completion signal, no reach to the owner store, no Orbit
envelope or audit trail, and concurrency that is a plan rate limit rather than a capacity that
scales with hosts.

The second host can add build capacity, provided it can run the drain
without becoming a second control plane. Today it cannot. `classify_workspace_auto_tasks` treats
*a live `task_auto_pipeline` run on this host* as the only claim record for a `backlog` task, and
that record is invisible to every other host: two hosts draining one store would admit the same
leaf twice. Two hosts each owning an independent store is what the operator has now — two control
planes over one repository, which the federated-mcp spec names an operator misconfiguration.

Existing roles, capability routing, reservations, and PR checks provide foundations. V1 must
add atomic admission, durable request/claim identity, routed task reads and writes, settlement,
and an owner landing consumer. These are substantive integration changes. Throughput depends on
file conflicts, provider limits, shared CI, and landing capacity as well as local build slots.

## 2. Core Concepts

**Owner.** The one checkout whose logical `owner_machine_id` is the local machine. It holds the
coordination store, task locks, the drain clock, and every `control_plane` tool. Unchanged from
host-registry.

**Follower.** A replica checkout of the same workspace on another host, running the drain loop in
**pull mode**. It holds `execute` only. It never mints tasks and never reserves locks locally. Its
coordination writes travel to the owner over federated MCP.

**Ready queue.** A logical owner-side query over dependency-ready backlog tasks in the canonical
automatic-dispatch order. A maintained projection is optional; admission always revalidates state.

**Pull.** One idempotent admission request. The owner selects one valid conflict-free task and
atomically records its reservation, claim, task transition, and response receipt. Idle is recorded
too. A retry uses the same request ID; a new poll uses a new one.

**Claim.** Durable authority for one task execution attempt, bound to an authenticated machine
and then one leaf run. Recovery revokes that authority before allowing a replacement attempt.

**Slot.** Local capacity less live leaf runs and pending admissions not yet represented by those
runs. The owner does not track host capacity.

**Epic (tag).** A size hint on a task: one large piece of work a top-tier crew takes on whole. It
no longer names a pipeline, a worktree, a reservation class, or an admission rule.

**Handoff.** The follower's durable PR and validation/review evidence, accepted by the owner
atomically with promotion to `review`. Execution writes close at this boundary.

**Landing.** An owner-side consumer verifies the pinned candidate and merges only with recorded
completion authority, then verifies actual merge evidence before marking the task done. This
consumer is new v1 work, triggered by accepted authorized handoffs. The unused scheduled
ship sweep and its wrapper are retired; explicit owner and follower drains remain.

## 3. At a Glance

| Concern | Where | Task | Status |
|---------|-------|------|--------|
| Contract: ready queue, `orbit.task.pull`, refusals, invariants | [specs/task-pull.md](./specs/task-pull.md) | [ORB-12488] | proposed |
| Transactional ready selection on the owner | [2_design.md §2](./2_design.md#2-the-ready-queue-and-orbittaskpull) | — | to file |
| Request receipt + claim + reservation + status in one transaction | [crates/orbit-core/src/runtime/task/locks.rs](../../../crates/orbit-core/src/runtime/task/locks.rs) | — | to file |
| Pull-mode drain loop (`orbit run auto --pull`) | [workspace_auto_pipeline.yaml](../../../crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml) | — | to file |
| Claimed leaf dispatch, binding, and terminal settlement | [task_pr_pipeline.yaml](../../../crates/orbit-core/assets/jobs/task_pr_pipeline.yaml) | — | to file |
| Replica task reads and coordination writes route to the owner | [crates/orbit-cmd/src/registry_runtime.rs](../../../crates/orbit-cmd/src/registry_runtime.rs), [crates/orbit-mcp](../../../crates/orbit-mcp) | — | to file |
| Manual claim inspection and recovery | [2_design.md §3.1](./2_design.md#31-attempt-ownership-and-recovery) | — | to file |
| Durable handoff and authorized landing consumer | [2_design.md §3.2](./2_design.md#32-durable-review-and-landing-handoff) | — | to file |
| Failure and concurrency acceptance coverage | [2_design.md §8](./2_design.md#8-required-validation-scenarios) | — | to file |
| Execution provenance on runs, tasks, artifacts | [2_design.md §6](./2_design.md#6-execution-provenance) | — | to file |
| Retire epic machinery; `epic` becomes a tag | [2_design.md §7.1](./2_design.md#71-epic-machinery) | — | to file |
| Retire ship sweep and its scheduled wrapper | [2_design.md §7.3](./2_design.md#73-ship-sweep) | — | to file |
| Retire failed-run triage | [2_design.md §7.2](./2_design.md#72-failed-run-triage) | — | to file |
| Follower preconditions: auth probe, version parity | [2_design.md §4](./2_design.md#4-follower-preconditions) | — | to file |
| Followers pull; the owner never places | [4_decisions.md](./4_decisions.md#followers-pull-the-owner-never-places) | [ORB-12488] | recorded |
| Requests identify admissions and claims identify attempts | [4_decisions.md](./4_decisions.md#requests-identify-admissions-and-claims-identify-attempts) | [ORB-12488] | recorded |
| Owner ordering does not require a materialized queue | [4_decisions.md](./4_decisions.md#owner-ordering-does-not-require-a-materialized-queue) | [ORB-12488] | recorded |
| Validation runs where the work ran | [4_decisions.md](./4_decisions.md#landing-consumes-durable-evidence-and-explicit-completion-authority) | [ORB-12488] | recorded |
| Owner is the always-on host | [4_decisions.md](./4_decisions.md#the-owner-is-the-always-on-host-that-followers-can-reach) | [ORB-12488] | recorded |
| Epic is a tag, not a pipeline | [4_decisions.md](./4_decisions.md#epic-is-a-tag-not-a-pipeline) | [ORB-12488] | recorded |
| Blocked tasks wait for a reader, not a classifier | [4_decisions.md](./4_decisions.md#blocked-tasks-wait-for-a-reader-not-a-classifier) | [ORB-12488] | recorded |

## Task References

- [ORB-12488] — authored this design folder for the pull-based multi-host drain.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
