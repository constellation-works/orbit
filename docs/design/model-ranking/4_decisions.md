---
title: Model Ranking — Decisions
owner: claude
last_updated: 2026-11-09
last_validated: 2026-11-09
status: Draft
feature: model-ranking
doc_role: decisions
type: design
summary: "Why the ranking pools crews by model, compares against expected size rather than actual lines, uses tie bands, keeps cost out of the score, leaves reviewers and pilots on their boards, and uses no judged quality or churn."
tags: [model-ranking, agent-scorecards]
paths: ["crates/orbit-core/src/application/agents/**"]
related_features: [agent-scorecards]
related_artifacts: [ORB-23297]
---

# Model Ranking — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

## Rank models, pooling their crews

**Recorded:** 2026-11 · [ORB-23297]

### Context

The weekly MVP took each model's best crew. The best of several noisy rows is biased
upward, so a model with `opus` and `opus@medium` got two draws while a single-crew model
got one. It also rewarded the more expensive effort setting while cost went unscored.

### Decision

A ranking row is a model. All of a model's crews and effort levels are pooled into one set
of tasks, and the split is shown under the row but never ranked.

### Consequences

- Adding a crew or an effort level to a pool never improves a model's rank by itself.
- Cost: a model whose effort levels differ sharply in quality shows one blended score, so
  the effort comparison has to be read from the detail rather than the board.

## Compare against expected size, never actual lines

**Recorded:** 2026-11 · [ORB-23297]

### Context

Daniel's first list normalised measures per line: regressions, cost and time per line
changed. Dividing by the lines a model writes rewards padding. A model that writes twice
the lines for the same fix halves every per-line rate. The scorecard reached the same
conclusion for escaped regressions
([decision](../agent-scorecards/4_decisions.md#score-escaped-regressions-against-expected-change-size)).

### Decision

Every measure is compared with the fleet's expectation for the same tier, and escaped
regressions also with the same expected size. Expected size is the fleet's median lines
changed for the task's tier and type. Per-line rates are shown, never scored.

### Consequences

- Routing larger or harder work to a model doesn't lower its score.
- Cost: a model that solves a task in genuinely fewer lines gets no credit for it in the
  score.

## Bands instead of strict ranks

**Recorded:** 2026-11 · [ORB-23297]

### Context

The MVP's podium points paid the same for a 14-point lead as for a 0.1-point lead. The
ORB-21789 backtest found that crew explains little beyond task size, so many orderings are
inside the noise.

### Decision

Ranks are bands. A model shares its band with the one above it unless the paired bootstrap
gives the higher model at least an 80% chance of really being ahead. The score and its
interval are always shown.

**R3 shadow recommendation (2026-11 · [ORB-23304]):** evaluate a wider 95% threshold.
Across eight retained-history rolling cutoffs, ±50% weight perturbations changed band
membership in 14/96 cases at 80%, versus 6/96 at 95%. This remains sensitive, and CI-cause,
recovery-fault and tool-attribution coverage is incomplete. R2's implemented threshold
remains 80%; 95% is a shadow target pending further evidence and Daniel's adoption
decision. The task's readout also includes eight complete weekly windows and recommends
evaluating two-sided K = 30 shrinkage in shadow. Cost: wider bands yield more co-leaders
and less separation without guaranteeing weight stability.

### Consequences

- A week with no clear leader says so, rather than crowning a winner on noise.
- Cost: a shared first place is a less satisfying headline, and the MVP note must handle
  co-leaders.

## Cost orders a tie band and is never scored

**Recorded:** 2026-11 · [ORB-23297]

### Context

The scorecard records
[Cost is shown, never scored](../agent-scorecards/4_decisions.md#cost-is-shown-never-scored):
cost is a trade-off that pool weights make, and one weight for it would hide a cheap model's
lower quality inside a high score. Daniel wanted cost to count, because on the hard tier
Grok took about 3× the cost and time of other models. He also judged that discounting
performance by cost would be unfair, and that all else being equal the cheaper model should
be preferred.

### Decision

Cost, tokens and time are never score terms. Within a tie band, models are listed cheapest
first by tier-adjusted cost per task (list price). Cost never moves a model across a band
boundary. A per-tier score-against-cost chart marks the models no other model beats on both
score and cost. That chart, not the ranking, is the input for pool weights. This amends the
scorecard decision for the ranking only. The scorecard boards are unchanged.

### Consequences

- The part of a model's extra cost that comes from thrashing (retries, rework, recoveries,
  tool errors) is already scored under reliability. Only the price of equally good work is
  left to the tie-break and the chart.
- A model that is better but more expensive still ranks above a cheaper, weaker one.
- Cost: cost only matters within a band, so a model that costs 3× as much and is barely
  better still leads. Acting on that is a routing decision.

## Reviewers and pilots stay on their own boards

**Recorded:** 2026-11 · [ORB-23297]

### Context

The MVP added reviewer and pilot points to implementer points. Models aren't given equal
shares of those roles, so a model that only implements lost points for work it was never
given. The scorecard already has reviewer and pilot boards.

### Decision

The ranking covers implementation only. Reviewer and pilot quality stay on their existing
scorecard boards, with no new board.

### Consequences

- Cost: a model that is a strong reviewer but an average implementer doesn't show that
  strength on the ranking.

## No judged quality or churn measures

**Recorded:** 2026-11 · [ORB-23297]

### Context

Two further measures were considered:

- **A model-judged code-quality grade.** It came from the archived decision-assistance
  [delivery-quality proposal](../_archived/decision-assistance/specs/delivery-quality.md).
  That grader was never evaluated, and the two related judges failed their preregistered
  screens.
- **Code survival:** the share of lines still unchanged after 30 days. Daniel rejected it.
  Churn here clusters in hotspots, and much of it is small improvement work, so survival
  would mostly measure which files a model happened to touch.

### Decision

Neither is part of the ranking.

### Consequences

- Maintainability that passes review and causes no regression is invisible to the ranking.
- Cost: a working but bloated or poorly structured change scores the same as a clean one.

## One settled window for every measure

**Recorded:** 2026-11 · [ORB-23297]

### Context

Escaped regressions are mostly filed by delivery reviews within days of landing, so a
window ending now flatters recent work. Settling only the regression term would rank each
model on two different task populations.

### Decision

Every measure uses the same 30 days of work, ending `settle_lag` (default 14 days) before
`until`. A live view of the most recent 30 days is labelled provisional, and it is never
frozen or published.

### Consequences

- The published ranking reflects work that is two to six weeks old.
- Cost: a model's recent improvement or decline shows up in the ranking two weeks late.

## One assessed tier is enough to be ranked

**Recorded:** 2026-11 · [ORB-23672]

### Context

The eligibility gate required at least two assessed tiers with at least 5 tasks
each. A model the operator routes to one tier on purpose could never be ranked,
however many tasks it did, and hard-only crews dropped off as older off-tier
tasks aged out of the window. The only workaround was putting crews into a
second tier just to qualify, which turned pool weights into a rank-qualification
lever. Daniel (2026-11-09): "I also lean towards rule change. certain crews like
muse-spark and gemini-flash truly belong in one tier only".

### Decision

A model is eligible with at least 30 implemented tasks and at least one assessed
tier with at least 5 tasks. An eligible model spanning exactly one assessed tier
is flagged single-tier, naming the tier (for example `single tier: hard`), in the
rank row, the CLI text and JSON output, and the dashboard Rank table. Scoring,
shrinkage, the within-tier bootstrap, bands and cost ordering are unchanged.

### Consequences

- Single-tier models are ranked on the tier they actually work, and pool weights
  are no longer a qualification lever.
- Cost: a single-tier score is evidence about that tier only; the flag keeps
  that visible rather than implying breadth the model has not shown.

## Task References

- [ORB-23297] — designs the model ranking (this folder)
- [ORB-23672] — admits single-tier models and flags them
- [ORB-23304] — R3 calibration and replay readout; recommends wider bands for shadow evaluation

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
