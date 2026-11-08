---
title: Agent Scorecards — Decisions
owner: claude
last_updated: 2026-11-08
last_validated: 2026-11-08
status: Draft
feature: agent-scorecards
doc_role: decisions
type: design
summary: Standing rules for agent scorecards; one owner per problem, cost shown but never scored, reviewers scored on accuracy, escaped regressions scored against expected size.
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

- The attribution strip's per-role totals are ownership counts, not score contributions: each problem contributes once to exactly one owner total, and no problem appears in two, but rankings use only their role's scored terms. In particular, `no_test` escapes increase the Implementers strip count; since [ORB-21789] they are also scored, except a defect a pre-landing review already charged.
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

## Score escaped regressions against expected change size

**Recorded:** 2026-11-03 · [ORB-21789] revisited [ORB-19629], which left escaped regressions unscored until roughly 100 linked regressions with sites and another backtest.

**Code anchors:** `crates/orbit-core/src/application/agents/roles/implementers/performance.rs::TERMS`, `performance.rs::charged_by_pre_landing_review`, `crates/orbit-core/src/application/agents/roles/implementers/context.rs::append_diff_context`

### Context

`review.timing` is `off` in ws_orbit and `before_pr` is deprecated, so few
tasks carry a pre-landing review settlement: in the 7 days to 2026-11-03,
muse-wonder-full had 0 of 12 tasks reviewed, opus 3 of 90 and sol 19 of 161.
Delivery review covers every landed task and files each finding as a fix task
linked `regression_from` to its culprit. With escaped regressions unscored, the
review terms saw almost nothing and the score was effectively landings minus
rework: on hard work muse-wonder-full ranked third at 4.0 severity-weighted
regressions per task, 31.6 over expected on 12 tasks.

The rerun backtest read the ws_orbit owner store on 2026-11-03 at 06:55 UTC:
1,760 typed `regression_from` links, **662 with sites** (sites start
2026-10-28), 453 of those filed by `delivery-code-review`, and 543 against 306
culprits with a recorded landed diff size. Delivery review is prompt: from the
culprit's landing to the link's filing took a median of 0.2 hours, 7.4 at the
90th percentile and 31 at the 95th. The cohort is the **814** tasks that landed
with a recorded diff, crew and tier from 2026-10-28 02:14 to 2026-11-01 06:55
UTC, so each had 48 hours of follow-up, excluding `no-diff-expected` and empty
diffs. **192** of them had a linked regression with sites within 48 hours (359
links). The outcome is whether a task had any such link, fitted by logistic
regression with 5-fold cross-validation by task:

| Predictors | Cross-validated AUC |
|---|---:|
| ln(1 + lines changed) | 0.769 |
| Tier | 0.718 |
| Size and tier | 0.777 |
| Size, tier and crew | 0.763 |

Crew did not improve on size and tier: the likelihood-ratio χ² was 13.4 on 8 df
(p = 0.10), and it held out worse. On `no_test` links alone, the implementer-owned
escape, p was 0.08. Crew added no more beyond tier alone (p = 0.11) or beyond the
expected-size bucket the score uses (p = 0.11). A 24-hour follow-up (1,041
tasks, 236 regressed) gave the same picture (crew p = 0.08). This reproduces
the 2026-10-27 finding (AUC 0.737, no crew effect beyond size), whose scripts
were never recovered, on four times the linked sample.

### Decision

The implementer score takes delivery-review evidence as **Escaped regressions**,
a problem term weighted −0.5 per severity point: `no_test` regression links
against the row's landed tasks, at their severity weights. A `medium` finding
costs as much as a review reject. Its expected count is the size-adjusted one
the residual already displayed: the fleet rate for the task's tier and
expected-size bucket. Expected size is the fleet's median for the task's
(tier, type) cell, never the row's own diff. So routing larger work to a crew
does not lower its score, but writing a larger diff than comparable work does.

A defect is counted once across review layers. When a link's `sites` name a
path that a finding of the row's pre-landing `reject` (not a false reject) or
`accept_with_fixes` cited or changed, that verdict already charged the defect
and the link adds nothing. Other links on the same task still count, since they
are defects the pre-landing review did not report.

The alternative was a delivery-review review term: an accept credit for a task
with no findings, and the delivery verdict replacing the pre-landing one. It was
not chosen. Nothing in the scan records when delivery review has covered a task,
so a clean credit would be paid before the review ran. Escape attribution
already sends `review_missed`, `spec_ambiguous` and the unscored escapes to
their owners.

### Consequences

- A crew whose work no pre-landing review sees is now scored on its delivery-review findings, and a crew whose diffs run larger than comparable work pays for the regressions that come with them.
- `escaped_regressions` moves from the row's stats into its scored counts and expectations. The `regression_residual` stat is removed: the term's actual, expected count and contribution replace it. Diff economy stays shown, not scored.
- By-type cells score the term against their (tier, type) cell rates, so crew-performance draws now respond to escaped regressions.
- The `no_test` charge stays in the attribution strip's Implementers count even when a pre-landing verdict absorbed it.
- Cost: crew differences in regression rate beyond size were not significant (p ≈ 0.1), so much of the term's spread is noise; shrinkage toward the fleet average and the −0.5 weight limit it. A sparse expected-size bucket served only by the row's own work sets its own expectation and cannot penalize it. Site matching depends on filers naming sites: a link without sites always counts, even when it repeats a pre-landing finding. The filer's severity stands until verification confirms or replaces it, and 95% of delivery-review findings arrived within 31 hours of landing, so the newest tasks can still pick up findings after they are first scored.

## A scorecard window can end in the past

**Recorded:** 2026-11-08 · ORB-23066

### Context

Every scorecard window ended at the scan's now. `--window 7d` meant the last 7 days, and a count window (`last50` and the other lastN values) meant the newest N attempts in the scan. Reviewer scores stay provisional for 30 days, because a review's missed regressions can surface that long after the review. A window that ends now is therefore mostly unsettled. The weekly crew-rankings note needs reviewer scores on a window of the same length that ends 30 days back.

### Decision

`orbit agents scorecard`, `orbit agents show`, the matching MCP tools and `GET /api/agents/{role}/scorecard` and `/detail` take an optional `until`: an RFC 3339 instant, or a duration ago such as `30d` (units `m`/`min`/`minute`, `h`/`hour`, `d`/`day`, `w`/`week`). The window is then `[until - window, until)` for a calendar window, and the newest N attempts with `start < until` for a count window. A bare date is not accepted. A future instant is refused. Omitted, selection is unchanged: a calendar window still starts `window` before now and does not drop work at or after that instant. The JSON payload always reports the served `since` and `until` (RFC 3339 UTC). When `until` was omitted, `until` is the instant used as now. Reviewer provisional counts and the other lookbacks stay measured against now, so a window ending 30 or more days before now reports `provisional: 0` for reviewers.

### Consequences

- A consumer can ask for settled reviewer scores without changing the provisional rule or the default window.
- The human selection line does not gain an until label, so omitting the flag leaves that line unchanged. The JSON payload gains `until` on every response, including the deprecated orchestrator aliases, which do not grow their own flag.
- Cost: a caller that compared whole JSON bytes of an omitted window must ignore the new `until` field. Parsing accepts the resident-metrics duration grammar, including a trailing `s` and ignored whitespace, and rejects a bare calendar date rather than guessing one.

## Task References

- ORB-23066 — optional `until` window end on the agent scorecard CLI, MCP tools and API.

- ORB-17118 — the unit-landed orchestrator principle this decision extends.
- ORB-17779 — replaced complexity-weighted implementer credit with unit landings and same-task-mix fleet expectations.
- ORB-19629 — left escaped regressions unscored pending a larger linked sample and another backtest.
- ORB-21789 — reran the backtest on 662 links with sites and scored escaped regressions against expected change size, counting a defect once across review layers.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
