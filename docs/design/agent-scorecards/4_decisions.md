---
title: Agent Scorecards — Decisions
owner: claude
last_updated: 2026-10-24
last_validated: 2026-10-24
status: Draft
feature: agent-scorecards
doc_role: decisions
type: design
summary: Standing rules for agent scorecards; one owner per problem, cost shown but never scored, reviewers scored on accuracy.
tags: [agent-scorecards, dashboard, analytics]
paths: ["crates/orbit-core/src/application/agents/**", "crates/orbit-core/src/application/codebase/pilot.rs"]
related_features: [user-interface, review-gate]
related_artifacts: []
---

# Agent Scorecards — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

## Each problem counts against exactly one role

**Recorded:** 2026-10 · agent-scorecards design (no task yet)

### Context

A defect usually involves several roles: the orchestrator wrote the spec, the implementer wrote the code, the reviewer accepted it and the pilot declared its footprint. The orchestrator scorecard charged orchestrators for crew-caused rework, so on real data most of an orchestrator's penalty measured its implementers. Once there is a scorecard for every role, charging a problem to every role involved makes the rankings move together and hides who should change.

### Decision

Every recorded problem has exactly one owning role, or none (**Not scored**). The owner is the role whose safeguard the record says failed: a rework cause names spec, crew or infrastructure; a regression's escape names the gate that missed it. Credit is separate from problems. A role that catches or fixes another role's problem may earn credit for it, but the problem still counts once, against its owner. Any new scorecard term must name its owner under this rule before it is added, and a problem with no recorded cause is shown as Not scored rather than assigned a guessed owner.

### Consequences

- The attribution strip's per-role totals equal the problems behind the four scorecards, and no problem appears in two.
- An implementer is not charged for a defect review should have seen; the reviewer is. Orchestrator spec regressions narrow to escape `spec_ambiguous`.
- Cost: a problem that really was shared is charged to one role only, so a weak implementer paired with a weak reviewer looks better than either deserves on its own tab. Reading roles together is required to see that.

## Cost is shown, never scored

**Recorded:** 2026-10 · agent-scorecards design (no task yet)

### Context

Operators choose crew pools by trading quality against spend. If cost is a term, one weight decides that trade for everyone, and a cheap crew's lower quality can be hidden inside a high score.

### Decision

Dollar cost, tokens and wall time never enter an agent score. They are shown next to it: under **Shown, not scored** for every role, and as a score-against-cost panel where the trade is the point. The same holds for any future resource measure: if it is something an operator trades against quality, it is shown, not weighted.

### Consequences

- A score answers "how good is the work", and the cost panel answers "at what price"; the frontier of crews nothing beats on both makes the trade visible.
- Cost: the ranking alone never recommends a crew. An operator has to read two views, and a reader who looks only at the ranking will prefer expensive crews.

## Score accuracy, never the volume of objections

**Recorded:** 2026-10 · agent-scorecards design (no task yet)

### Context

A reviewer can raise its catch count by rejecting more, or look efficient by accepting everything. Either way, counting verdicts rewards a posture instead of correct judgement.

### Decision

A gatekeeping role (reviewers today, and any future verifier) earns only for objections that held up and is charged for objections that did not and for problems it let through. Its accept rate, reject rate and finding count are never terms. They are shown, with peers on the same tiers as the comparison, so an outlier is visible without being scored.

### Consequences

- Rejecting more cannot raise a reviewer's score; only being right can.
- Reviewer scores depend on later evidence, so recent scores are provisional until the miss lookback passes.
- Cost: the review gate does not record whether its own findings were right, so "held up" is measured through proxies (disposed findings, false rejects) that misjudge some cases until a direct record exists.

## Compare implementers with the fleet on their task mix

**Recorded:** 2026-10-24 · [ORB-17779] removed implementer complexity weighting and centred every term on same-tier fleet expectations, extending [ORB-17118]'s unit-landed orchestrator principle.

**Code anchors:** `crates/orbit-core/src/application/agents/roles/implementers.rs::rows`, `crates/orbit-core/src/application/agents/score.rs::score`

### Context

Complexity pools decide which work a crew receives. Weighted landings ranked
pool membership, while unadjusted rejects charged crews for difficulty.
Fleet-wide rates were a real alternative, but would retain the task-mix bias
when tiers have different outcome rates.

### Decision

Implementer landings count one each. Compare every term's actual count with
the fleet's expected count on that row's tier mix, per 100 tasks, starting at
100. Pool tiers below 30 observations with the largest tier before computing
rates, rather than letting a handful of rare-tier tasks set their own rate.
Keep cost outside the score and keep the existing shrinkage and support rules.

### Consequences

- A row matching each reference pool's rates has raw 100 regardless of mix.
- Expected counts and actual counts remain inspectable in the existing detail.
- Cost: fleet-relative scores can move when peers change even if the crew's own
  outcomes do not. Pooling sparse tiers trades exact per-tier invariance for a
  supported reference; until those tiers grow, their crews inherit the larger
  pool's rate and atypical difficulty can still affect their comparison.

## Task References

ORB-17779 — replaced complexity-weighted implementer credit with unit landings and same-task-mix fleet expectations.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
