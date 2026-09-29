---
title: Distributed Drain — Overview
owner: claude
last_updated: 2026-09-29
last_validated: 2026-09-29
status: Draft
feature: distributed-drain
doc_role: overview
type: design
summary: Run the workspace drain on more than one host against one owner store — followers pull one task at a time from the owner's ready queue over federated MCP, validate where they built, and land through the owner.
tags: [distributed-drain, multi-host, pull, federated-mcp, resident-orchestrator]
paths: ["crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml", "crates/orbit-core/assets/activities/classify_workspace_auto_tasks.yaml", "crates/orbit-store/src/repository/task/coordination/admission.rs", "crates/orbit-core/src/application/automation/ownership.rs", "crates/orbit-cmd/src/registry/runtime/mod.rs", "crates/orbit-mcp/**"]
related_features: [distributed-drain, federated-mcp, host-registry, activity-job, state-compatibility, task-migration, automation-triggers]
related_artifacts: [ORB-12488, ORB-12490, ORB-12491, ORB-12492, ORB-12495, ORB-12500, ORB-12516, ORB-12528, ORB-12616, ORB-12617, ORB-13625, ORB-13639, ORB-13642, ORB-13649, ORB-13663, ORB-13664]
---

# Distributed Drain — Overview

> **Status: Draft, live.** The contract [ORB-12488] authored is implemented: the owner's claim and
> admission substrate, the registered `orbit.task.pull` / `orbit.drain.claim.bind` /
> `orbit.drain.claim.settle` tools, the routed follower peer (`orbit run auto --pull`), handoff
> acceptance and approval, and the landing consumer are all live ([§3](#3-at-a-glance) tracks each
> concern). The design stays Draft while operation of the multi-host drain still teaches it
> things; [2_design.md](./2_design.md) carries the same status.

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

Existing roles, capability routing, reservations, and PR checks were the foundations. V1 added
atomic admission, durable request/claim identity, routed task reads and writes, settlement, and an
owner landing consumer. V1 supports only `review_policy = none`; implementation validation still
runs, and declared context selectors remain protected even when their files do not exist. These
were substantive integration changes. Throughput depends on file conflicts, provider limits, shared CI,
and landing capacity as well as local build slots.

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

**Handoff.** Durable PR (or owner-local candidate), validation evidence, and a typed
`review_policy: none` disposition, accepted by the owner atomically with promotion to `review`. Execution writes close at this boundary.

**Landing.** An owner-side consumer verifies the pinned candidate and merges only with recorded
completion authority, then verifies actual merge evidence before marking the task done. This
consumer is live, triggered by accepted authorized handoffs or explicit owner approval.
Ship-sweep routines, their wrapper, and the separate CLI remain available alongside explicit drains;
every entry point uses the same claim admission. No schedule is enabled by this design.

## 3. At a Glance

| Concern | Where | Task | Status |
|---------|-------|------|--------|
| Contract: ready queue, `orbit.task.pull`, refusals, invariants | [specs/task-pull.md](./specs/task-pull.md) | [ORB-12488] | done |
| Transactional ready selection on the owner | [crates/orbit-store/src/repository/task/coordination/admission.rs](../../../crates/orbit-store/src/repository/task/coordination/admission.rs) (`admit_locked`); [2_design.md §2](./2_design.md#2-the-ready-queue-and-orbittaskpull) | [ORB-12528] | done |
| Request receipt + claim + reservation + status in one transaction | `TaskCommitBoundary::admit_task` in [crates/orbit-store/src/repository/task/coordination/admission.rs](../../../crates/orbit-store/src/repository/task/coordination/admission.rs) | [ORB-12528] | done |
| Pull-mode drain loop (`orbit run auto --pull`), owner pull/bind/settle tools, routed follower peer | [workspace_pull_pipeline.yaml](../../../crates/orbit-core/assets/jobs/workspace_pull_pipeline.yaml) | [ORB-13625] | done |
| Claimed leaf dispatch, binding, and terminal settlement | [task_claimed_pr_pipeline.yaml](../../../crates/orbit-core/assets/jobs/task_claimed_pr_pipeline.yaml), [task_claimed_local_pipeline.yaml](../../../crates/orbit-core/assets/jobs/task_claimed_local_pipeline.yaml) | [ORB-12616], [ORB-13642], [ORB-13663] | done |
| Replica task reads and coordination writes route to the owner | [crates/orbit-cmd/src/registry/runtime/selection.rs](../../../crates/orbit-cmd/src/registry/runtime/selection.rs), [crates/orbit-core/src/adapter/tool_host/worker_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/worker_tools.rs), [crates/orbit-mcp](../../../crates/orbit-mcp) | [ORB-13625], [ORB-13649] | done |
| Manual claim inspection and recovery | `orbit.drain.claims` listing; dashboard `claim.recover`; [2_design.md §3.1](./2_design.md#31-attempt-ownership-and-recovery) | [ORB-12495], [ORB-12516] | done |
| Durable handoff and authorized landing consumer | [crates/orbit-core/src/application/landing/mod.rs](../../../crates/orbit-core/src/application/landing/mod.rs), [task_landing_pipeline.yaml](../../../crates/orbit-core/assets/jobs/task_landing_pipeline.yaml); [2_design.md §3.2](./2_design.md#32-durable-review-and-landing-handoff) | — | live |
| Review-only handoff approval and revocation | dashboard `handoff.approve` / `handoff.revoke`; [2_design.md §3.2](./2_design.md#32-durable-review-and-landing-handoff) | [ORB-12516] | done |
| Non-pruning selector storage/projection and frozen footprints | `declared_context_files` in [crates/orbit-core/src/runtime/task/mod.rs](../../../crates/orbit-core/src/runtime/task/mod.rs); [2_design.md §2](./2_design.md#2-the-ready-queue-and-orbittaskpull) | [ORB-12490] | done |
| Enforce v1 review policy `none` and typed handoff evidence | `admission_refusal` in [admission.rs](../../../crates/orbit-store/src/repository/task/coordination/admission.rs); [2_design.md §3.2](./2_design.md#32-durable-review-and-landing-handoff) | — | done |
| Claim-aware capacity accounting and interrupted-run recovery | [leaf_occupancy.rs](../../../crates/orbit-core/src/adapter/engine_host/v2_host/admission/leaf_occupancy.rs); [2_design.md §3](./2_design.md#3-pull-mode-drain-and-the-pulled-leaf-pipeline) | [ORB-12617] | done |
| Failure and concurrency acceptance coverage | [2_design.md §8](./2_design.md#8-required-validation-scenarios) | [ORB-12617] | partial |
| Execution provenance on runs, tasks, artifacts | [2_design.md §6](./2_design.md#6-execution-provenance) | [ORB-13649] | done |
| Retire epic machinery; `epic` becomes a tag | [2_design.md §7.1](./2_design.md#71-epic-machinery) | [ORB-12491] | done |
| Retain ship sweep and adapt all entry points to claim admission | [2_design.md §7.3](./2_design.md#73-ship-sweep) | [ORB-12500] | done |
| Retire failed-run triage | [2_design.md §7.2](./2_design.md#72-failed-run-triage) | [ORB-12492] | done |
| Read-only identity/capability/version probe and receipt reconciliation | `orbit.drain.probe`, `orbit.drain.receipt.lookup`; [2_design.md §4](./2_design.md#4-follower-preconditions) | [ORB-12495] | done |
| Followers pull; the owner never places | [4_decisions.md](./4_decisions.md#followers-pull-the-owner-never-places) | [ORB-12488] | recorded |
| Requests identify admissions and claims identify attempts | [4_decisions.md](./4_decisions.md#requests-identify-admissions-and-claims-identify-attempts) | [ORB-12488] | recorded |
| Owner ordering does not require a materialized queue | [4_decisions.md](./4_decisions.md#owner-ordering-does-not-require-a-materialized-queue) | [ORB-12488] | recorded |
| Landing consumes durable evidence and explicit completion authority | [4_decisions.md](./4_decisions.md#landing-consumes-durable-evidence-and-explicit-completion-authority) | [ORB-12488] | recorded |
| Owner is the always-on host | [4_decisions.md](./4_decisions.md#the-owner-is-the-always-on-host-that-followers-can-reach) | [ORB-12488] | recorded |
| Epic is a tag, not a pipeline | [4_decisions.md](./4_decisions.md#epic-is-a-tag-not-a-pipeline) | [ORB-12488] | recorded |
| Blocked tasks wait for a reader, not a classifier | [4_decisions.md](./4_decisions.md#blocked-tasks-wait-for-a-reader-not-a-classifier) | [ORB-12488] | recorded |

## Task References

- [ORB-12488] — authored this design folder for the pull-based multi-host drain.
- [ORB-12492] — retired terminal failed-run triage (§7.2).
- [ORB-12491] — retired epic execution and added the isolated retirement check (§7.1).
- [ORB-12490] — replaced context pruning with `declared_context_files`.
- [ORB-12495] — added the probe, receipt lookup and claim listing.
- [ORB-12500] — added published-PR observation and the shared entry-point admission decision.
- [ORB-12516] — added owner dashboard claim state and actions.
- [ORB-12528] — landed the task/reservation commit boundary.
- [ORB-12616] — made the owner-local claimed leaf executable.
- [ORB-12617] — unified capacity accounting and added fault-injection acceptance fixtures.
- [ORB-13625] — registered `orbit.task.pull`, claim bind and settle, and the routed follower peer.
- [ORB-13639] — the follower closes its local settlement when the owner has already ended the claim (revoked or recovered), instead of aborting the drain pass on `stale_claim`.
- [ORB-13642] — added claimed-mode implementation and evidence-bearing failure settlement.
- [ORB-13649] — scoped run-keyed task lookups to the run ID plus the executing machine, after an owner and a follower minted the same run ID.
- [ORB-13663] — moved settlement from the admitting drain to the admission record, so cancelling a drain no longer strands its live leaves.
- [ORB-13664] — the claude provider disables background tasks on every child it spawns, so an agent's validation gates run in the foreground and finish before the leaf reports.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
