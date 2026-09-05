---
title: Operation Mode — Overview
owner: codex
last_updated: 2026-09-05
last_validated: 2026-09-05
status: Draft
feature: operation-mode
doc_role: overview
type: design
summary: Proposed operation-mode presets for scoped preparation, promotion, delivery, and bounded recovery through existing Orbit pipelines.
tags: [operation-mode, automation, authorization]
paths: ["crates/orbit-core/assets/jobs/**", "crates/orbit-core/src/application/job/**", "crates/orbit-config/src/**"]
related_features: [activity-job, routines, task-artifacts, auditability]
related_artifacts: [ORB-11314]
---

# Operation Mode — Overview

**Proposed; documentation only.** Operation mode would bundle Orbit's existing
pipeline controls into two understandable presets, tentatively **supervised**
and **autonomous**. Astra would retain engineering judgment about worthwhile
work, scope, architecture, and unresolved tradeoffs. Orbit would take over
repeated preparation, eligible promotion, admission, polling, and bounded
recovery. This proposal neither implements nor enables those changes.

## 1. Motivation

An orchestrator currently spends reasoning and context on mechanical turns:
finding unprepared tasks, invoking pilots, checking outcomes, promoting approved
work, refilling execution slots, and diagnosing familiar delivery failures.
Those turns compete with the decisions for which a capable model is useful.
The objective is less orchestrator consumption **per accepted change**, with
quality and human control preserved; a larger completed-task count alone is
not success.

Daniel's initial **fast/slow** examples were automatic completion, more frequent
`task_pilot_pipeline` runs, promotion after preparation, recovery enabled, and
parallelism up to ten. These are useful requirements, but speed conflates
authority with capacity. A supervised run may use ten workers; an autonomous
run may use one. Prefer supervised/autonomous as working names, with cadence
and concurrency shown separately. The final names remain open.

The proposed preset behavior is:

| Control | Supervised | Autonomous, after explicit scoped enablement |
| --- | --- | --- |
| Preparation | Explicit pilot runs and existing enabled routines | Automatically refresh eligible stale/unprepared tasks; suggested five-minute due interval |
| Proposed → backlog | Separate approval | Deterministic promotion only with fresh successful pilot evidence, no decision blockers, and a matching grant |
| Completion | Default `review`; explicit existing completion authorization remains possible | Request `done` within the grant; PR-only or other repository constraints cap delivery |
| Recovery | Existing configured step recovery; explicit or already enabled triage | Schedule bounded diagnosis and eligible retry through existing recovery/triage paths |
| Leaf concurrency | Existing default five, overridable | Suggested ceiling ten, bounded by capacity, hard limits, reservations, and conflicts |
| Window | Existing one-tick/window behavior | Explicit admission scope, with a bounded window recommended; standing scope requires explicit selection |

These are proposed defaults, not shipped mode values. Selecting the autonomous
preference in a global config must not itself authorize a workspace's future
tasks. The proposed enable operation makes the scope and requested promotion /
completion rights explicit and records that authorization once. It need not
ask again for each eligible task inside that grant.

## 2. Core Concepts

- **Preset:** desired operating defaults; it is neither a crew nor a delivery
  branch. It does not choose Astra's model or change repository policy.
- **Scope grant:** a durable, attributable authorization for specified
  workspaces, tasks or an explicit dynamic selection, operations, and optionally
  a bounded admission window. Preparation, promotion, and completion are
  separate rights.
- **Effective policy:** the resolved settings and grant references captured
  for a run; child work inherits a bounded subset of its parent's authority.
- **Preparation evidence:** a successful pilot assessment tied to task meaning,
  source revision, and decision disposition. A populated selector list is not
  approval or proof of readiness.
- **Escalation:** a durable task diagnosis requiring Astra or a human decision,
  with evidence and exhausted limits. It is a legitimate outcome of automation.

Scope includes configuring and composing current jobs, scheduling preparation,
validating promotion, explaining effective policy, and evaluating overhead.
It excludes autonomous invention of product priorities, silent architecture
choices, a second pipeline engine, a generic policy framework, model reassignment,
weaker validation, and changes to merge authorization or protected branches.

## 3. At a Glance

| Concern | File | Task |
| --- | --- | --- |
| Source-verified current behavior and extension seams | [Current design](./2_design.md) | [ORB-11314] |
| Proposed resolution, authority, promotion, recovery, rollout, and evaluation | [Vision](./3_vision.md) | [ORB-11314] |
| Repository ownership and dependency constraints | [Architecture](../../../ARCHITECTURE.md) | [ORB-11314] verifies existing boundaries |

## Task References

- [ORB-11314] — proposes operation-mode presets without runtime changes.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
