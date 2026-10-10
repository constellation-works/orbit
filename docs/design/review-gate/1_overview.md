---
title: Review Gate — Overview
owner: codex
last_updated: 2026-10-09
last_validated: 2026-10-08
status: Accepted
feature: review-gate
doc_role: overview
type: design
summary: Independent automatic code review — before-PR gating or a before-landing review of the open PR, each by a fresh reviewer that fixes its findings as a second commit, after-landing scheduling, one review per candidate, and exact-tree delivery coverage.
tags: [review-gate, review-policy, automation, delivery]
paths: ["crates/orbit-config/src/operation.rs", "crates/orbit-core/src/application/review/**", "crates/orbit-store/src/driver/sqlite/review/**", "crates/orbit-automation/src/review/**", "crates/orbit-engine/src/executor/automation/vcs/review_gate.rs"]
related_features: [automation-triggers, activity-job, auditability]
related_artifacts: [ORB-11333, ORB-11528, ORB-11545, ORB-13989, ORB-13992, ORB-14849]
---

# Review Gate — Overview

**Shipped.** [ORB-11333] implements an automatic code review that is
independent of who implemented the work. Before-PR review admits a fresh,
separately configured reviewer that checks the implementation commit, fixes
what it finds as one second commit (`review: <summary>`) under its own
identity, and comments every finding with what changed for it [ORB-13989].
The verdict is `accept` (no fixes), `accept_with_fixes` (owner validation
reruns on the reviewer commit before the PR opens, and the PR body gains a
"Review fixes" section), or `reject` (the task blocks with both commits
preserved; there is no second review round). Passed certificates exclude exactly reproduced
landings from redundant after-landing review while QA stays independent.
Neither review timing nor a reviewer verdict grants merge permission.

Review runs at three timings [ORB-13992] [ORB-14849]: `review.before_pr` in
`config.toml` holds PR creation for the reviewer; `review.before_landing`
opens the PR first and reviews it while hosted CI runs, landing only the head
that review settled; and the `delivery-code-review` auto-task's own `enabled`
flag reviews landed deliveries in batches. Both config switches share
`review.minutes`, the limit for one candidate's review, and there is one
review layer before landing: config load fails while both are on. The reviewer crew stays `operation.review_crew`; the
`[operation]` table once also carried operation-mode presets and grants,
removed on 2026-09-21 (see [orbit-core decisions](../orbit-core/4_decisions.md)).

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

- **Before-PR switch:** `review.before_pr`, captured once per delivery run
  or drain in its immutable input and never re-read.
- **Before-landing switch:** `review.before_landing`, captured the same way;
  the reviewer reviews the published PR head, a fix is pushed under a lease
  on that head, and any outcome but an approve leaves the PR open in review.
- **After-landing switch:** the `delivery-code-review` auto-task's `enabled`
  flag; it never affects delivery admission.
- **Reviewer:** a fresh invocation with its own instruction, tool allowlist
  and wall clock, resolved from `operation.review_crew`. It never becomes the
  implementer and never merges.
- **One review per candidate:** each candidate in a delivery run lineage
  (workspace, task set, base branch, and the run with its resumes) gets one
  review, bounded by `review.minutes` of reviewer wall clock.
- **Two-commit shape:** the implementation commit, never amended, then at
  most one reviewer commit carrying every fix; owner validation reruns on
  the reviewer commit before publication, and its paths widen the task's
  selectors.
- **Certificate:** the durable record binding verdict, reviewer identity,
  base/reviewed/final candidate, commits, findings with what each fix
  changed, validation and consumed
  budget. Indexed by final candidate tree when passed.
- **Delivery coverage:** exact-tree exclusion of an already-reviewed landing
  from after-landing review; never a rewrite of pending debt.

Scope covers admission, the reviewer invocation, settlement, budgets,
managed completion under a gate, and coverage. It excludes granting merge
authority, changing task lifecycle, and reviewing the reviewer's own fixes
a second time.

## 3. At a Glance

| Concern | File | Task |
| --- | --- | --- |
| Shipped contract: gate, evidence rules, budgets, coverage, surfaces, rollback | [Design](./2_design.md) | [ORB-11333] |
| Verdicts, the two-commit shape, the findings comment, and revalidation | [Design §3.1](./2_design.md#31-verdicts-the-two-commit-shape-and-revalidation-orb-13989) | [ORB-13989] |
| Before-landing review of the open PR, and its claimed-leaf placement | [Design §3.2](./2_design.md#32-before-landing-review-of-the-open-pr-orb-14849) | [ORB-14849] |
| Operating a blocked review | [Review gate runbook](../../runbooks/review-gate.md) | [ORB-13989] |
| What a validation record establishes | [Design §4](./2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545) | [ORB-11528], [ORB-11545] |
| After-landing scheduling and coverage consumers | [Delivery automation operations](../automation-triggers/5_operations.md) | [ORB-11331] |
| `[operation]` key reference | [CONFIG.md](../../CONFIG.md) | [ORB-11333] |

## Task References

- [ORB-11333] — implements independent review policy, the before-PR gate, lineage budgets, and delivery coverage.
- [ORB-11528] — adds validation-record roles to the certificate contract.
- [ORB-11545] — tightens what a superseded validation record may claim.
- [ORB-11331] — owns the delivery automation consumers that spend certificates.
- [ORB-13989] — the reviewer fixes its findings as a second commit and comments them; retires the rework loop and the repair-cycle budget.
- [ORB-14849] — adds before-landing review of the open PR beside hosted CI.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
