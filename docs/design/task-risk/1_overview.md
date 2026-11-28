---
title: Task Risk — Overview
owner: codex
created_date: 2026-10-29
updated_date: 2026-11-28
last_validated: 2026-11-07
status: Draft
stage: designing
next: "Review estimands and evidence prerequisites before authorizing implementation."
feature: task-risk
doc_role: overview
type: design
summary: "Advisory task-risk estimates separating model-relative complexity, repair burden and escaped regressions."
tags: [task-risk, backlog-forecast, agent-scorecards]
---

# Task Risk — Overview

Task risk is a draft design for estimating three operational outcomes of a task, conditional on its demands and execution configuration, with size kept as evidence and every estimate advisory only: it cannot silently alter admission, crew selection, review policy or scorecard weights.

Status: Draft; design only. No scoring, routing or implementation change is approved.

## 1. Motivation

Bearing: The [backlog forecast](../backlog-forecast/2_design.md#per-task-riskiness-estimator)
consumes the task riskiness estimator; [agent scorecards](../agent-scorecards/2_design.md)
already distinguish execution provenance and role attribution. The
[task risk display](../user-interface/2_design.md) serves the same derived estimate
in lists and details. This scope extends those mechanisms, without changing their
current contracts. [ORB-20095] requests this design.

### The gap

A single size curve cannot answer whether a task's reasoning demands exceed a
particular model's reliable capability. Building a compiler from scratch can add
interacting semantic, optimization and code-generation obligations faster than its
line count suggests. Luna and Astra may have different responses; neither equal
capability nor accelerating error growth follows from that example alone.

At base `fe97e5d54dd3cda67f426d106e95b121b77a493f`,
[`riskiness.rs`](../../../crates/orbit-core/src/app/codebase/risk.rs)
is estimator v3: `p = 1 - exp(-exp(a + b * ln(1 + size)))`, separately fitted to
declared paths and available review/landed diff lines. Its outcome is a done task
named by any recorded `regression_from` link. It does not measure unsuccessful
attempts, a fixed follow-up horizon, latent defect counts or intrinsic capability.
A well-reviewed weak initial implementation can land safely; a failed attempt
cannot appear in this completed-task denominator at all.

Historical evidence must retain that distinction. [ORB-19361] introduced the
size-only estimator using backtests also summarized by the
[symbol-risk design](../symbol-risk-index/1_overview.md) under [ORB-19358]. Their task
records and artifacts were inspected: they describe weekly temporal comparisons,
but expose no original analysis script or frozen input manifest. Consequently this
design does not reuse their numerical performance or significance claims as
verified results. Without the original cohort and label rules, those summaries
cannot validate production calibration, model equivalence, absent interactions or
extrapolation to harder work. In particular, pre-attribution tasks must not be
treated as clean negatives.

[ORB-20094] records the completed October 29, 2026 exploratory analysis; its
scripts, frozen input, results and within-tier comparisons are attached to
[ORB-20095] under `backtest-2026-10-29/`. [ORB-20107] adds the results and
size-response figures to this document. The [dated evidence report](references/2026-10-29-risk-backtest.md)
records the input hash, cohort, joins, cutoffs, exclusions, uncertainty and
provenance limits. These are descriptive seven-day observations and exploratory
temporal comparisons, not the proposed primary 30-day validation or a causal
capability study. A separate display-only panel ([ORB-20380]) plots the same
kind of seven-day escaped-regression rate against landed size on Analytics ›
Agents › Implementers (`application::agents::roles::implementers::size_curves`).
It does not install the proposed `task_risk` payload, change `riskiness`, or
authorize scoring, routing or admission.

The task's 06:29 UTC evidence update identifies source-policy changes, confirmed
against their git diffs: `09e60b0757811368879fd0885f36950f586c0aa0`
(2026-09-13 UTC) introduced review/CI attribution;
`536c84994cb5e8dcf7b88d1bd0d75a0e5d3061ca` (October 8) repaired claimed-worker
filing; `37d10744d68d076be18ace14b3387d9f961da570` (October 19 UTC) added typed
nature/escape; and `689139770b3a7bb399d3b4834c2bb355adf803db` (October 25 UTC)
expanded QA/full-review attribution. These commits establish policy changes,
not deployment dates or complete reporting. A confirmed reporting-coverage start
and channel-specific coverage intervals are required inputs; never infer them
from the first link or label pre-tracking silence clean. Evaluate contemporaneous
and retrospective/backfilled links separately. A mature horizon must lie wholly
inside a supported reporting interval. At this draft date a stable period starting
with the October 8 repair cannot yet provide 30-day mature outcomes: shorten only
in a separately labelled exploratory analysis, or wait, never silently redefine H.

### Intended outcome

Estimate three distinct operational outcomes conditional on task demands and
execution configuration, while retaining size as evidence. Make prediction time,
population, uncertainty and unsupported cases explicit. Keep core generic across
models and projects, and keep risk advisory: a new estimate cannot silently alter
admission, crew selection, review policy or scorecard weights.

### Out of scope

- **Production changes.** This task writes a design and index only; it dispatches
  no implementation and does not alter scoring, routing, admission or models.
- **Permanent rankings.** No hard-coded Luna/Astra ordering, exponential law,
  or causal capability score from observational regression links.
- **Unobservable guarantees.** No promise of total latent defect counts, complete
  reporting or reliable extrapolation beyond the evidence.
- **Unrelated permissions.** Office permission repair is independent of this work.

### Acceptance (feature level)

- Every prediction names its stage, population, horizon/budget and units;
  failed/unreviewed/immature cases cannot silently become clean observations.
- Model-blind snapshots and actual execution provenance reproduce each estimate;
  unknowns, mixed models and uncertainty are visible without alias reconstruction.
- Temporal and mature shadow comparisons are replayable from receipts, with
  matched size baselines, interaction ablations, support and explicit gate outcomes.
- Unsupported requests return reasons and advisory next steps, not invented
  percentages. Predictive comparisons do not imply causal capability rankings.
- Old readers and forecasts remain compatible until separately validated versioned
  integrations; performance, rollback and unchanged admission are demonstrated.

The estimands, protocol, evidence and open questions are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## 2. Core Concepts

- **Task riskiness estimator.** The shipped size-only estimator (v3) that fits regression probability from declared paths or diff lines; this design extends it without changing its contract.
- **Operational outcome.** One of the three outcomes the design estimates, each with a named stage, population, horizon or budget and units.
- **Estimand.** The precise quantity a prediction claims, so failed, unreviewed or immature cases cannot silently become clean observations.
- **Execution provenance.** The recorded model and configuration that actually ran, so estimates are reproducible from model-blind snapshots.
- **Unsupported case.** A request the evidence cannot support; it returns reasons and advisory next steps instead of an invented percentage.
- **Shadow comparison.** A replayable temporal or mature comparison, with matched size baselines and explicit gate outcomes, run before any versioned integration.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Outcomes and denominators | [2_design.md](./2_design.md#outcomes-and-denominators) | [ORB-20095] |
| Assignment-time complexity and provenance | [2_design.md](./2_design.md#assignment-time-complexity-and-provenance) | [ORB-20095] |
| Hierarchical responses and identification | [2_design.md](./2_design.md#hierarchical-responses-and-identification) | [ORB-20095] |
| Exploratory evidence from October 29, 2026 | [2_design.md](./2_design.md#exploratory-evidence-from-october-29-2026) | [ORB-20094], [ORB-20107] |
| Dated evidence report | [references/2026-10-29-risk-backtest.md](./references/2026-10-29-risk-backtest.md) | [ORB-20094] |
| Uncertainty and unsupported cases | [2_design.md](./2_design.md#uncertainty-and-unsupported-cases) | [ORB-20095] |
| Temporal and shadow protocol | [2_design.md](./2_design.md#reproducible-temporal-and-shadow-protocol) | [ORB-20095] |
| Current size-only estimator | [risk.rs](../../../crates/orbit-core/src/app/codebase/risk.rs) | [ORB-19361] |
| Display-only size curves | [2_design.md](./2_design.md#surfaces-compatibility-and-performance) | [ORB-20380] |
| Symbol-risk backtests it cites | [symbol-risk-index](../symbol-risk-index/1_overview.md) | [ORB-19358] |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |

## Task References

- [ORB-20380] — shows display-only escaped-regression size curves on the Implementers panel; it does not install the proposed estimator.
- [ORB-20107] — adds the exploratory backtest, within-tier curves and evidence report.
- [ORB-20095] — requests this model-relative task-risk Draft and generated index.
- [ORB-20094] — authorized the authoring-session backtest and owns its receipts.
- [ORB-19361] — introduced the size-only riskiness estimator and recorded prior evidence.
- [ORB-19358] — scoped the symbol-risk index and summarized the historical backtests.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
