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
related_features: [distributed-drain, federated-mcp, host-registry, resident-orchestrator, activity-job, state-compatibility, task-migration]
related_artifacts: [ORB-12488]
---

# Distributed Drain — Overview

> **Status: Draft, proposed.** Nothing in this folder is implemented. [ORB-12488] authored the
> contract; implementation tasks are to be filed from [§3](#3-at-a-glance).

Orbit's throughput on a repository is capped by the build capacity of the one host that runs its
drain. The distributed drain lets **every host that holds a checkout of the workspace** run the
drain loop, while **exactly one host** — the owner checkout — keeps the task store, the lock
table, and the sweep clock. Followers do not receive work; they **pull** it. A follower with a
free build slot calls one new owner-side tool, `orbit.task.pull`, which pops the next
**conflict-free** task from the owner's **ready queue** and, in the same transaction, reserves its
locks and moves it to `in-progress`. From that point the task looks exactly like any task an agent
is carrying today:
the follower implements and validates it in its own worktree, pushes the branch, opens the PR,
and promotes it through the owner. No lease, no heartbeat, no fleet registry. The owner never
learns about hosts; it only learns that a task became `in-progress`.

## 1. Motivation

Both of the operator's hosts saturate at roughly ten concurrent `task_auto_pipeline` runs on the
Orbit repository, and the ceiling is Cargo, not the agent. The build budget
([runbooks/build-budget.md](../../runbooks/build-budget.md)) bounds heavy phases per host; it
cannot create capacity. Buying a larger machine was rejected as premature. Hosted cloud sessions
were evaluated and rejected for v1: no completion signal, no reach to the owner store, no Orbit
envelope or audit trail, and concurrency that is a plan rate limit rather than a capacity that
scales with hosts.

The two existing hosts together double the ceiling, provided the second host can run the drain
without becoming a second control plane. Today it cannot. `classify_workspace_auto_tasks` treats
*a live `task_auto_pipeline` run on this host* as the only claim record for a `backlog` task, and
that record is invisible to every other host: two hosts draining one store would admit the same
leaf twice. Two hosts each owning an independent store is what the operator has now — two control
planes over one repository, which the federated-mcp spec names an operator misconfiguration.

The missing piece is one queue and one tool. Everything else — owner and replica checkout roles, the
`control_plane` / `execute` capability split, SSH-carried federated MCP, the callers file, the
task-lock table, the PR ship pipeline, worktree GC — already exists or is already specified.

## 2. Core Concepts

**Owner.** The one checkout whose logical `owner_machine_id` is the local machine. It holds the
coordination store, task locks, the drain clock, and every `control_plane` tool. Unchanged from
host-registry.

**Follower.** A replica checkout of the same workspace on another host, running the drain loop in
**pull mode**. It holds `execute` only. It never mints tasks, never reserves locks locally, and
never runs an epic. Its coordination writes travel to the owner over federated MCP.

**Ready queue.** The owner-maintained ordered list of `backlog` tasks whose dependencies are
satisfied, excluding epic roots and their descendants, in the priority/age/tag order the owner's
readiness rules already produce. A projection of the store, recomputed on task changes; the only
place order is decided.

**Pull.** `orbit.task.pull`: pop the first ready-queue entry whose lock footprint overlaps no held
lock, reserve that footprint, and set it `in-progress`, in one transaction. One task per call. A
pulled task is indistinguishable in the store from a task the owner's own drain started.

**Slot.** One unit of a host's build capacity: `max_active_leaf_runs` minus the live leaf runs on
that host. A host pulls once per free slot. Capacity stays on the host; the owner never learns it.

**Landing.** What a follower does with a finished branch: push, open the PR, promote the task to
`review` through the owner. Merging (`pr_complete`) stays with the owner's existing ship sweep.

## 3. At a Glance

| Concern | Where | Task | Status |
|---------|-------|------|--------|
| Contract: ready queue, `orbit.task.pull`, refusals, invariants | [specs/task-pull.md](./specs/task-pull.md) | [ORB-12488] | proposed |
| Ready-queue projection on the owner | [2_design.md §2](./2_design.md#2-the-ready-queue-and-orbittaskpull) | — | to file |
| Pop + reservation + `in-progress` in one transaction | [crates/orbit-core/src/runtime/task/locks.rs](../../../crates/orbit-core/src/runtime/task/locks.rs) | — | to file |
| Pull-mode drain loop (`orbit run auto --pull`) | [workspace_auto_pipeline.yaml](../../../crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml) | — | to file |
| Pulled leaf pipeline skips the gate | [task_pr_pipeline.yaml](../../../crates/orbit-core/assets/jobs/task_pr_pipeline.yaml) | — | to file |
| Replica coordination writes route to the owner | [crates/orbit-cmd/src/registry_runtime.rs](../../../crates/orbit-cmd/src/registry_runtime.rs), [crates/orbit-mcp](../../../crates/orbit-mcp) | — | to file |
| Execution provenance on runs, tasks, artifacts | [2_design.md §6](./2_design.md#6-execution-provenance) | — | to file |
| Follower preconditions: auth probe, version parity | [2_design.md §4](./2_design.md#4-follower-preconditions) | — | to file |
| Followers pull; the owner never places | [4_decisions.md](./4_decisions.md#followers-pull-the-owner-never-places) | [ORB-12488] | recorded |
| `in-progress` plus a held lock is the claim | [4_decisions.md](./4_decisions.md#in-progress-plus-a-held-task-lock-is-the-claim) | [ORB-12488] | recorded |
| Order lives in one owner queue; pull takes one | [4_decisions.md](./4_decisions.md#order-lives-in-one-owner-queue-and-a-pull-takes-one-task) | [ORB-12488] | recorded |
| Validation runs where the work ran | [4_decisions.md](./4_decisions.md#validation-runs-where-the-work-ran-the-owner-only-lands) | [ORB-12488] | recorded |
| Owner is the always-on host | [4_decisions.md](./4_decisions.md#the-owner-is-the-always-on-host-that-followers-can-reach) | [ORB-12488] | recorded |

## Task References

- [ORB-12488] — authored this design folder for the pull-based multi-host drain.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
