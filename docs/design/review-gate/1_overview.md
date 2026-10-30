---
title: Review Gate — Overview
owner: codex
last_updated: 2026-10-29
last_validated: 2026-10-28
status: Accepted
stage: shipped
next: "ORB-20099: staged before-PR deprecation (§1.1), then dependent ORB-20087: selective before-landing risk/path rules (§3.3); symbols deferred."
feature: review-gate
doc_role: overview
type: design
summary: Independent automatic review with captured timing and exact-tree coverage; proposed staged before-PR deprecation precedes selective before-landing risk/path rules and preserves existing runs and evidence.
tags: [review-gate, review-policy, automation, delivery]
paths: ["crates/orbit-config/src/operation.rs", "crates/orbit-core/src/application/review/**", "crates/orbit-store/src/driver/sqlite/review/**", "crates/orbit-automation/src/review/**", "crates/orbit-engine/src/executor/automation/vcs/review_gate.rs"]
related_features: [automation-triggers, activity-job, auditability]
related_artifacts: [ORB-11333, ORB-11528, ORB-11545, ORB-13989, ORB-13992, ORB-14849, ORB-20087, ORB-20099]
---

# Review Gate — Overview

**Shipped.** [ORB-11333] implements an automatic code review that is
independent of who implemented the work. Before-PR review admits a fresh,
separately configured reviewer that checks the implementation commit, fixes
what it finds as one second commit (`review: <summary>`) under its own
identity, and comments every finding with what changed for it [ORB-13989].
The verdict is `accept` (no fixes), `accept_with_fixes` (owner validation
reruns on the reviewer commit before the PR opens, and the PR body gains a
"Review fixes" section), `reject` (the task blocks with both commits
preserved; there is no second review round), or `incomplete` (the review
could not finish or required evidence is unavailable). An evidence-only gap
can hold the candidate for the owner to supply the named checks. Passed
certificates exclude exactly reproduced landings from redundant after-landing
review while QA stays independent.
Neither review timing nor a reviewer verdict grants merge permission.

**Proposed prerequisite, not shipped:** [Design §1.1](./2_design.md#11-proposed-prerequisite-deprecate-before-pr-review)
deprecates before-PR mode with warnings and explicit migration to
before-landing, preserving legacy inputs, captured runs, budgets and review
evidence. Implement it before selective review; removal of legacy handlers
is separate later work.

**Proposed extension, not shipped:** [Design §3.3](./2_design.md#33-proposed-selective-before-landing-review)
adds optional risk-threshold and repository-relative path rules that select
the existing before-landing reviewer, with observation before enforcement.
It includes proposed `config.toml` settings, exact-head receipts and
owner/follower enforcement. Symbol-history triggers remain deferred.

Review runs at three timings [ORB-13992] [ORB-14849]: `review.timing =
"before_pr"` in `config.toml` holds PR creation for the reviewer;
`review.timing = "before_landing"` opens the PR first and reviews it while
hosted CI runs, landing only the head that review settled; and the
`delivery-code-review` auto-task's own `enabled` flag reviews landed
deliveries in batches. Both config timings share `review.minutes`, the limit
for one candidate's review, and there is one review layer before landing:
`review.timing` names at most one. The reviewer is drawn from the single crew
or weighted pool in `workflow.purpose_crews.pre_landing_review`, or in
`workflow.purpose_crews.review` while that is unset; the `[operation]` table
once also carried operation-mode presets and grants, removed on 2026-09-21
(see [orbit-core decisions](../orbit-core/4_decisions.md)).

With its own `review.timing` not `before_landing`, an owner can use
`review.before_landing_hosts` to enable before-landing review for claimed
deliveries from listed machines while leaving its own deliveries and unlisted
hosts unaffected. The host selector cannot be combined with `review.timing =
"before_pr"`; there is one review layer before landing.

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

- **Before-PR timing:** `review.timing = "before_pr"`, captured once per
  delivery run or drain in its immutable input and never re-read.
- **Before-landing timing:** `review.timing = "before_landing"`, captured the
  same way; the reviewer reviews the published PR head, a fix is pushed under
  a lease on that head, and any outcome but an approve leaves the PR open in review.
- **After-landing switch:** the `delivery-code-review` auto-task's `enabled`
  flag; it never affects delivery admission.
- **Reviewer:** a fresh invocation with its own instruction, tool allowlist
  and wall clock, resolved from `workflow.purpose_crews.pre_landing_review`
  (else `workflow.purpose_crews.review`). It never
  becomes the implementer and never merges.
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
  from after-landing review, or a `rebased_clean` exclusion when the reviewed
  head merged cleanly onto a base that moved; never a rewrite of pending
  debt.

Scope covers admission, the reviewer invocation, settlement, budgets,
managed completion under a gate, and coverage. It excludes granting merge
authority, changing task lifecycle, and reviewing the reviewer's own fixes
a second time.

## 3. At a Glance

| Concern | File | Task |
| --- | --- | --- |
| Shipped contract: gate, evidence rules, budgets, coverage, surfaces, rollback | [Design](./2_design.md) | [ORB-11333] |
| Proposed before-PR deprecation, explicit migration and preservation contract | [Design §1.1](./2_design.md#11-proposed-prerequisite-deprecate-before-pr-review) | [ORB-20099] (proposed prerequisite) |
| Verdicts, the two-commit shape, the findings comment, and revalidation | [Design §3.1](./2_design.md#31-verdicts-the-two-commit-shape-and-revalidation-orb-13989) | [ORB-13989] |
| Before-landing review of the open PR, and its claimed-leaf placement | [Design §3.2](./2_design.md#32-before-landing-review-of-the-open-pr-orb-14849) | [ORB-14849] |
| Proposed selective risk/path rules, configuration and rollout; symbols deferred | [Design §3.3](./2_design.md#33-proposed-selective-before-landing-review) | [ORB-20087] (proposed) |
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
- [ORB-20087] — proposed configurable risk/path selection of before-landing review; symbol triggers deferred.
- [ORB-20099] — proposed staged before-PR deprecation preserving runs and evidence; [ORB-20087] depends on it.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
