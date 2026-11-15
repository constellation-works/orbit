---
title: Model Ranking — Design
owner: claude
last_updated: 2026-11-11
last_validated: 2026-11-11
status: Accepted
feature: model-ranking
doc_role: design
type: design
summary: "Design: per-model implementer ranking from six attributed measures, each compared with the same-mix fleet expectation and divided by a frozen noise scale, shrunk, bootstrapped into tie bands and ordered by score within a band, with cost used for exact score ties."
tags: [model-ranking, agent-scorecards]
paths: ["crates/orbit-core/src/application/agents/**", "crates/orbit-web/src/api/agents.rs", "crates/orbit-cli/src/command/agents.rs", "crates/orbit-types/src/workflow/final_recovery.rs", "crates/orbit-engine/src/executor/automation/vcs/landing/checks/**"]
related_features: [agent-scorecards, task-pilot, granted-tools]
related_artifacts: [ORB-23297, ORB-23066, ORB-23277, ORB-21789, ORB-17779]
---

# Model Ranking — Design

## 1. Scope

The ranking answers one question: **which model does the best implementation work on the
tasks Orbit gives it?** It ranks implementers only.

- Reviewers and pilots keep their existing [scorecard boards](../agent-scorecards/2_design.md).
  Models aren't given equal shares of those roles, so a model that only implements would
  otherwise lose points for work it was never given.
- Orchestrators aren't ranked.
- Interactive work (a desktop or terminal agent that sets the task's crew) has no job run,
  tool attribution or cost, so the default execution scope is `managed`, as on the
  scorecard. The ranking accepts the scorecard's `execution` selector.

## 2. Ranking unit and window

**Row.** A row is the `crew_model` of the final implementation run (scorecard §3.2), pooled
across every crew and effort level that ran it. The detail view shows the per-crew and
per-effort split. Tasks whose model is unknown are counted in the fleet expectation, not
ranked, and are reported as `unattributed`.

**Window.** A task belongs to the window by its charged attempt's start, as on the
scorecard. The ranked window is the 30 days ending at the **settled cutoff**:
`until − settle_lag`, where `settle_lag` defaults to 14 days. Escaped regressions arrive
late, mostly from delivery reviews within days of landing. A model whose work is all recent
would otherwise look cleaner than it will turn out to be. Every measure uses the same task
population, so a model's tier mix is the same for every term. A **live** view of the 30
days ending at `until` is available; it is labelled provisional and never frozen or
published. `until` uses the scorecard's [ORB-23066] support.

## 3. Measures

Each measure is a count of events attributed to the row, divided by the row's units. Only
problems the implementer caused are counted. The owner rules are the scorecard's §3.1;
Not-scored causes stay Not scored.

| Pillar | Measure | Weight | Units | Attributed event | Source |
|---|---|---|---|---|---|
| Quality | Escaped regressions | 30 | implemented tasks | `regression_from` links with escape `no_test` against the row's landed tasks, at severity weight, deduplicated against pre-landing review charges [ORB-21789] | exists |
| Quality | Review outcome | 20 | reviewed deliveries | `+1` first-pass accept, `−1` reject, `−0.5` accept-with-fixes, summed per delivery | exists |
| Reliability | Implementer CI failures | 10 | implemented tasks | required-check failures on the row's candidate heads classed `code` (§5.1) | **new** |
| Reliability | Implementer-fault final recoveries | 10 | implemented tasks | final-recovery decisions with `fault: implementer` (§5.2) | **new** |
| Reliability | Crew-caused rework | 5 | implemented tasks | scorecard's crew-caused rework, minus events already counted as CI failures or final recoveries (§3.1) | exists |
| Reliability | Tool-call errors | 5 | tool calls | Orbit tool calls in the row's implementing activities classed `expected` or `denied` (§5.3) | partly new |

Quality carries 50 of the 100 weight points and reliability 30. The other 20 are
deliberately unassigned: the [cost decision](4_decisions.md#score-orders-a-tie-band-cost-breaks-exact-score-ties)
keeps efficiency out of the score.

### 3.1 Counting each event once

One underlying failure can show up in several places. Each is charged in the first matching
row, in this order:

1. a reviewer verdict;
2. a CI failure on a candidate head;
3. a final recovery;
4. crew-caused rework.

A repair run that fixes its own CI failure charges the failed head once. A head that fails
again after the repair is a new failure. Escaped regressions keep the scorecard's
deduplication against pre-landing reviews.

### 3.2 Shown, not scored

The row detail lists the following, each compared with the same-mix expectation where that
applies:

- cost per task (list price, with coverage);
- tokens;
- the agent's own run time, excluding queue and CI waits;
- landed rate;
- per-line rates;
- the effort mix;
- the execution split;
- unattributed and Not-scored counts.

None of these enter the score.

## 4. Score

### 4.1 Expected for the same work

For each measure `m` and row `r`, `expected(r, m)` is the scorecard's same-mix expectation
(§5.2), including its conservative tier borrowing:

- **Escaped regressions** use the size-adjusted expectation: tier and type set the cell,
  and the cell's fleet-median lines changed set the bucket, never the row's own lines.
- **The other measures** use the tier expectation.
- **Tool-call errors** use the fleet's error rate per call on the same tiers, weighted by
  the row's calls per tier.

The residual is per unit:

```
resid(r, m) = sign(m) × (actual(r, m) − expected(r, m)) / units(r, m)
```

`sign(m)` is `+1` for review outcome and `−1` for every problem measure, so a positive
residual is always better than expected.

### 4.2 Noise scale

Each measure has a frozen **noise scale** `scale(m)`. It is the standard deviation of
`resid` for a reference-sized model sample, estimated by resampling tasks from the fleet
within each tier: 1,000 draws of `n_ref` tasks with the fleet's tier mix, where `n_ref` is
the median eligible row's units over the calibration period.

- **Floor.** A scale is floored at the 10th percentile of its weekly estimates during
  calibration, so a measure that is almost always zero cannot dominate the score.
- **When it is frozen.** Scales are frozen per *season*, a period during which the scales
  stay fixed. A season starts at adoption, and again whenever a measure's definition,
  weight or attribution rule changes. Each frozen scale records its season, its
  calibration window and `n_ref`.

### 4.3 Composite, shrinkage and eligibility

```
z(r)     = Σ_m w(m) × resid(r, m) / scale(m)  ÷  Σ_m w(m)
z_s(r)   = (U(r) × z(r) + K × z_loo(r)) / (U(r) + K)        K = 30
score(r) = 100 + 10 × z_s(r)
```

- **Units.** `U(r)` is the row's implemented tasks.
- **Leave-one-out mean.** `z_loo(r)` is the units-weighted mean `z` of every *other*
  eligible model.
- **Two-sided shrinkage.** A model is pulled toward the others from either side, the
  harder the less evidence it has. Unlike the scorecard's one-sided smoothing, it also
  lifts a weak model. That is adopted only after the replay in §7 shows how it moves the
  result.
- **Unmeasured measures.** A measure with zero units for a row (no reviewed deliveries, no
  tool calls) contributes 0 and its weight drops out of that row's denominator. The detail
  shows it as unmeasured. Review outcome is also unmeasured for a model with fewer reviewed
  deliveries than `review_min_support` (30, the scorecard's value, returned in `rules`):
  its units, actual and expected still show, but it contributes nothing and its 20 weight
  points leave that model's denominator [ORB-23838]. The bootstrap fixes that status by
  the model's own reviewed count, a tier point applies it to that tier's reviewed count,
  and the calibration sample and the other models' expectations are unchanged.

A row is **eligible** with at least 30 implemented tasks and at least one assessed
tier with at least 5 of its tasks. A model eligible on exactly one assessed tier is
flagged **single-tier**, naming that tier (`single tier: hard`); the flag is a label,
not a change to its score. The board is **qualified** when at least 3 models
are eligible. An unqualified board shows scores without bands or ranks and is never frozen
as a result.

### 4.4 Uncertainty and tie bands

The **interval** is computed by resampling each eligible row's tasks with replacement
within its tiers, 1,000 times, keeping the expectations and scales fixed, and taking the
10th to 90th percentile of `score`. The same draws give the paired probability `P(a > b)`
for every pair of models.

**Bands** are formed top-down:

1. Sort by `score`.
2. A model joins the band of the model directly above it when `P(above > it) < 0.95`.
3. Otherwise it starts a new band.

Bands are displayed as shared ranks: `1=`, `1=`, `3`.

**Unequal measure coverage.** A model's score is comparable with another's only when both
were scored on the same measures. A model with no reviewed deliveries (or fewer than
`review_min_support`) has review outcome unmeasured, so its 20 weight points leave its
denominator and its score rests on the measures it does have. Its escaped-regression
expectation comes from whole-window fleet rates, so a model whose tasks sit in an era with
few recorded regressions reads as a large favourable residual on a thin base (ORB-24203:
gpt-6-sol, 117 tasks all before the first reviewed delivery, scored 115.8 on 35 of 55
measured weight points and held band 1 alone). Rows are therefore compared as follows:

- The **top band** draws only from eligible models at the board's best-covered measured
  weight (`weights_measured`, the largest among eligible rows). A model below that weight
  cannot hold band 1, whatever its score, and its row carries `top_band_ineligible_reason`
  naming the measures it was not scored on and its measured weight. It is still banded
  and ranked below, by the same `P(above > it)` rule, so the MVP is always a model scored
  on the full set.
- Lower bands take the remaining models in score order by the same rule; a partly
  measured model can share a band or rank below a fully measured one.
- Each row exposes `weights_measured` and the per-measure `status`, so a consumer can show
  the evidence behind a score.
- The denominator is unchanged: an unmeasured measure still leaves it (§4.3, ORB-23838).
  The gate is a rule about who may hold the top band, not a change to any score. The
  era bias in the escaped-regression expectation is not corrected here; the gate keeps
  such a score from deciding the top rank until the expectation is era-matched.

The 95% threshold is **adopted** (Daniel, 2026-11-10 · [ORB-23839]); R2's 80% rule
is superseded but kept legible here and in the decisions record. The [ORB-23304]
readout found band-membership changes in 14 of 96 one-at-a-time ±50% weight
perturbations at 80%, versus 6 of 96 at 95%, over eight retained-history rolling
cutoffs. Wider bands reduce sensitivity but do not establish robustness; the earliest
rolling cutoff is retention-limited. Eight additional complete, non-overlapping weekly
windows are included in the task artifact. Keep the proposed weights and evaluate
K = 30 in shadow; CI and recovery remain unmeasured, and the sparsely attributed tool
measure is exploratory. This adoption does not waive R1's coverage exit.

### 4.5 Order within a band

Within a band, models are listed by **score**, highest first. Exact score ties are ordered
by the row's list-price cost per task divided by the fleet's cost per task on the same tier
mix, cheapest first, then by model name. Cost is priced at provider list prices, so flat-rate
subscriptions don't read as free. Rows with less than 80% cost coverage have unknown cost;
on an exact score tie, priced rows come first and unknown-cost rows sort by model name.
Cost never moves a model across a band boundary.

### 4.6 Value chart

For each tier with at least 3 ranked models of at least 10 tasks, the view plots each
model's tier-specific score against its cost per task on that tier, one tier at a time
behind a tier switch. Models that no other model beats on both score and cost are marked
as the **frontier**; a model whose tier cost covers too few of its tasks is drawn hollow
and never joins it. This is the input for pool weights, which stay Daniel's. The chart is
not part of the ranking.

## 5. New data

### 5.1 CI failures by cause

Today Orbit reads required-check results at landing and matches declared infrastructure
signatures (`workflow.ci_infrastructure_signatures`), but it doesn't keep a per-head record
of failures with their cause.

Each required-check failure on a candidate head is recorded with:

- head SHA, task, the run that produced the head, check name;
- the error signature (`log_signature`);
- a **class**:
  - `infrastructure`: a declared infrastructure signature matched;
  - `flaky`: the same head passed the same check on a rerun with no new commit;
  - `code`: the same head failed the same check again on a rerun, or the failure signature
    names a file or test the head changed;
  - `unknown`: none of these could be established.

Only `code` is charged, to the run that produced the head. `unknown` is shown as coverage.
A failure on a head that the base branch also fails is classed `infrastructure`. Final
recovery's mechanical re-request of failed checks (once per head) and CI auto-fix reruns
already produce a second result for many failures. Where neither ran, R1 adds one rerun of
the failed checks per head before classing the failure.

### 5.2 Fault on final recoveries

`FinalRecoveryDecision` (`crates/orbit-types/src/workflow/final_recovery.rs`) has no owner
today. Each decision gains a required `fault`:

- `implementer`
- `specification`
- `review`
- `infrastructure`
- `orbit`
- `external`
- `unknown`

The fault comes with one line of evidence that cites a run, check or file.

- **What is charged.** Only `implementer` is charged.
- **Bias check.** The recovery agent can be the same family as the implementer it judges,
  so the implementer's model is left out of the recovery packet. Attributions are audited
  weekly on a sample of 10 decisions, which a different family reviews blind to the
  implementer's model.
- **Disagreement.** A disagreement rate above 20% in a season suspends the measure (weight
  0, shown) until the rubric is fixed.

As built [ORB-23301]: the fault and evidence sit beside the decision on the run checkpoint
and the task's typed history decision, and decisions recorded before R1 read `unknown`.
An agent answer without both is malformed and escalates as `unknown`. Orbit records the
provider Orbit observed for the judging agent as `fault_judged_by`, never from the agent's
answer; it is `null` when no provider was observed. The packet strips model, provider and crew keys from its
evidence and masks the run's crew and model names. The audit is the disabled-by-default
`recovery-fault-audit` auto-task. It samples through the blind `final_recovery` task field
and skips decisions judged by its own family. Until R2 records seasons, every decision
counts in `season-0`.

### 5.3 Tool-call errors by model

Tool audit events already carry a class (`FailureClass`):

- `denied`: policy denial;
- `expected`: an expected negative path, such as input validation or an invalid
  transition;
- `diagnostic`;
- `unexpected`;
- `deferred`.

The measure counts `expected` and `denied` on Orbit tools called inside the row's
implementing activities.

- `unexpected` failures are Orbit's own, `deferred` aren't failures, and `diagnostic`
  events are lifecycle noise. None of these are counted.
- Host-to-host tools such as `orbit.drain.claim.settle` are never in an implementing
  activity.
- Attribution to a model comes from the job run. [ORB-23277] adds a server-derived model to
  every MCP tool audit event, which also covers calls whose job-run link is missing.

Tool schema changes (ORB-23277–23280) move this measure for every model at once. Each such
change starts a new season (§4.2).

## 6. Surfaces

- **CLI:** `orbit agents rank [--window 30d] [--until <ts>] [--settle-lag 14d]
  [--execution managed|all] [--live] [--json]`.
- **API:** `GET /api/agents/rank`, with the same parameters. The payload carries the
  following, so every number on the page can be recomputed:
  - per row: score, interval, band, cost ratio, per-measure actual, expected, residual and
    contribution, units, eligibility and the effort split;
  - per board: the season's scales and weights, and whether the board is qualified.
- **Dashboard:** Analytics › Agents opens on a **Rank** tab ([ORB-23303]). Its summary strip reports
  **Ranked**, **Tasks scored**, **Not scored**, and **Measures live**. The refined rank table
  groups eligible models by rank and shows the score, p10–p90 interval, cost per task, task
  count, and landed rate; a row's chevron opens its per-measure breakdown. Ineligible models
  appear in a **Not ranked** disclosure. The value chart has a tier switch
  ([user interface §5c](../user-interface/2_design.md#5c-analytics--agents)).
- **Snapshots:** a weekly routine freezes the settled ranking, with its inputs, as a task
  artifact. The weekly MVP note and the public analytics site read the frozen snapshot,
  never a live query.

### 6.1 As built (R2)

- **Rows and expectations.** The ranking reads the implementer scorecard's per-task
  counts and expectations (tier rates with the conservative borrowing, and the
  size-adjusted escaped-regression expectation) and sums them per model, pooling every
  model-and-recorded-effort scorecard row across its crews and effort levels. A model's
  expectation on a shared term is therefore the sum of its model-and-effort scorecard rows. A task
  two rows of one model both worked counts once per row, as on the scorecard. The board stays pooled by model alone; each row's `effort_split` uses the same `(model, reasoning_effort)` encoding as the scorecards (a wire name, `null` for the provider default, `unknown` for unrecorded work). An implementing invocation with no dispatch identity counts under its run's crew-label effort when the scorecard resolved that run's attempt from a `<crew>@<effort>` label (agent-scorecards §2, Crew-label fallback) [ORB-24150].
- **Measures recorded today.** Escaped regressions, review outcome and crew-caused rework
  are measured. CI failures and tool-call errors report `unmeasured` while model attribution
  is unavailable. Final recoveries also report `unmeasured`: R1 records their fault, but the
  ranking does not yet compute that measure. Each unmeasured measure's weight leaves every
  denominator, and review outcome leaves a model's denominator while the model has fewer than
  30 reviewed deliveries. A measure whose calibrated scale is zero reports `no_scale` and leaves the
  denominator too.
- **Seasons.** `orbit agents rank calibrate [--window] [--until] [--settle-lag]
  [--execution]` calibrates the settled window and appends a season to
  `.orbit/state/agents/rank-seasons.json`: its number, the definition version, the
  calibration window, `n_ref`, the draws, and each measure's scale, unfloored estimate,
  floor and weekly estimates. The board uses the newest season of the current definition
  version; a definition change bumps the version, so older seasons stay on file unused.
  Before the first calibration the scales are estimated from the ranked window and the
  payload says `scales_source: "window_estimate"`.
- **Resampling** uses a fixed seed, so the same facts give the same scales, intervals and
  bands. The interval and paired probabilities resample eligible models only; the
  leave-one-out mean is recomputed in every draw.
- **Cost** is the scorecard's recorded run cost (the provider's cost, else the cost derived
  from list prices). Within a band, rows are ordered by descending score. On an exact score
  tie, priced models come first by cost ratio, then model name; unknown-cost models follow
  by model name.
- **Payload.** Per row: `score`, `raw_score`, `z`, `z_loo`, `z_shrunk`, `interval`
  (`p10`, `p90`), `rank` and `rank_label` (`2=`), `cost` (`ratio`, `per_task`,
  `expected_per_task`, `coverage`, `cost_unknown`), per-measure `actual`, `expected`,
  `residual`, `units`, `contribution` (score points before shrinkage) and `status`,
  eligibility, `single_tier` (the sole assessed tier's name for an eligible
  single-tier model, else `null`), `crews` and `effort_split`. Each row also carries `tiers`
  (tasks, an unsmoothed score, cost per task, coverage and `cost_unknown`),
  `lines` (landed lines changed) and `execution` (managed, interactive and
  unclassified tasks). Per board: `measures` with their scales, `season`,
  `scales_source`, `rules`, `qualified`, `unattributed`, `execution_split`,
  `not_scored` and `paired_probabilities`. A tier score is `100 + 10 × z`
  on that tier's tasks only, before the board's shrinkage; the page computes
  the frontier from those points. The weekly snapshot is later work.
- **Band threshold.** 95%, adopted 2026-11-10 (Daniel · [ORB-23839]), following
  the R3 shadow recommendation in §4.4; this supersedes R2's 80% rule.
- **Still unmeasured at HEAD.** CI failures, final recoveries and tool-call errors are not
  scored. [ORB-23304] (R3) changed no ranking code. Tool-call errors stay unmeasured although
  [ORB-23277] now attributes each tool audit row to a model; see [ORB-23914].

## 7. Validation and rollout

| Phase | Work | Exit |
|---|---|---|
| R1 | Record CI failures by cause (§5.1) and recovery fault (§5.2); [ORB-23277] lands. Built: [ORB-23300], [ORB-23301], [ORB-23277] | Each new record type covers at least 80% of the events in a 14-day sample. Not yet shown |
| R2 | Compute the ranking, CLI and API; calibrate season-0 scales. Built: [ORB-23302] | It reproduces the scorecard's expectations to within rounding on the shared terms |
| R3 | Replay 8–12 settled weeks, adding one component at a time: model pooling, then each new measure, then noise scaling, then two-sided shrinkage. Built: [ORB-23304] (readout only) | A readout of what moves the result; weights stable under ±50% perturbation, or the readout widens the bands |
| R4 | Run in shadow beside the scorecard and the MVP for 4 weeks, publishing both. No open task: [ORB-23305] (freeze code) was rejected 2026-11-09 | Daniel adopts or amends |
| R5 | The MVP note and the public site switch to the frozen ranking | — |

## 8. Out of scope

- **A model-judged code-quality grade.** The archived
  [delivery-quality proposal](../_archived/decision-assistance/specs/delivery-quality.md)
  is not used. See [decisions](4_decisions.md#no-judged-quality-or-churn-measures).
- **Code survival and churn.**
- **Cost, tokens or time as score terms.**
- **Reviewer, pilot and orchestrator quality**, which have their own boards.
- **Crew routing.** The ranking never changes pool weights.

## Task References

- [ORB-23297] — designs the model ranking (this document)
- [ORB-23300] — R1: records implementer-caused CI failures by cause
- [ORB-23303] — the dashboard Rank tab and per-tier value chart
- [ORB-23305] — R4 freeze and snapshot task, rejected 2026-11-09
- [ORB-23914] — proposed task to wire the tool-call errors measure (divergence from §5.3 and §6.1)
- [ORB-23066] — `--until` on the scorecards
- [ORB-23277] — server-derived model on MCP tool audit events
- [ORB-23301] — records fault and evidence on final-recovery decisions, with the weekly audit
- [ORB-23838] — requires minimum support for implementer review terms
- [ORB-21789] — size-adjusted escaped regressions for implementers
- [ORB-17779] — same-task-mix expectations on the implementer scorecard
- [ORB-23302] — R2: computes the ranking, `orbit agents rank` and `GET /api/agents/rank`
- [ORB-23304] — R3: season-0 candidate scales, component replay, coverage and weight sensitivity
- [ORB-23839] — adopts the 95% tie-band threshold

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
