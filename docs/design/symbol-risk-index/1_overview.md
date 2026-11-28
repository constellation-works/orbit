---
title: Symbol Risk Index — Overview
owner: claude
created_date: 2026-10-27
updated_date: 2026-11-28
last_validated: 2026-11-06
status: Draft
stage: designing
next: "Daniel reviews this overview and 2_design.md; implementation tasks are filed after acceptance"
feature: symbol-risk-index
doc_role: overview
type: design
summary: "A routine-built per-symbol index of body size, test coverage and regression-site history, read by admission riskiness and a test-priority view, each shipped only if its backtest beats today's model"
tags: [symbol-risk-index, riskiness, regression-risk, test-strategy, routines]
related_features: [backlog-forecast, routines, plugins]
---

# Symbol Risk Index — Overview

Status: proposed. This doc and [2_design.md](./2_design.md) are the design deliverable of
[ORB-19358]; no code lands with them. This overview states the problem, the evidence, the
non-goals and the acceptance criteria; [2_design.md](./2_design.md) specifies the record, the indexer
boundary, storage, the routine, staleness and renames.

Bearing: per-task riskiness is `crates/orbit-core/src/app/codebase/risk.rs`,
initially shipped as estimator version 1 by [ORB-18690] and consumed by the backlog forecast
([ORB-18691], [backlog-forecast scope](../backlog-forecast/2_design.md#per-task-riskiness-estimator)).
[ORB-19361] replaced its per-file maximum with a size-based fit: declared path count
before a diff exists, lines changed once one does. [ORB-19376] subsequently stabilized finite
fits and advanced the estimator to version 3 without changing those bases. Lines changed per
landed delivery are
already recorded by `codebase/diff_stat.rs` ([ORB-18383]). A `regression_from` link can carry
`regression.sites`, the `symbol:`/`file:` selectors of the code that was wrong (`dir:` when no
narrower site can be named)
(`crates/orbit-types/src/task/regression.rs`), and [ORB-19276] made Orbit always record
them. The Coverage workflow (`.github/workflows/coverage.yml`) uploads a workspace LCOV report
for `main` every six hours ([development reference](../../development/testing.md#coverage)). The orbit-graph
plugin already indexes symbols with source extents in ten languages and maps changes to
symbols and candidate tests. Routines run catalog jobs from the sweep clock
([routines design](../routines/2_design.md)).

## Problem

Daniel (2026-10-27): rate each symbol upfront, as a routine, and track how many tests cover
it, so tests can be focused on high-risk symbols. Risk exists only per task, fitted from
historical task sizes and regression outcomes at read time. Admission scoring and test
planning have no shared,
standing per-symbol record.

Two backtests on ws_orbit history set what such a record should hold. Each used 11 weekly
rolling folds, about 4,550 test tasks, 13.8% regressed, with labels from `regression_from`
links. The scripts live in the 2026-10-27 orchestrator session; this doc summarizes them and
does not re-run them.

| Feature or model | Result |
|---|---|
| Lines changed (diff) | AUC **0.737**, the best single feature |
| Files changed (diff) | AUC 0.714 |
| Declared files, `1-(1-p)^k` (from [ORB-19361]) | AUC 0.704 |
| Then-production estimator v1: max of shrunk per-file rates, crew × complexity shift | AUC 0.64; Brier score worse than predicting the constant base rate |
| Behaviors per changed function (1 + branch count) | AUC 0.675 alone; nothing over size |
| Caller fan-in with depth decay | coefficient turns negative once size is in the model |
| Number of domains touched | coefficient turns negative once size is in the model |
| Crew proneness per unit of change | about +0.004 AUC, within noise |
| Per-crew size slopes | omnibus test not significant (p≈0.17–0.46) |
| Free-slope fit `1-exp(-exp(a + b·log size))` (from [ORB-19361]) | AUC no better than fixed slope; best calibration (logloss 0.377 vs 0.397) |

Size carries almost all of the signal. Per-file attribution, branch counts, call-graph reach,
domain spread and crew identity add nothing once size is controlled, and some make the score
worse. Two candidate signals could not be tested:

- **Per-symbol regression-site history.** Until `regression.sites` accumulate, a label blames
  the whole culprit task, not the code that was wrong. There is no site history to test yet.
- **Coverage.** Nobody has measured line or branch coverage against later regressions.

The fact that shapes the fix: before a diff exists, the only size Orbit can see is the declared
footprint, and today that is a count of paths. A count treats a 20-line helper file and a
3,000-line module alike. A per-symbol line count, kept current by a routine, lets admission
measure a declared footprint in lines.

## Goal

A standing per-symbol index, rebuilt per base commit by a routine, that holds each symbol's
body size, its test coverage and its regression-site history. Two consumers read it: admission
riskiness (a declared footprint scored in lines) and a test-priority view (where a regression
test pays for its gate cost, and where tests are candidates for retirement). Each consumer
ships only if a stated backtest beats today's model; otherwise it is dropped and the current
behavior stays. Language parsing stays outside Orbit core. Task riskiness's API, states and
the forecast's consumption of `probability` are unchanged.

## Non-goals

- **Running coverage.** The index reads the Coverage job's report; it never runs an
  instrumented suite itself.
- **Parsing languages in core.** Symbol extents, LCOV-to-symbol mapping, test detection and
  rename evidence come from the indexer plugin. Core stores opaque selectors and numbers.
- **Call-graph, branch-count, domain or crew risk factors.** Excluded by the backtest above.
- **Review-time scoring from the index.** The actual diff's lines changed are the better
  measure once a diff exists.
- **Generating tests or test tasks.** See the reconciliation above.
- **Gating merges or admission.** Riskiness stays informational and forecast input; the index
  adds no admission or review policy.

## Acceptance (feature level)

- Given an enabled routine and an indexer plugin, each time the integration head or the
  coverage report changes, Orbit records one generation with a body-line count for every
  indexed symbol. A run with neither changed records nothing.
- Given a generation, `orbit codebase symbols <selector>` reports the symbol's size, coverage
  and coverage state, site-history counts and shrunk rate, and the generation's base commit and
  lag.
- Given a renamed symbol with an unchanged body, its site history follows it to the new
  selector.
- Given G1 has not passed, riskiness keeps the size estimator from [ORB-19361], including
  [ORB-19376]'s finite-fit repair. Given it passed, a
  footprint fully resolved by a fresh index is scored on `declared_lines`; any other footprint
  is scored on `declared_files` and names the reason.
- Until a view gate passes, Orbit exposes no list for it, and nothing ranks symbols.
- No risk score anywhere reads fan-in, branch counts, domain count or crew proneness.

The mechanisms, ship gates and open questions are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## Task References

- [ORB-19358] — wrote this scope and design
- [ORB-19361] — replaced the per-file maximum with a size-based riskiness fit (declared files, diff lines)
- [ORB-19376] — stabilized finite fits and advanced the size estimator to version 3
- [ORB-19276] — made Orbit always record `regression.sites`
- [ORB-18690] — shipped per-task riskiness estimator version 1
- [ORB-18691] — made the backlog forecast consume riskiness for regression rework
- [ORB-18383] — recorded lines added and deleted per landed delivery

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
