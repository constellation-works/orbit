---
title: "Scope: backlog forecast from delivery history"
owner: claude
last_updated: 2026-10-26
last_validated: 2026-10-26
status: Draft
stage: shipped
next: "—"
feature: backlog-forecast
doc_role: scope
type: design
summary: "Scope: forecast backlog time to drain, USD and normalized tokens as p50/p90 by resampling completed deliveries, store the forecast hourly and score it against actuals"
tags: [backlog-forecast, analytics-metrics]
---

# Scope: backlog forecast from delivery history

Status: phase 1 shipped [ORB-18385].
Bearing: Orbit already records, for every managed invocation, its wall duration, tokens by kind, provider cost and the tasks it served ([auditability](../auditability/2_design.md)); normalizes tokens across models as `GET /api/cost` does; knows each task's complexity, crew and status history; and admits leaf runs under a drain ceiling (`max_active_leaf_runs`, adjustable live through run control, [auto-concurrency](../auto-concurrency/2_design.md)). Analytics shows what delivery cost in the past (Cost, Metrics › Tokens per task, Delivery friction); nothing says what the open backlog will take.

## Problem

An operator looking at 40 backlog tasks cannot say whether they drain overnight or over a week, or whether they cost $50 or $500. Per-complexity averages mislead: delivery time and cost are long-tailed (one recovery loop or rework doubles a task), so a mean is neither likely nor safe, and the sum of means hides that the backlog runs on a few parallel slots. The history to answer it exists, but nothing resamples it, and nothing checks whether any estimate was right.

## Goal

A read-only forecast for the selected workspace's dispatchable backlog: time to drain, USD and normalized tokens, each as p50 and p90, overall and by complexity, with counts of the tasks it could not forecast well. Forecasts are stored and later scored against what their tasks actually did, so calibration is observed rather than assumed. Nothing about dispatch, admission or task state changes.

## Shape

### Sampling unit

One **completed delivery**: a task that reached `done` inside the lookback (default 30 days, 7–180 by `lookback_days`), together with every managed invocation linked to it, at any time. Recoveries, auto-fixes, reviews and reworks are part of the delivery, so their cost is in the sample. A done task with no linked invocation (hand-closed, or delivered before invocations were linked) is not a sample; the response counts it as `history_without_invocations`.

Completion is dated by the last event in the task's durable history whose `to_status` is `done`, including creation in that status. The generated index's `updated_at` only narrows candidate reads; later comments and artifacts cannot move a completion into the lookback. A done task without a recorded done event is omitted from sampling and the seven-day done rate rather than dated by its last metadata write. Each request reads the event log once for each indexed done candidate since the earliest of the lookback, the seven-day rate window and the oldest stored forecast; the maximum lookback is 180 days.

For each delivery:

```
start_i  = ts_i - duration_ms_i        (an invocation's ts is recorded when it ends)
hours    = |union of [start_i, ts_i]|, bridging gaps < 2 h between consecutive intervals
tokens   = Σ normalized tokens_i       (null when any invocation's model has no price row)
usd      = Σ provider_cost_usd_i       (null when any invocation lacks a provider cost)
```

Bridging short gaps counts CI, review and gate waits inside one attempt as delivery time; a longer gap is the task waiting in the backlog between attempts and is not counted. Hours are wall time a slot is busy with the task, not CPU time.

### Cells and fallback

Each forecast task samples from the deliveries of its **cell**: its complexity bucket × its crew pool (the task's crew, else the crews the [automatic crew pools](../../CONFIG.md#automatic-crew-pools-by-complexity) route its complexity to). Below `MIN_CELL_SAMPLES` (5) deliveries the task falls back to its complexity's whole history, and when it is unassessed, or its complexity has no deliveries, to every delivery in the lookback. With no history at all it has no forecast. Each fallback is counted:

| Count | Meaning |
|---|---|
| `unassessed` | No complexity; sampled from every delivery. |
| `thin_cell_fallbacks` | Assessed, but its cell had fewer than 5 deliveries (`fallback_complexity` + assessed `fallback_workspace`). |
| `uncovered_model` | No sampled delivery has every invocation priced for normalization, so the task has no token forecast. |
| `uncosted` | No sampled delivery has every invocation's provider cost, so the task has no USD forecast. |
| `no_history` | Nothing to sample. |

For initial backlog and proposed tasks, a delivery is drawn whole; a missing token or USD value is redrawn from deliveries of the resolved pool that recorded that metric. Hours use all deliveries. A total is null in an iteration when any task in it has no draw for that metric, so a quantile is never a silent undercount.

### Simulation

The forecast set is the workspace's `backlog` and `in-progress` tasks. `review`, `blocked` and `someday` are left out (review is mostly done; blocked and someday are not dispatchable). `proposed` tasks are simulated separately and reported as `if_approved`.

Each of 1000 iterations (seeded, so a refresh does not jitter) draws one delivery per task and list-schedules the draws on `N` slots: in-progress tasks first, each with its draw minus the hours, tokens and USD its invocations have already used (floored at zero); then the backlog by priority and creation time; then, for `if_approved` only, proposed tasks. Time to drain is the makespan; tokens and USD are sums. The response reports nearest-rank p50 and p90 of each, and per complexity the p50/p90 of summed work hours, tokens and USD. No point estimate is reported anywhere.

**Capacity.** `N` is the leaf-run ceiling the drain admits under: the live local drain's effective `max_active_leaf_runs` (a run-control adjustment, else its submitted value), else the live pull drain's, else the job default (5), the same value `GET /api/workflows/auto/readiness` reports without a requested concurrency. `concurrency` (1–64) overrides it for a what-if. `inputs.concurrency_source` and `inputs.auto_concurrency_mode` say which applied.

**Arrivals.** The default assumes **no new arrivals**: the backlog only drains. The response always reports the arrival rate over the last 7 days, split into auto-task filings (`auto-task:` tags), sweep filings (CI-failure, QA, code-review, security-review, delivery-review, friction-curation and baseline-red-repair tags) and other filings, beside the done rate over the same days. `arrivals=on` adds a Poisson stream of tasks at the measured rate, each drawing from every delivery (preserving its token and USD values, including nulls), and the drain ends when the queue first empties; a drain that has not emptied within 90 days counts as beyond the horizon, so its quantile can be null.

A sampled arrival with missing tokens or USD makes that iteration's **total** null for that metric, for both `forecast` and `if_approved`; it is never counted as free or imputed. Duration and the other metric remain independent. As with other missing iteration values, null totals sort after numeric totals for nearest-rank quantiles: a rank that lands on null is null, rather than a quantile computed after discarding incomplete iterations. Fully covered arrival histories retain numeric cost and token quantiles. Per-complexity rows describe initial tasks only and exclude arrivals.

`coverage` and `proposed_coverage` count initial tasks only. `arrivals.coverage` describes the shared arrival sampling pool, even with arrivals off: `deliveries` is its size, `missing_tokens` counts deliveries without normalized tokens and `missing_usd` counts deliveries without complete provider cost. These are history-delivery counts, not counts of simulated arrivals. `assumptions.arrival_missing_metrics = "propagate_null"` identifies this policy. The panel shows the gaps and explains unavailable totals; with arrivals off these gaps do not affect initial-task draws.

### Storage

The canonical forecast (default lookback, the live ceiling, no arrivals) is persisted in `backlog_forecasts` (store migration v62, additive): `workspace_id`, `forecast_at`, `model_version`, the inputs, the totals and, per task, its id, complexity, basis and its own p50/p90 of hours, tokens and USD (net of what it had spent). The owner host’s sweep clock attempts a canonical write on each non-dry-run tick, and a canonical endpoint read may also write when the slot is free. Both triggers share the same inputs and immediate transaction: the recent-row check and insert allow at most one row per workspace per 60 minutes, including when a read and a tick race. The clock checks the newest row before reading delivery history or simulating, so ticks within the hour skip that work. Follower replicas never write on the scheduled path. Non-canonical reads, an empty dispatchable backlog or no delivery history never write. A failed write is logged and fails neither the tick nor the read.

### Scoring

Every request scores the newest 60 stored forecasts against the current task records and invocations:

- **Per task.** A stored task now `done` is scored on what it used after `forecast_at` (the same hours, tokens and USD measures as a delivery, cut at the forecast time) against its stored p50 and p90.
- **Per forecast.** Once none of its tasks is pending, the forecast is *settled*; actual time to drain is the last task's durable done-event time minus `forecast_at`, and actual tokens and USD are the sums. For an older task without a recorded done event, scoring falls back to its last linked invocation's end, never its mutable `updated_at`. It is scored as a whole only when nothing was excluded.
- **Exclusions.** Tasks since `rejected` or `archived`, tasks no longer in the store, tasks whose complexity changed (re-scoped), and done tasks with no invocation after the forecast are excluded from scoring and counted per forecast and overall.

The response reports, per measure, the number scored and the fraction of actuals at or under p50 and p90. A calibrated forecast lands near 50% and 90%.

### Surfaces

- `GET /api/diagnostics/backlog_forecast?workspace=…[&lookback_days=…][&concurrency=…][&arrivals=on]`, read-only apart from the throttled snapshot write.
- Analytics › Forecast in the dashboard: the quantile tiles, the by-complexity table, coverage counts, the arrival rate beside the no-arrivals assumption, and calibration ([user interface](../user-interface/2_design.md)).
- The Tasks page auto-drain card shows that same default forecast (30 days, no arrivals) as a compact summary under its admission forecast: time to drain p50 and p90, the backlog and in-progress counts, tokens when the payload has them, and cost only when the history is costed. The summary links to Analytics › Forecast. An error, a host without the endpoint, or no estimate is one muted line and leaves the rest of the card alone [ORB-18533].

The shared calculation is `crates/orbit-core/src/application/task/backlog_forecast.rs`, with `backlog_forecast/model.rs` simulating and `backlog_forecast/scoring.rs` scoring. The sweep clock and the HTTP adapter at `crates/orbit-web/src/api/diagnostics/backlog_forecast/` use that service over `orbit-store`'s `BacklogForecastStoreBackend`.

## Non-goals

- **Dispatch or admission.** The forecast steers nothing; admission keeps its own resource forecasts.
- **Per-host capacity.** Followers pulling from the workspace add capacity the forecast does not model.
- **Queueing on review or CI.** A delivery's sampled hours already contain its waits; the simulation does not model shared reviewer or CI capacity.

## Concerns and honest limitations

- **Admission can admit fewer.** In `enforce` auto-concurrency mode, resource admission may hold leaf runs below the ceiling, so the forecast is optimistic there; `auto_concurrency_mode` is reported for that reason.
- **Independence.** Draws are independent, but a bad week (a flaky CI, a provider outage) correlates deliveries; p90 understates that tail.
- **Survivor history.** Only finished work is sampled. Tasks rejected after burning cost are not in the history.
- **Short history.** A new workspace has few deliveries per cell; most tasks fall back and the counts say so. Calibration needs stored forecasts whose tasks have finished, which takes days.
- **Clock availability.** Unattended snapshots require the owner host’s sweep clock to run. A stopped clock or exhausted tick budget delays the next snapshot; a canonical read can still fill a free hourly slot.

## Phasing

1. **Forecast, storage and scoring.** The endpoint, the v62 table, scoring and Analytics › Forecast [ORB-18385].

## Open questions

1. **Arrival classes?** Sweep and auto-task filings are often small; drawing them from their own history rather than every delivery would sharpen `arrivals=on`.

## Acceptance (feature level)

- Given a backlog and delivery history, Orbit reports p50 and p90 time to drain, USD and normalized tokens overall and by complexity, with fallback and coverage counts.
- Given stored forecasts whose tasks finished, Orbit reports the fraction of actuals under p50 and p90 and counts the excluded tasks.

## Task References

- [ORB-18385] — added the backlog forecast, its storage and its scoring pass.
- [ORB-18533] — shows the default backlog forecast on the Tasks drain card.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
