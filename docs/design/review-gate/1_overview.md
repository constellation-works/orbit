---
title: Review Gate — Overview
owner: codex
last_updated: 2026-09-21
last_validated: 2026-09-21
status: Accepted
feature: review-gate
doc_role: overview
type: design
summary: Independent automatic code review — before-PR gating with a fresh reviewer and scoped repairs, after-landing scheduling, lineage budgets, and exact-tree delivery coverage.
tags: [review-gate, review-policy, automation, delivery]
paths: ["crates/orbit-config/src/operation.rs", "crates/orbit-core/src/application/review/**", "crates/orbit-store/src/driver/sqlite/review/**", "crates/orbit-automation/src/review/**", "crates/orbit-engine/src/executor/automation/vcs/review_gate.rs"]
related_features: [automation-triggers, activity-job, auditability]
related_artifacts: [ORB-11333, ORB-11528, ORB-11545]
---

# Review Gate — Overview

**Shipped.** [ORB-11333] implements an automatic code review that is
independent of who implemented the work. Before-PR review admits a fresh,
separately configured reviewer that checks the implementation, makes bounded
scoped repairs committed under its own identity, and validates the final
candidate before the PR opens. Passed certificates exclude exactly reproduced
landings from redundant after-landing review while QA stays independent.
Neither review timing nor a reviewer verdict grants merge permission.

The keys live under the `[operation]` table in `config.toml`. That table once
also carried operation-mode presets and grants; those were removed on
2026-09-21 (see [orbit-core decisions](../orbit-core/4_decisions.md)), and the
table name was kept so existing configuration keeps resolving.

## 1. Motivation

An implementer reviewing its own work is not review. Orbit already knows the
exact candidate a delivery run produced, the tasks it claims to satisfy, and
the validation it ran, so the missing piece is a separate invocation that is
given that evidence and is accountable for a verdict — before the PR exists,
where a defect is still cheap, or after landing, where debt can be batched.

The second motivation is not paying for the same review twice. A landing that
reproduces an already-reviewed tree exactly is covered by that certificate;
anything else stays an ordinary review obligation.

## 2. Core Concepts

- **Review timing:** `none`, `before-pr`, or `after-landing`, captured once
  per delivery run in its immutable input and never re-read.
- **Reviewer:** a fresh invocation with its own instruction, tool allowlist
  and wall clock, resolved from `operation.review_crew`. It never becomes the
  implementer and never merges.
- **Lineage budget:** reviewer starts, repair cycles, and wall-time minutes
  bounding one delivery candidate lineage (workspace, task set, base branch).
- **Certificate:** the durable record binding verdict, reviewer identity,
  base/reviewed/final candidate, commits, findings, validation and consumed
  budget. Indexed by final candidate tree when passed.
- **Delivery coverage:** exact-tree exclusion of an already-reviewed landing
  from after-landing review; never a rewrite of pending debt.

Scope covers admission, the reviewer invocation, settlement, budgets,
managed completion under a gate, and coverage. It excludes granting merge
authority, changing task lifecycle, and reviewing the reviewer's own repairs
a second time.

## 3. At a Glance

| Concern | File | Task |
| --- | --- | --- |
| Shipped contract: gate, evidence rules, budgets, coverage, surfaces, rollback | [Design](./2_design.md) | [ORB-11333] |
| What a validation record establishes | [Design §4](./2_design.md#4-what-the-validation-records-establish) | [ORB-11528], [ORB-11545] |
| After-landing scheduling and coverage consumers | [Delivery automation operations](../automation-triggers/5_operations.md) | [ORB-11331] |
| `[operation]` key reference | [CONFIG.md](../../CONFIG.md) | [ORB-11333] |

## Task References

- [ORB-11333] — implements independent review policy, the before-PR gate, lineage budgets, and delivery coverage.
- [ORB-11528] — adds validation-record roles to the certificate contract.
- [ORB-11545] — tightens what a superseded validation record may claim.
- [ORB-11331] — owns the delivery automation consumers that spend certificates.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
