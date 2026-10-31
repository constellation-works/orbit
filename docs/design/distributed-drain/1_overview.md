---
title: Distributed Drain — Overview
owner: claude
last_updated: 2026-10-28
last_validated: 2026-10-28
status: Draft
stage: building
next: "Owner review of the building stage now that receipt compaction and accounting have an operator caller (ORB-19424)."
feature: distributed-drain
doc_role: overview
type: design
summary: Run the workspace drain on more than one host against one owner store — followers pull one task at a time from the owner's ready queue over federated MCP, validate where they built, and land through the owner.
tags: [distributed-drain, multi-host, pull, federated-mcp]
paths: ["crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml", "crates/orbit-core/assets/activities/classify_workspace_auto_tasks.yaml", "crates/orbit-store/src/repository/task/coordination/admission/**", "crates/orbit-core/src/application/automation/ownership.rs", "crates/orbit-cmd/src/registry/runtime/mod.rs", "crates/orbit-mcp/**"]
related_features: [distributed-drain, federated-mcp, host-registry, activity-job, state-compatibility, task-migration, automation-triggers]
related_artifacts: [ORB-12488, ORB-12490, ORB-12491, ORB-12492, ORB-12495, ORB-12500, ORB-12516, ORB-12528, ORB-12616, ORB-12617, ORB-13625, ORB-13639, ORB-13642, ORB-13649, ORB-13663, ORB-13664, ORB-13992, ORB-13908]
---

# Distributed Drain — Overview

> **Status: Draft, building; core runtime live.** [ORB-12488] authored the contract.
> The owner's claim and admission substrate, the internal `orbit.task.pull` / `orbit.drain.claim.bind` /
> `orbit.drain.claim.settle` runtime tools, the routed follower peer (`orbit run auto --pull`), handoff
> acceptance and approval, and the landing consumer are all live ([§3](#3-at-a-glance) tracks each
> concern). The design stays Draft while operation of the multi-host drain still teaches it
> things; [2_design.md](./2_design.md) carries the same status. [ORB-13806] removed the
> drain protocol operations from public MCP; followers use the internal owner route.

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

Before distributed admission, `classify_workspace_auto_tasks` treated a live
`task_auto_pipeline` run on its own host as the only claim record for a backlog task.
That host-local record could not coordinate a second drain. [ORB-12528] landed the
transactional admission substrate and [ORB-13625] enabled follower pull against it,
so a second host can now add capacity without becoming a second control plane.
Two independent owner stores over one repository remain an operator misconfiguration.

Existing roles, capability routing, reservations, and PR checks were the foundations. V1 added
atomic admission, durable request/claim identity, routed task reads and writes, settlement, and an
owner landing consumer. With the owner's before-PR review (`review.timing = "before_pr"`) on,
each claimed PR leaf runs the reviewer the claim captured before it opens its pull request
[ORB-13908]; [ORB-13992] separated after-landing review from admission. Implementation validation still
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

**Claim.** Durable authority for one task execution attempt, bound to the execution machine
in trusted runtime invocation context and then one leaf run. The SSH machine label is
caller-chosen, not an authenticated principal; [ORB-14898] clarified this fence. Recovery
revokes that authority before allowing a replacement attempt.

**Slot.** Local capacity less live leaf runs and pending admissions not yet represented by those
runs. The owner does not track host capacity.

**Epic (tag).** A size hint on a task: one large piece of work a top-tier crew takes on whole. It
no longer names a pipeline, a worktree, or a reservation class. Admission has one carve-out for
it: a tagged root with no `context_files` of its own is withheld while a descendant has some
([§7.1](./2_design.md#71-epic-machinery)).

**Handoff.** Durable PR (or owner-local candidate), validation evidence, and typed evidence
for the captured review timing, or `not_required` when review is off. The owner accepts it
atomically with promotion to `review`. Execution writes close at this boundary.
[ORB-13908] enabled before-PR review; [ORB-14849] added before-landing timing, and
[ORB-15192] made that timing selectable by executor host.

**Landing.** An owner-side consumer verifies the pinned candidate and merges only with recorded
completion authority, then verifies actual merge evidence before marking the task done. This
consumer is live, triggered by accepted authorized handoffs or explicit owner approval.
The ship sweep is retired: its routine, wrapper and separate CLI were removed, and a leftover
routine loads as retired and never fires. Owner dispatch is an `orbit run auto` drain or an
explicit `orbit run ship`; every entry point uses the same claim admission. No schedule is
enabled by this design.

The core execution and landing slices are done. Admission receipt compaction and storage
accounting were first removed as unused ([ORB-15332], [ORB-17641]); [ORB-19424] restored both
with an operator caller, `orbit doctor admission-receipts`. Without `--compact` the command
reports receipt counts and logical payload bytes. With it, compaction moves idle and settled
receipts to permanent tombstones and keeps full receipts for unsettled claims and for failed or
revoked claims with a preserved PR. The feature remains `building` per its frontmatter stage.

The separate fault-matrix audit [ORB-12501] was explicitly deferred by the owner and
is now archived; [ORB-12617] completed the mixed-admission and end-to-end fixture
slice, not that entire audit. [ORB-19393] completed inherited-check waits and
externally merged handoff reconciliation; its integration delivery landed during
this reconciliation, after the pinned source checkout.

## 3. At a Glance

| Concern | Where | Task | Status |
|---------|-------|------|--------|
| Contract: ready queue, `orbit.task.pull`, refusals, invariants | [specs/task-pull.md](./specs/task-pull.md) | [ORB-12488] | done |
| Transactional ready selection on the owner | [crates/orbit-store/src/repository/task/coordination/admission/admit.rs](../../../crates/orbit-store/src/repository/task/coordination/admission/admit.rs) (`admit_locked`); [2_design.md §2](./2_design.md#2-the-ready-queue-and-orbittaskpull) | [ORB-12528] | done |
| Request receipt + claim + reservation + status in one transaction | `TaskCommitBoundary::admit_task` in [crates/orbit-store/src/repository/task/coordination/admission/admit.rs](../../../crates/orbit-store/src/repository/task/coordination/admission/admit.rs) | [ORB-12528] | done |
| Pull-mode drain loop (`orbit run auto --pull`), owner pull/bind/settle tools, routed follower peer | [workspace_pull_pipeline.yaml](../../../crates/orbit-core/assets/jobs/workspace_pull_pipeline.yaml) | [ORB-13625] | done |
| Claimed leaf dispatch, binding, and terminal settlement | [task_claimed_pr_pipeline.yaml](../../../crates/orbit-core/assets/jobs/task_claimed_pr_pipeline.yaml), [task_claimed_local_pipeline.yaml](../../../crates/orbit-core/assets/jobs/task_claimed_local_pipeline.yaml) | [ORB-12616], [ORB-13642], [ORB-13663] | done |
| Replica task reads and coordination writes route to the owner | [crates/orbit-cmd/src/registry/runtime/selection.rs](../../../crates/orbit-cmd/src/registry/runtime/selection.rs), [crates/orbit-core/src/adapter/tool_host/worker_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/worker_tools.rs), [crates/orbit-mcp](../../../crates/orbit-mcp) | [ORB-13625], [ORB-13649] | done |
| Manual claim inspection and recovery | `orbit.drain.claims` listing; dashboard `claim.recover`; CLI `orbit task recover-claim`; [2_design.md §3.1](./2_design.md#31-attempt-ownership-and-recovery) | [ORB-12495], [ORB-12516], [ORB-19071] | done |
| Durable handoff and authorized landing consumer | [crates/orbit-core/src/application/landing/mod.rs](../../../crates/orbit-core/src/application/landing/mod.rs), [task_landing_pipeline.yaml](../../../crates/orbit-core/assets/jobs/task_landing_pipeline.yaml); [2_design.md §3.2](./2_design.md#32-durable-review-and-landing-handoff) | [ORB-12497], [ORB-12499] | done |
| Review-only handoff approval and revocation | dashboard `handoff.approve` / `handoff.revoke`; CLI `orbit task revoke-handoff`; [2_design.md §3.2](./2_design.md#32-durable-review-and-landing-handoff) | [ORB-12516], [ORB-19071] | done |
| Non-pruning selector storage/projection and frozen footprints | `declared_context_files` in [crates/orbit-runtime/src/task/mod.rs](../../../crates/orbit-runtime/src/task/mod.rs); [2_design.md §2](./2_design.md#2-the-ready-queue-and-orbittaskpull) | [ORB-12490] | done |
| Refuse pulls from callers that cannot run the review timing the owner captured, and require typed handoff evidence | `admission_refusal` in [refusal.rs](../../../crates/orbit-store/src/repository/task/coordination/admission/refusal.rs); [2_design.md §3.2](./2_design.md#32-durable-review-and-landing-handoff) | [ORB-13908], [ORB-14849], [ORB-15192] | done |
| Claim-aware capacity accounting and interrupted-run recovery | [leaf_occupancy.rs](../../../crates/orbit-core/src/application/admission/leaf_occupancy.rs); [2_design.md §3](./2_design.md#3-pull-mode-drain-and-the-pulled-leaf-pipeline) | [ORB-12617] | done |
| Mixed admission and end-to-end acceptance fixtures | [2_design.md §8](./2_design.md#8-required-validation-scenarios) | [ORB-12617] | done |
| Full fault-matrix closing audit | [2_design.md §8](./2_design.md#8-required-validation-scenarios) | [ORB-12501] | deferred by owner; archived |
| Admission receipt compaction and storage accounting | [2_design.md §2](./2_design.md#2-the-ready-queue-and-orbittaskpull); [specs/task-pull.md](./specs/task-pull.md#internal-storage-accounting); `orbit doctor admission-receipts` | [ORB-19424] | done |
| Reliable settlement and foreground agent checks | [2_design.md §3](./2_design.md#3-pull-mode-drain-and-the-pulled-leaf-pipeline) | [ORB-13639], [ORB-13663], [ORB-13664] | done |
| Execution provenance on runs, tasks, artifacts | [2_design.md §6](./2_design.md#6-execution-provenance) | [ORB-13649] | done |
| Retire epic machinery; `epic` becomes a tag | [2_design.md §7.1](./2_design.md#71-epic-machinery) | [ORB-12491] | done |
| Adapt all entry points to claim admission (ship sweep since retired) | [2_design.md §7.3](./2_design.md#73-ship-sweep) | [ORB-12500] | done |
| Retire failed-run triage | [2_design.md §7.2](./2_design.md#72-failed-run-triage) | [ORB-12492] | done |
| Read-only identity/capability/version probe and receipt reconciliation | `orbit.drain.probe`, `orbit.drain.receipt.lookup`; [2_design.md §4](./2_design.md#4-follower-preconditions) | [ORB-12495] | done |
| Followers pull; the owner never places | [4_decisions.md](./4_decisions.md#followers-pull-the-owner-never-places) | [ORB-12488] | recorded |
| Requests identify admissions and claims identify attempts | [4_decisions.md](./4_decisions.md#requests-identify-admissions-and-claims-identify-attempts) | [ORB-12488] | recorded |
| Owner ordering does not require a materialized queue | [4_decisions.md](./4_decisions.md#owner-ordering-does-not-require-a-materialized-queue) | [ORB-12488] | recorded |
| Scarcity-aware admission order: work only the requester can run leads its band | `census_dispatch_order` in [census.rs](../../../crates/orbit-store/src/contracts/task/coordination/census.rs); [specs/task-pull.md](./specs/task-pull.md#executor-census-and-affinity); [4_decisions.md](./4_decisions.md#admission-prefers-work-only-the-requesting-executor-can-run) | [ORB-17304] | done |
| Inherited-check waits and externally merged handoff reconciliation | [2_design.md §3.2](./2_design.md#32-durable-review-and-landing-handoff) | [ORB-19393] | done |
| Landing consumes durable evidence and explicit completion authority | [4_decisions.md](./4_decisions.md#landing-consumes-durable-evidence-and-explicit-completion-authority) | [ORB-12488] | recorded |
| Owner is the always-on host | [4_decisions.md](./4_decisions.md#the-owner-is-the-always-on-host-that-followers-can-reach) | [ORB-12488] | recorded |
| Epic is a tag, not a pipeline | [4_decisions.md](./4_decisions.md#epic-is-a-tag-not-a-pipeline) | [ORB-12488] | recorded |
| Blocked tasks wait for a reader, not a classifier | [4_decisions.md](./4_decisions.md#blocked-tasks-wait-for-a-reader-not-a-classifier) | [ORB-12488] | recorded |

## Task References

- [ORB-12488] — authored this design folder for the pull-based multi-host drain.
- [ORB-12492] — retired terminal failed-run triage (§7.2).
- [ORB-12491] — retired epic execution (§7.1).
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
- [ORB-13992] — admission keys only on before-PR review; the probe reports both review switches.
- [ORB-17304] — implemented scarcity-aware admission against the live executor census.
- [ORB-12497] — implemented typed handoff acceptance, approval, revocation and landing-start requests.
- [ORB-12499] — implemented the authorized owner landing consumer and uncertain-merge reconciliation.
- [ORB-12501] — owned the closing fault-matrix audit, explicitly deferred by the owner and now archived.
- [ORB-13806] — withdrew distributed-drain protocol operations from public MCP.
- [ORB-13908] — enabled the before-PR review gate on claimed leaves.
- [ORB-14849] — added before-landing review against the published PR head.
- [ORB-14898] — clarified the caller-chosen machine fence and checked SSH worker/session consistency.
- [ORB-15192] — made before-landing review selectable by executor machine.
- [ORB-19071] — added operator CLI/MCP claim recovery and handoff revocation.
- [ORB-19393] — implemented inherited-check waits and externally merged handoff reconciliation.
- [ORB-15332] — removed the unused admission compaction helper without changing runtime behavior.
- [ORB-17641] — removed the unused admission receipt accounting API.
- [ORB-19424] — added the operator caller (`orbit doctor admission-receipts`, with `--compact`) for admission receipt compaction and storage accounting.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
