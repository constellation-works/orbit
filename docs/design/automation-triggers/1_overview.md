---
title: Automation Triggers — Overview
owner: codex
last_updated: 2026-09-05
last_validated: 2026-09-05
status: Draft
feature: automation-triggers
doc_role: overview
type: design
summary: Proposed bounded state-driven triggers for routines and auto-tasks with durable work identity and honest coverage.
tags: [automation-triggers, routines, auto-tasks, scheduling]
paths: ["crates/orbit-core/src/application/routines/**", "crates/orbit-core/src/application/auto_tasks/**", "crates/orbit-core/src/adapter/engine_host/v2_host/task_pilot/**", "crates/orbit-core/src/adapter/engine_host/v2_host/triage.rs"]
related_features: [routines, auto-tasks, operation-mode]
related_artifacts: [ORB-11315, ORB-11295, ORB-11314, ORB-11316]
---

# Automation Triggers — Overview

**Proposed; documentation only.** Share the decision that work is due across
routines and auto-tasks while preserving their different actions: invoke a job
or mint a task. The existing sweep clock can reconcile durable state in bounded
passes. A new resident daemon is not required. Astra formulated this proposal
for [ORB-11315]; no definition, scheduler, authority, or runtime state is changed.

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
generic event bus, user expressions/scripts, arbitrary transition subscriptions,
cross-host failover, automatic engineering decisions, or a second job scheduler.
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

[Operation mode](../operation-mode/3_vision.md) supplies optional defaults and
scoped authorization; triggers decide when work is due. Its review-policy
extension owns review meaning and content-specific exclusions. Neither proposal
must land first: absent mode support, explicit definition values and existing
authority suffice. Unknown review coverage remains uncovered.

## 3. At a Glance

| Concern | File | Task |
| --- | --- | --- |
| Current seams, proposed contract, YAML, timelines, rollout | [Design](./2_design.md) | ORB-11315 |
| Alternatives, costs, unresolved decisions | [Vision](./3_vision.md) | ORB-11315 |
| Existing job scheduling | [Routines](../routines/2_design.md) | ORB-11315 inspection |
| Existing task minting | [Auto-tasks](../auto-tasks/2_design.md) | ORB-11315 inspection |
| Mode and review-policy relationship | [Operation-mode proposal](../operation-mode/3_vision.md) | ORB-11314, ORB-11316 |

Execution waited until the OpenCode prerequisite [ORB-11295] landed as
`8da5a925f` (PR 1382), an ancestor of the inspected checkout `c286142bce`.
This candidate belongs in a dedicated task PR to `agent-main`, left unmerged;
publication does not approve the proposed runtime behavior.

## Task References

- [ORB-11315] — formulates the shared trigger proposal.
- [ORB-11295] — landed prerequisite before design execution.
- [ORB-11314] — proposes operation-mode defaults and scoped automation.
- [ORB-11316] — extends mode with review timing, repair bounds, and coverage.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
