---
title: Finding Verification — Overview
owner: claude
last_updated: 2026-10-28
last_validated: 2026-10-26
status: Accepted
stage: shipped
next: "—"
feature: finding-verification
doc_role: overview
type: design
summary: Review-filed bug findings are rejected by default by the task pilot or the implementer when the defect does not exist; the pooled verifier job is the opt-in gate whose CONFIRMED verdict unlocks backlog and drain admission.
tags: [finding-verification, review, automation, admission]
paths: ["crates/orbit-core/src/application/task/finding_verification.rs", "crates/orbit-core/src/application/task/finding_verification_steps/**", "crates/orbit-core/assets/jobs/finding_verification_pipeline.yaml", "crates/orbit-core/assets/activities/*finding*.yaml", "crates/orbit-core/assets/routines/finding_verification.yaml"]
related_features: [automation-triggers, review-gate, distributed-drain, routines]
related_artifacts: [ORB-15662]
---

# Finding Verification — Overview

Review jobs file what they believe are defects as `bug` tasks. A review can be
wrong: the code may not behave as claimed, the defect may already be fixed, or
the path may be unreachable.

By default a wrong finding is rejected by work that already reads its code
[ORB-19635]. The task pilot returns `finding_invalid` with `path:line`
evidence at its pinned revision, and apply rejects the task. An implementer
that finds the defect absent at pickup hands off a `finding-invalid.json`
report instead of a change, with no workspace gates, and the commit step
rejects the task. Both record the same REJECTED verdict as the job below,
with their own `source`.

The dedicated job is the opt-in alternative. `finding_verification_pipeline` has a read-only
verifier crew judge each such finding against the code at one pinned revision.
It records a CONFIRMED or REJECTED verdict with `path:line` evidence. When a
workspace opts in with `[review] verify_findings = true` (default off), until a
finding is CONFIRMED for its current material, approval, promotion to backlog
and drain admission refuse it and name the job [ORB-15760]. Editing its title, description,
criteria, tags, type or evidence selectors requires verification again;
comment-only edits retain confirmation. Legacy records without a finding
fingerprint cannot unlock admission. A REJECTED verdict moves the task to
`rejected` [ORB-15662].

## 1. Motivation

An unverified finding costs a full delivery when a drain admits it: an
implementer, review rounds, and often a no-diff outcome. Checking the claim
first is far cheaper, and it gives the operator the evidence either way.

## 2. Core Concepts

- **Review finding** — a task of type `bug` tagged `code-review`,
  `delivery-code-review`, `security-review` or `qa-sweep`.
- **Verdict** — `CONFIRMED` or `REJECTED`, with confidence, reasoning and
  evidence cited as `path:line` at the verified sha.
- **Verification record** — the trusted `finding_verified` history entry that
  the `finding-verification` actor writes. Its comment is the copy shown by
  `orbit task show`. Its `source` names the route that decided it:
  `task_pilot`, `implementer`, or absent for the job.
- **Escalation** — a REJECTED verdict from the low tier, or any low-confidence
  verdict, is judged again once by the next complexity tier's pool.
- **Unverified finding hold** — the admission and promotion refusal for a
  finding without a CONFIRMED record matching its current material.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Verdict schema, record, gate predicate, crew draw | [finding_verification.rs](../../../crates/orbit-core/src/application/task/finding_verification.rs) | [ORB-15662] |
| Prepare and apply actions | [application/task/finding_verification_steps/](../../../crates/orbit-core/src/application/task/finding_verification_steps/) | [ORB-15662] |
| Job | [finding_verification_pipeline.yaml](../../../crates/orbit-core/assets/jobs/finding_verification_pipeline.yaml) | [ORB-15662] |
| Verifier activity | [verify_finding.yaml](../../../crates/orbit-core/assets/activities/verify_finding.yaml) | [ORB-15662] |
| State trigger (`finding_unverified`) | [observation.rs](../../../crates/orbit-core/src/application/automation/members/member_host/observation.rs) | [ORB-15662] |
| Seeded routine (disabled) | [finding_verification.yaml](../../../crates/orbit-core/assets/routines/finding_verification.yaml) | [ORB-15662] |
| Admission hold | [pilot/admission.rs](../../../crates/orbit-core/src/application/task/pilot/admission.rs) | [ORB-15662] |
| Pilot `finding_invalid` result and its rejection | [task_pilot.yaml](../../../crates/orbit-core/assets/activities/task_pilot.yaml), [pilot/apply/task.rs](../../../crates/orbit-core/src/application/task/pilot/apply/task.rs) | [ORB-19635] |
| Implementer `finding-invalid.json` report | [commit/finding_invalid.rs](../../../crates/orbit-engine/src/executor/automation/vcs/commit/finding_invalid.rs) | [ORB-19635] |

## Task References

- [ORB-15662] — added the verification job, the verdict record, the promotion
  and admission gate, and the `finding_unverified` state trigger.
- [ORB-19635] — made the task pilot and the implementer the default rejection
  routes, recording the same verdict with its `source`.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
