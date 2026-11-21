---
title: Automation Triggers — Overview
owner: codex
last_updated: 2026-11-18
last_validated: 2026-11-18
status: Draft
stage: shipped
next: "—"
feature: automation-triggers
doc_role: overview
type: design
summary: Bounded delivery and preparation triggers for routines and auto-tasks, with durable work identity and honest coverage.
tags: [automation-triggers, routines, auto-tasks, scheduling]
paths: ["crates/orbit-core/src/application/routines/**", "crates/orbit-core/src/application/auto_tasks/**", "crates/orbit-core/src/application/task/pilot/**"]
related_features: [routines, auto-tasks, review-gate, condition-triggers]
related_artifacts: [ORB-11315, ORB-11295, ORB-11314, ORB-11316]
---

# Automation Triggers — Overview

Delivery triggers are implemented in [ORB-11330], and bounded state preparation
in [ORB-11331]. The earlier terminal failed-run triage target has since been
retired ([distributed-drain §7.2](../distributed-drain/2_design.md#72-failed-run-triage)):
the `execution_failed` trigger kind still parses, but its `task_triage_pipeline`
target is not shipped. Failed runs leave their task blocked with the failure
attached; re-backlogging remains a deliberate human transition. The shared
`orbit-automation` domain uses the existing sweep clock, Core action adapters
and Store checkpoints. See
[Operations](references/operations.md) for accepted configuration and limits. Batched pilot
preparation shipped in [ORB-12746]; [ORB-16839] added mixed-crew
partitioning. Broader coordination and illustrative schema-v2 examples remain
future intent, while operation mode was retired. New workspaces seed the
`task_pilot` routine enabled. Existing workspaces keep their setting.

## 1. Motivation

Cron is appropriate for periodic maintenance, but elapsed hours are a poor proxy
for landed changes, missing preparation, or exhausted execution retries. Repeated
orchestrator inspection spends judgment on mechanically discoverable work. The
proposed triggers make code review and QA due after configured delivery counts,
pilot due for eligible unassessed/stale tasks, and triage due for settled failure
incidents. Cron and intervals remain available for inherently time-based work.

The hard part is what a fire means. A minted QA task may remain in backlog; a
review may fail after examining half a range; a wrapper and its child may report
the same failure. Counting task completions or moving one timestamp at dispatch
would lose work or multiply it. This design makes observation, action creation,
and accepted coverage separate facts.

Goals are predictable bounded work, explainable deferrals, recoverable batches,
explicit authority, and fewer mechanical orchestration turns. Non-goals are a
generic event bus, user scripts, arbitrary transition subscriptions, cross-host
failover, automatic engineering decisions, or a second job scheduler. User
expressions were a non-goal too. [Condition triggers](../condition-triggers/1_scope.md)
reverse that for type-checked, side-effect-free predicates only, as a `condition`
kind beside the typed kinds [ORB-19288]; scripts and the other non-goals stand.
Delivery-triggered review is not a replacement for required PR review or QA.

## 2. Core Concepts

| Concept | Meaning |
| --- | --- |
| Trigger | Typed predicate over time or authoritative state; returns due work and reasons. |
| Consumer | One routine or auto-task definition, qualified by owning workspace/host and definition epoch. |
| Delivery | One verified landing unit on the configured integration branch, independent of how many tasks it closes. |
| Batch | Immutable input, source revisions, coverage obligations, and action identity captured before dispatch/mint. |
| Observation | Source evidence examined and durably retained, including unresolved and filtered evidence. |
| Coverage | Exact obligations whose required examination/application has been accepted; launch and task status are insufficient. |
| Incident | One execution failure cause with explicit child/wrapper/retry lineage and bounded diagnosis. |
| Fresh assessment | Assessment of the current material task/source contract, including a valid decision to leave work unready. |

Triggers decide when work is due; the [review gate](../review-gate/2_design.md)
owns review meaning and content-specific exclusions [ORB-11333]. Explicit
definition values and existing authority decide the rest — operation mode,
which once supplied optional defaults and scoped authorization, was removed on
2026-09-21 ([orbit-core decisions](../orbit-core/4_decisions.md)). Unknown
review coverage remains uncovered.

## 3. At a Glance

| Concern | File | Task and disposition |
| --- | --- | --- |
| Shared evaluator, delivery batches and coverage | [Design](./2_design.md) | [ORB-11330] implemented (done) |
| State preparation and per-member settlement | [Operations](./references/operations.md#state-preparation-orb-11331) | [ORB-11331] implemented (done); failure triage later retired |
| Batched and mixed-crew pilot preparation | [Operations](./references/operations.md#state-preparation-orb-11331) | [ORB-12746] batched members; [ORB-16839] partitioned crews (done) |
| Alternatives, proposed YAML and future coordination | [Vision](./3_vision.md), [Design](./2_design.md) | [ORB-11315] authored the proposal (done) |
| Review-policy relationship | [Review gate](../review-gate/2_design.md) | [ORB-11333] implemented coverage exclusions (done) |
| Condition predicates | [Condition triggers](../condition-triggers/1_scope.md) | [ORB-19288] scoped the separate feature (done) |

Execution waited until the OpenCode prerequisite [ORB-11295] landed as
`8da5a925f` (PR 1382), an ancestor of the inspected checkout `c286142bce`.
The design landed through a dedicated task PR to `agent-main`; publication
did not approve the proposed runtime behavior. Delivery triggers and bounded
state preparation have since shipped in [ORB-11330] and [ORB-11331].

## Task References

- [ORB-11315] — formulates the shared trigger proposal.
- [ORB-11330] — implements delivery triggers on the sweep clock with durable
  batches and coverage checkpoints.
- [ORB-11331] — implements bounded state preparation and failure triage with
  Store checkpoints.
- [ORB-11295] — landed prerequisite before design execution.
- [ORB-11333] — implements review timing, repair bounds, and delivery coverage.
- [ORB-19288] — scopes the `condition` trigger kind beside the typed kinds,
  reversing the user-expression non-goal.
- [ORB-11314] — designed the operation-mode presets for automated preparation
  and delivery; the mode concept was later removed.
- [ORB-11316] — extended the operation-mode design with review timing and
  repair policy; the mode concept was later removed.
- [ORB-12746] — batched due preparation members into one task-pilot run.
- [ORB-16839] — partitioned mixed-crew preparation into bounded pilot groups.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
