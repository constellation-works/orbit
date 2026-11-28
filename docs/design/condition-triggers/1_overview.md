---
title: Condition Triggers — Overview
owner: claude
created_date: 2026-10-27
updated_date: 2026-11-28
last_validated: 2026-11-03
status: Draft
stage: building
next: "File phase 3: host scope and notifications"
feature: condition-triggers
doc_role: overview
type: design
summary: "A condition trigger kind whose predicate is a type-checked CEL expression over task, workspace, host and file state, fired once per subject edge with a durable fire identity, beside the typed trigger kinds"
tags: [condition-triggers, automation-triggers, routines, auto-tasks, scheduling, cel]
---

# Condition Triggers — Overview

Status: draft. This doc is the design deliverable of [ORB-19288]. Phase 1 (the CEL
schema, load-time checking and `orbit automation explain`) shipped in [ORB-19990]. Phase 2
(firing for task and workspace scopes, `when:` guards, chains and `clear-stall`) shipped in
[ORB-20156]; host and file scopes and high-authority actions are still refused. Revised 2026-10-29 with Daniel's answers to four open questions (transitions,
own-task exclusion with `self` reads, follower-host definitions, re-enable); see
[Open questions › Resolved](./2_design.md#resolved-2026-10-29). It reverses one recorded non-goal of
[automation triggers](../automation-triggers/1_overview.md#1-motivation), "user
expressions/scripts", for predicates only. The other non-goals stay: a generic event
bus, scripts, arbitrary transition subscriptions, cross-host failover and a second
scheduler.

Bearing: the pre-condition baseline below was read at `d0cfa7a04`. The shipped
phase-1 and phase-2 changes are recorded in [Phasing](./2_design.md#phasing); baseline claims
about missing predicates or edge state describe the motivation, not current gaps.

- **Two trigger carriers.** A routine (`.orbit/routines/*.yaml`) runs one catalog job.
  Its `trigger` takes one of `cron`, `deliveries_landed` or `state: {kind: …}`
  (`RoutineTrigger`, `crates/orbit-types/src/workflow/routine.rs`). An auto-task
  (`.orbit/auto_tasks/*.yaml`) mints one task from a template. Its `schedule` takes
  exactly one of `cron`, `every_minutes` or `deliveries_landed`
  (`AutoTaskSchedule` and `RawSchedule`,
  `crates/orbit-types/src/workflow/auto_task/definition.rs`). Both structs refuse
  unknown fields.
- **Typed state kinds.** `StateTriggerKind`
  (`crates/orbit-types/src/workflow/automation/members.rs`) has these kinds:
  `preparation_eligible`, `decision_requested`, and the retired `block_escalated`,
  `execution_failed`, `finding_unverified` and `pilot_warnings_held` (the last two retired
  in [ORB-22844]). Each
  kind has one Core observation rule
  (`crates/orbit-core/src/application/automation/members/member_host/observation.rs`).
  The only configurable predicate is `PreparationEligibility`, a fixed struct of
  statuses, required tags, excluded tags and task types.
- **One clock and one store.** `orbit clock tick` runs once per
  `cadence_seconds` (default 60, `crates/orbit-clock/src/clock_settings.rs`).
  It visits each registered owner checkout on the host
  (`crates/orbit-automation/src/routines/loader.rs`) and evaluates routines and
  auto-tasks under a 5-minute `TICK_DEADLINE`
  (`crates/orbit-automation/src/routines/sweep/core.rs`).
- **Fenced checkpoints.** Consumer state is committed with a generation
  compare-and-swap. A losing writer defers with `ConcurrentEvaluation`
  (`crates/orbit-automation/src/checkpoint.rs`).
- **Keyed actions.** Members claim attempts with an `action_key`. Before a job is
  submitted, the job store resolves that key to any existing run
  (`automation_job_for_key`,
  `crates/orbit-store/src/contract/job_run.rs`). Task
  creation can reserve a key, its input digest and the task ID before it writes the
  bundle (`add_task_admitted_guarded`, `crates/orbit-core/src/application/task/add.rs`).
  Time-scheduled auto-tasks use a different method. They claim a slot in a cursor
  file before minting and then record the task ID
  (`fire_slot`, `crates/orbit-automation/src/auto_tasks/scheduler/locked.rs`).
- **Host facts.** The host resource sampler measures CPU (load per core, and measured
  utilization), memory percent, Linux I/O pressure, and disk used percent. Disk is
  measured for the filesystem that holds each of three paths: the repository root,
  `state/worktrees` and the global root (`HostResourceSample`,
  `crates/orbit-runtime/src/host/resource/mod.rs`, and `host_resource_paths`,
  `crates/orbit-runtime/src/host/probes.rs`). A missing value means
  unavailable, never zero. Admission throttling already uses high and resume
  thresholds as hysteresis (`ResourceThrottleSettings`,
  `crates/orbit-config/src/registry/settings/machine.rs`, and
  `crates/orbit-runtime/src/host/resource/pressure.rs`).
- **Authority.** The governed-operation catalog
  (`crates/orbit-common/src/governance/authorization/catalog/entry.rs`) marks routine and
  auto-task toggles and schedule edits from the dashboard as operator-only. The
  `orbit.auto_task` action `add` and `orbit.auto_task` action `update` tools can be granted to agents
  (`grants.rs`). Only the operator, or a registered channel's own plugin callback,
  can answer a decision request (`orbit.decision.submit`,
  [human decision channel](../human-decision-channel/2_design.md#3-delegated-human-authority)).

## Problem

Daniel wants automation that waits for a situation instead of a clock tick or a
hard-coded kind. Four situations he named (2026-10-27):

- **A.** The board has drained, so start release preparation.
- **B.** Release preparation is done, so release and tag.
- **C.** The disk is at 90%, so tell me and run GC.
- **D.** A file arrived in a folder, so run a job on it.

Today each would need a new `StateTriggerKind`: an enum variant, a Core observation
rule, a target job and a release. None of them fits a cron schedule. Polling
with an interval auto-task mints a task on every slot, whether or not anything
happened.

The non-goal had good reasons. The trigger design says a human block or a routine
metadata change must not invoke a model
([automation-triggers §2](../automation-triggers/2_design.md#2-shared-contract-and-ownership)).
Expressions also invite loops, unbounded scans and silent failures.

The current code shapes the fix in four ways:

- **Edge state can live in the existing fenced checkpoint.** Nothing records
  "this subject already fired while true". Delivery coverage and state members are
  the closest models.
- **No workspace-wide change feed exists.** Task events are appended to each task's
  own events file (`TaskEventRowV2`, `crates/orbit-types/src/task/artifacts.rs`).
  The store's task query filters by status and tag
  (`crates/orbit-store/src/contract/task/query.rs`), but has no "changed since"
  filter.
- **Orbit runs no long-lived watcher.** The clock tick is a stateless one-shot
  started by launchd or systemd. Coordination-state automation runs only in owner
  checkouts, but a pull follower's clock still evaluates its replica-local cron
  worktree GC (`worktree_gc_pipeline`) and store GC (`store_gc_pipeline`); every
  other routine, auto-task and recovery stays owner-only (`runs_in_replica`,
  `crates/orbit-core/src/app/routine/loader.rs`; `collect_host_routines`
  consumed at `crates/orbit-core/src/app/routine/sweep/tick.rs`).
- **Any path to tag or publish must keep release authority with Daniel.** RELEASING.md
  and the constellation rules forbid an agent from tagging or publishing unasked.
  npm publish needs Daniel's OTP.

## Goal

A routine or auto-task can fire on a typed, side-effect-free predicate over a
task, the workspace board, the owner host or a file under a declared root. It
fires once each time that predicate becomes true for a subject, and never twice for
the same edge, across crashes, restarts and concurrent evaluators. An operator can
see, at any time, what matches now and why each subject does or doesn't match.

The typed kinds, cron, intervals, delivery coverage and every existing definition
stay unchanged. A condition can't move a task or run a high-authority action
unless an operator grants it explicitly.

## Non-goals

- **Scripts, user functions, or regular expressions in v1.** Predicates only (§5).
- **Task transitions as actions.** Pipeline-only (§6). Approving, re-backlogging or
  completing a task stays a human or typed-pipeline decision.
- **Cross-workspace and cross-host reads.** Each condition reads its owner's
  workspace, and a host condition reads only the host evaluating it (§2, §4).
- **Host definitions outside workspaces.** Definitions stay in workspace `.orbit/`;
  a follower is reached through `hosts:`, not a second definitions root (§4).
- **Replacing typed kinds.** Coverage, fingerprints, slots and receipts stay typed
  (§1).
- **Exactly-once execution of a job's side effects.** Fires are exactly-once at
  action admission. As with every routine, a job's own effects are only as
  idempotent as the job.
- **A generic event bus or webhooks.** These stay non-goals of automation triggers.
- **Unattended releases.** No grant makes a release run without a per-fire human
  approval when the target asks for one (§7).
- **A workflow engine.** Triggers start work, and jobs order it (§11). There are no
  trigger-to-trigger links, nested trigger trees or time-correlation rules.

## Acceptance (feature level)

- A routine or auto-task with `condition` whose expression names an unknown field,
  is not `bool`, uses a computed cross-read index or lacks a task prefilter fails to
  load. The error names the file and the clause, and every other definition still
  loads.
- An older binary that meets a condition definition reports a per-file load error
  and fires nothing.
- `orbit automation explain` shows, without writing anything, the current matches,
  the closest misses and a clause tree with operand values for any subject.
- Enabling a condition with more current matches than `max_matches` is refused. By
  default, enabling baselines current matches without firing.
- A subject that becomes true fires exactly once: across a crash at every step of the
  protocol, a restart, and two concurrent evaluators. It fires again only after its
  `rearm` holds and `when` becomes true again.
- A `for_minutes` hold doesn't fire on a shorter true spell, or across an
  observation gap longer than the continuity limit.
- A fire passes its subject to the action. The minted task or job input carries the
  fire ID, the subject key, the immutable observation revision at the edge and the
  bound variable's fields. Fire and decision history writes never change that
  identity or recapture its input.
- A pull follower and a replica checkout never fire a workspace, task or file
  condition.
- A host condition listing a follower in `hosts` is evaluated on that follower against
  its own host facts and runs only a host-local GC job there; with any other action,
  a mint or a `notify`, it fails to load naming the host and the action.
- `board` counts leave out tasks the definition itself minted unless
  `board_includes_own` is true, and `self.last_fired` and `self.last_minted` read the
  definition's latest fire and mint, surviving edits and disable/re-enable.
- Re-enabling a disabled condition baselines current matches by default; with
  `on_enable: fire_existing` it fires those true at re-enable, and in neither case
  edges that came and went while it was disabled.
- An operator-authority target doesn't run without a current grant whose digest
  matches the definition and target. With `approval: per_fire`, it doesn't run
  without a human answer, and an agent edit stops both.
- A per-fire request's binding stays current across its own trusted `condition_fired`,
  decision-request, delivery and answer bookkeeping, including their history and
  `updated_at` changes. A real subject change between observation and request or
  between request and admission supersedes it even if `when` stays true; a
  definition, target or grant change does likewise. Reverting a change never
  revives the request. The apply step fences freshness and authority checks with
  keyed admission, and retries resolve the original run. Use case B's
  history-40/41/42 trace admits on unchanged freshness 40 and refuses after a real
  change at 43.
- A file condition fires once per new stable file, even for a re-save of the same
  bytes or a rename within its roots. It finds files added while Orbit was down.
  With `group_by: stem`, a `.mp4` and `.vtt` pair fires once, in either arrival order.
- A root the evaluator can't read is a load or enable error naming the root, or a
  `root_unreadable` stall after enable. It is never a match of zero subjects, and its
  tombstones are kept.
- A `when:` guard on a `cron`, `every_minutes`, `deliveries_landed` or `state` kind
  suppresses the fire while false or unknown. It never drops delivery debt or state
  members.
- Each task a fire mints carries its origin (trigger, fire ID, depth). A fire past
  depth 5 is refused and recorded, and explain shows each fire's upstream chain.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## Task References

- [ORB-19288] — designs condition triggers (this doc).
- [ORB-19990] — shipped phase 1: CEL schema, load-time checking and `orbit automation explain`.
- [ORB-20156] — shipped phase 2: firing for task and workspace scopes, `when:` guards, chains and `clear-stall`.
- [ORB-22844] — retired the `finding_unverified` and `pilot_warnings_held` trigger sources with their routines.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
