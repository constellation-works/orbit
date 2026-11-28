---
title: Backlog Forecast — Overview
owner: claude
created_date: 2026-10-25
updated_date: 2026-11-28
last_validated: 2026-11-25
status: Accepted
stage: building
next: "Residual rules (review phases, shadow resource-fit, follower capacity, richer calibration) are not filed"
feature: backlog-forecast
doc_role: overview
type: design
summary: "Simulate real drain admission, review modes, retry outcomes, host resource fit and regression rework; expose task riskiness, score each forecast model version separately, and correct per-metric quantiles from scored residuals"
tags: [backlog-forecast, backlog-forecast-v2, analytics-metrics]
---

# Backlog Forecast — Overview

Status: v1 shipped in [ORB-18385]; v2 scoped by [ORB-18685], with the implementation tasks listed in [2_design.md](./2_design.md#tasks). The per-task riskiness estimator shipped in [ORB-18690], [ORB-18686] shipped the admission factor, [ORB-18687] shipped review-mode sampling, [ORB-18688] shipped attempt outcomes and blocked returns, [ORB-18689] shipped enforce-mode resource fit as `MODEL_VERSION` **6**, [ORB-18853] keeps sub-second and same-instant blocked returns drawable as `MODEL_VERSION` **7**, [ORB-18856] counts censored blocked waits in return-wait totals as `MODEL_VERSION` **8**, [ORB-18691] ships one generation of regression rework and donor-based arrivals as `MODEL_VERSION` **9**, and [ORB-18859] stores retry-inclusive per-task work and spend as `MODEL_VERSION` **10** (see [Shipped so far](./2_design.md#shipped-so-far)). [ORB-20673] shifts each attempt by a residual correction as `MODEL_VERSION` **15**, [ORB-20728] counts one residual per distinct task as `MODEL_VERSION` **16**, [ORB-20729] applies that correction to the full-script total as `MODEL_VERSION` **17**, [ORB-21949] fits it and scores task-level calibration on one forecast per task, withholding a cohort whose held-out p90 coverage drops, as `MODEL_VERSION` **18**, and [ORB-25278] removes its cohort-center spread stretch and adds a held-out p50 check as `MODEL_VERSION` **20**, and [ORB-25806] recovers in-origin defect records and reports adjusted totals over a bounded share of fix-history gaps as `MODEL_VERSION` **21**. Model **22** adjusts the median and upper tail using each task's own uncorrected quantiles, [ORB-26752] corrects the finite-sample upper rank under equal weights as `MODEL_VERSION` **23**, and [ORB-28343] falls back along the return-basis chain for an unsupported return tail and marks un-rescuable blocked tasks unforecastable (`return_tail_unsupported`) as `MODEL_VERSION` **24**. The explicitly listed residual rules (review phases, shadow resource-fit, follower capacity, richer calibration) remain planned. “v2” names the feature revision; implementations increment `MODEL_VERSION` from the value they find.

Bearing: Orbit records managed invocations, task status history, leaf occupancy, admission decisions and host resource profiles. The shared service is `crates/orbit-core/src/unit/backlog_forecast/`, with sampling in `backlog_forecast/model.rs`, scenario aggregation in `backlog_forecast/simulation.rs`, synthetic bundles in `backlog_forecast/rework.rs`, and scoring in `backlog_forecast/scoring.rs`. It serves the owner sweep clock and `crates/orbit-web/src/api/diagnostics/backlog_forecast/`. The drain already owns the admission rules this forecast must reuse ([auto-concurrency](../auto-concurrency/2_design.md), [distributed drain](../distributed-drain/1_overview.md)).

## Problem

V1 draws a completed delivery per task and list-schedules independent work on N slots. A dependency chain can run side by side, overlapping lock surfaces consume separate slots, and resource-heavy work always fills the ceiling. Its priority/age sort differs from the drain's order. Mixing reviewed and unreviewed deliveries, omitting failed attempts and ignoring regression fixes further understates time and spend. An operator cannot tell whether 40 tasks drain overnight or over a week, or why a forecast missed.

## Goal

A read-only, admission-faithful forecast: p50/p90 elapsed time to drain, USD and normalized tokens, overall and by complexity, with completion probabilities, coverage and explanations of constrained parallelism. A reusable per-task riskiness estimate is also shown on task rows and task detail. Store canonical forecasts hourly and score their versions separately. Forecasts never dispatch, reserve real locks, change task state or change admission policy.

[ORB-18691] (`MODEL_VERSION` 9, `inputs.admission_factors.regression_rework: 1`) adds regression rework:

- **Risk.** One shared `task_riskiness_model` scores the initial cohort. Its probability is consumed unchanged. A missing estimate uses a shrunk, supported joint crew × complexity prior, else the supported workspace base rate (both at the configured risk minimum support). `risk_prior_fallbacks` counts those tasks; `risk_unestimated` counts missing supported priors. The original `riskiness` object stays null where the task API has no estimate. Predictions and stored tasks include it, `regression_probability` and `regression_probability_basis` (`estimate`, `crew_complexity`, `workspace`, `unsupported`), plus bundle sample count and basis.
- **Bundles.** Completed fixes in the delivery lookback with invocation evidence and usable footprints supply distinct historical donors. Bundles group their `regression_from` links by original task, preserving multiplicity and non-negative original-done-to-filing delays. Origins may precede the delivery lookback. Matching origin complexity/crew bundles are preferred at five observations, else the workspace bundle pool; a nonempty pool below five remains drawable and is counted as thin. No ordinary delivery substitutes for missing fix history. A measured fix without usable invocation/footprint or origin-done evidence makes adjusted totals unavailable instead of silently shrinking its bundle; from model 14 that applies only to the pools the fix's origins join, after recovery from durable history and with non-sample links excluded (see [Shipped so far](./2_design.md#shipped-so-far)). Known initial fixes suppress their observed copies; shared multi-culprit donors count once per scenario.
- **Admission.** After an initial task finishes, a Bernoulli draw selects a bundle with its risk probability. Fixes enter backlog after their detection delay, using donor priority/tags, latent locks and attempt scripts through the same admission and gap-fit pass. Conflict rows are frozen once and remapped for generated instances; instances of the same editing donor conflict. Future scheduled fixes keep an otherwise idle queue open. Detections beyond 90 days are counted and excluded from bounded spend. Only initial tasks generate new regression work.
- **Response.** Both scenarios add `regression_rework` with `available`, `expected_followups`, follow-up count/work-hour/token/USD quantiles, `generation_limit: 1`, expected `detection_outside_horizon` and history support. Initial complexity rows exclude synthetic work; `arrival_work` separates exogenous work. Missing fix metrics propagate null. Unsupported priors or fix evidence null the adjusted scenario totals, probabilities and `schedulable_subset` quantiles (time, tokens and USD) and retain an explicit `pre_regression_baseline`, whose `schedulable_subset` still carries those subset quantiles. The adjusted scenario never keeps a subset quantile drawn without the regression follow-up it claims to include. The dashboard labels a subset figure taken from that baseline as the pre-regression baseline, which excludes regression follow-up work. From model 14 the fix-evidence part is per sampled pool, with reasons. Coverage adds prior/missing-risk and missing/thin-regression-history counts.
- **Arrivals.** Gross seven-day filing rates remain. `adjusted_per_day` excludes `regression_from` filings (`excluded_regression_filings`). Arrivals sample a completed historical footprint donor by source class at five samples, else the workspace donor pool, including observed no-diff work; its measured delivery and resolved outcome pool supply the attempt script. Missing donor support makes the arrivals estimate unavailable. `arrivals.coverage` retains its delivery/metric counts and adds donor and class-fallback support. `arrival_conflicts` is `historical_footprint_donors`; `arrival_resources` is `donor_profile`.
- **Storage/scoring.** Model-9 rework-inclusive totals stay `provisional` until their 90-day observation cutoff. Reads combine the newest 60 snapshots with up to 60 mature model-9-or-later snapshots per version. Actual first-generation fixes are matched by `regression_from` to the original cohort, including filings after it first emptied, deduplicated and charged only if absent from that cohort. Spend is bounded at the horizon; a crossing invocation has unknown tokens/USD. Each version reports aggregate rework count/hour/token/USD hit rates. Old versions retain their scoring semantics.

**Model-9 latency evidence.** On dk-server-2, a debug build, 200 initial tasks (10% unknown footprints, 10% directory selectors, every fifth depending on an earlier task, mixed medium/high priorities), 40 measured deliveries, 5 slots and 1000 iterations, five serial HTTP reads measured p50 **3.83 s**, p95 **4.02 s**. Preparation ranged **43–83 ms**, simulation **3.50–3.86 s**, and peak server RSS rose from **54.5 to 112.4 MiB** (+57.9 MiB). This debug measurement exceeds the 1-second target; release performance and larger backlogs are unmeasured. `timing.preparation_ms` and `timing.simulation_ms` expose candidate timings. The task's `forecast-latency.json` artifact records the samples; the temporary benchmark fixture is not shipped.

[ORB-18859] (`MODEL_VERSION` 10; factor revisions unchanged) stores per-task work and spend that include retries:

- **Predictions.** Each `task_predictions` row adds `work_hours`, `tokens` and `usd`: p50/p90 of what the task's attempts started by the horizon used, failed, blocked and abandoned ones included, exactly as the scenario totals and `by_complexity` charge them. `work_hours` is slot work (setback holds plus delivery hours), never elapsed or return-wait time. An unscheduled task has null quantiles.
- **Storage.** A stored task's `forecast` is those quantiles under `work_hours`, `tokens` and `usd`, replacing models 1–9's single-delivery `hours`, `tokens` and `usd` draw. With one task and no arrivals they equal the cohort's initial-work totals.
- **Scoring.** From model 10 a done task's actual tokens and USD are every invocation after the forecast. Its actual work hours are the hold of each attempt that settled without delivering after the forecast, from admission or the forecast, plus the delivery's invocation hours since the last such settlement; without readable status history, every post-forecast invocation counts as the delivery. Older snapshots are still read and scored under their stored `hours` measure. `calibration.task_hours_measure` and each `by_model_version` row name the measure (`attempt_work_hours` or `delivery_hours`); `task_level.hours` keeps its key.

- **Residual series.** Each `calibration.forecasts[]` row carries `residual`: per metric (`hours`, `tokens`, `usd`) the `median` and count `n` of its scored tasks' `ln(actual / p50)`, and `time_to_drain_hours`, the same log ratio for a forecast scored whole. A value is absent where the actual or p50 is not positive. It reads the actuals and quantiles the hit rates use, so it adds no scoring pass.

Remaining feature-level limits after model 10: review residual/support phases, run-lineage rather than status-history episode evidence, provider holds, retained review locks, completion Brier scores, dependency/footprint rescope exclusions, execution-host/factor calibration partitions and revision-keyed facts caches are still unshipped. Donor history is the measured completed-delivery subset, not every filing. Generated-node materialization is bounded at 10,000 per iteration and uses precomputed overlap rows; dense arrival streams can cost more than the default no-arrivals scenario. Limits null affected time/spend draws and increment `simulation_truncated`.

## Non-goals

- Dispatching, real reservations, policy changes or automatically changing N based on a forecast.
- A pooled multi-host cluster simulator or new follower telemetry transport. One execution host is modelled with its own evidence; unavailable remote capacity is explicit.
- A separate shared reviewer/CI service-capacity queue. Review modes, observed waits and return delays are modelled, but future global CI/provider incidents and human decisions are not independently simulated.
- Recursive synthetic regression generations, causal crew comparisons or risk as an admission veto.

## Acceptance (feature level)

- The public forecast for A→B→C cannot finish earlier than its sampled dependency path. Conflicting editing tasks cannot overlap; real unknown-footprint work obeys pilot/exclusive/tree-reservation rules; no-diff work holds no locks. Reservation and dispatch results agree with real admission for identical frozen inputs.
- Changing review policy selects the corresponding observed mode history or reports its fallback. Raised block/fail history adds waits/incompletion, releases slots at episode end, and retains failed spend. Heavy profiles reduce parallelism in enforce mode at fixed N, with idle-host/head protection intact.
- Task list/detail APIs and UI show the same size-based risk estimate, its declared-files or diff-lines basis, size-band evidence, partial support and no-estimate reasons; a row without an estimate has no risk part, and only an estimated task shows the detail breakdown. High risk adds sampled, separately reported regression work/cost. Synthetic arrivals use measured conflict donors, not whole-tree guesses.
- Responses/storage expose the fields and model/factor revisions above, with old rows readable. Calibration keeps versions/measures apart and records non-drain outcomes and missing evidence. Optional forecasts and task riskiness do not delay drain controls or refreshes.
- Each implementation validates its observed behavior through public API/CLI boundaries and records the 200-task/1000-iteration latency where scheduling changes. UI changes attach dark-theme desktop/375px screenshots and update the affected interface docs.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## Task References

- [ORB-18385] — shipped the original forecast, hourly storage and scoring.
- [ORB-18685] — scopes v2 before implementation.
- [ORB-18686] — implements dependencies, locks, dispatch and schedule diagnostics.
- [ORB-18687] — implements review-mode sampling.
- [ORB-18688] — implements blocked/failed/retry outcomes.
- [ORB-18689] — implements resource-fit concurrency.
- [ORB-18853] — keeps sub-second and same-instant blocked returns drawable (model 7).
- [ORB-18856] — counts censored blocked waits in return-wait totals (model 8).
- [ORB-18690] — shipped task riskiness APIs and UI.
- [ORB-18691] — implements forecast regression rework.
- [ORB-18859] — stores retry-inclusive per-task work and spend (model 10).
- [ORB-20673] — corrects bias and quantiles from scored per-task residuals (model 15).
- [ORB-20728] — counts residual-correction samples by distinct task (model 16).
- [ORB-20729] — applies the residual correction to the full-script total (model 17).
- [ORB-21949] — fits and scores task-level calibration on the first not-started forecast per task (model 18).
- [ORB-25278] — replaces the residual correction's cohort-centre stretch with a pure p50 shift, checked on held-out tasks (model 20).
- [ORB-25806] — recovers in-origin defect records and bounds fix-history gaps so the adjusted forecast can be reported and scored (model 21).
- [ORB-28343] — falls back along the return-basis chain for a blocked task's unsupported return tail, marks un-rescuable tails `return_tail_unsupported` with `schedulable_subset`, and names unsupported return tails instead of horizon censoring in the dashboard (model 24).

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
