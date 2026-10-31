---
title: "Scope: symbol risk index"
owner: claude
last_updated: 2026-10-30
last_validated: 2026-10-30
status: Draft
stage: designing
next: "Daniel reviews this scope and 2_design.md; implementation tasks are filed after acceptance"
feature: symbol-risk-index
doc_role: scope
type: design
summary: "Scope: a routine-built per-symbol index of body size, test coverage and regression-site history, read by admission riskiness and a test-priority view, each shipped only if its backtest beats today's model"
tags: [symbol-risk-index, riskiness, regression-risk, test-strategy, routines]
related_features: [backlog-forecast, routines, plugins]
---

# Scope: symbol risk index

Status: proposed. This doc and [2_design.md](./2_design.md) are the design deliverable of
[ORB-19358]; no code lands with them. 1_scope.md states the problem, the evidence, the
outputs and their ship gates; [2_design.md](./2_design.md) specifies the record, the indexer
boundary, storage, the routine, staleness and renames.

Bearing: per-task riskiness is `crates/orbit-core/src/application/codebase/riskiness.rs`,
initially shipped as estimator version 1 by [ORB-18690] and consumed by the backlog forecast
([ORB-18691], [backlog-forecast scope](../backlog-forecast/1_scope.md#per-task-riskiness-estimator)).
[ORB-19361] replaced its per-file maximum with a size-based fit: declared path count
before a diff exists, lines changed once one does. [ORB-19376] subsequently stabilized finite
fits and advanced the estimator to version 3 without changing those bases. Lines changed per
landed delivery are
already recorded by `codebase/diff_stats.rs` ([ORB-18383]). A `regression_from` link can carry
`regression.sites`, the `symbol:`/`file:` selectors of the code that was wrong (`dir:` when no
narrower site can be named)
(`crates/orbit-types/src/task/regression.rs`), and [ORB-19276] made Orbit always record
them. The Coverage workflow (`.github/workflows/coverage.yml`) uploads a workspace LCOV report
for `main` every six hours ([DEVELOPMENT.md](../../DEVELOPMENT.md#coverage)). The orbit-graph
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

## Shape

### What the index holds

One record per symbol per index generation (one generation per base commit). The full schema
is [2_design.md §1](./2_design.md#1-the-per-symbol-record).

| Field | Source | Used for |
|---|---|---|
| **Size**: body lines | The indexer's symbol extent at the base commit | Admission score; risk axis of the view |
| **Coverage**: line coverage, and branch coverage where the report has it | The Coverage job's llvm-cov LCOV, mapped onto symbol extents by the indexer (preferred) | Coverage axis of the view |
| **Coverage fallback**: test functions referencing the symbol | The indexer's reference graph | Retirement list only (see below) |
| **Regression-site history**: shrunk rate at which the symbol was a recorded defect site | Core, from `regression.sites` on `regression_from` links and per-delivery symbol changes | View's risk axis and task risk, each behind its own gate |

Line coverage is preferred over a test count because ten trivial tests count for less than one
test that exercises every line and branch. Branch coverage is used where the report carries it.
The fallback count is weak in this repository: boundary-first tests reach internal symbols
transitively, so a direct reference count reads near zero for most production code. The view
therefore uses it only to find symbols with many direct tests (retirement candidates), never
to call a symbol under-tested.

**Not computed for risk.** The index does not compute behaviors per function, caller fan-in,
domain count or crew proneness, and no consumer may add them to a risk score. In the backtest
each added nothing over size or turned negative once size was in the model (table above).
Size and site history are the only risk inputs; coverage is the view's second axis, not a
risk input.

### Outputs

1. **Admission risk.** Before a diff exists, [ORB-19361]'s estimator fits
   `p = 1 - exp(-exp(a + b·log(1 + size)))`, with size the declared path count. This feature
   proposes a `declared_lines` basis: size is the sum of the indexed lines of the declared
   footprint (a `symbol:` selector's body lines, a `file:` selector's indexed lines). A footprint
   the index cannot resolve keeps the `declared_files` basis, labelled with the reason.
   Review-time risk does not use the index: once a diff exists, its lines changed are the
   better size ([ORB-19361]'s `diff_lines`).
2. **Test-priority view.** A read-only ranking of production symbols with two lists:
   **under-tested**, high risk with low coverage, where a regression test pays for its gate
   cost; and **retirement candidates**, low risk with many direct tests, as input to the
   test-strategy suite shrink. Risk is the size percentile until site history passes its gate,
   then the fitted site probability.

### Ship gates

Every gate reruns the 2026-10-27 protocol: the same weekly rolling folds, extended to the
newest complete week, the same labels, and a paired bootstrap over test tasks (or over
symbols for the view). "Beats" means a higher AUC with the 95% interval of the paired
difference above zero, and log loss no worse. [2_design.md §8](./2_design.md#8-backtests-and-ship-gates)
gives the exact protocol and the index reconstruction per fold.

| Gate | Candidate | Must beat | If it fails |
|---|---|---|---|
| **G1 admission** | `declared_lines` basis, falling back to `declared_files` where unresolved | `declared_files` on the same tasks (AUC 0.704 in the 2026-10-27 run), by at least 0.01 AUC | Riskiness keeps `declared_files`; nothing in admission reads the index. Size remains a view input. |
| **G2 site history in task risk** | The shipped size model plus the footprint's maximum shrunk site rate, judged separately for the declared and diff bases | The shipped size model alone, by at least 0.01 AUC | Site history never enters task risk for a basis that fails. |
| **G3a view risk axis** | Symbol risk (size, plus site rate where it improves the fit) predicting the symbols named as sites in the next four weeks | The symbol's file-level shrunk regression rate, which is the existing Regressions ranking: since [ORB-19442] it counts a regressed task against a file only where its links' `regression.sites` resolve under it, not its whole footprint | No per-symbol risk ranking ships. The view and both of its lists are dropped; size and coverage stay readable as index data only. |
| **G3b view coverage axis** | G3a's model plus coverage | G3a's model alone, by at least 0.01 AUC | The under-tested list is dropped. Coverage stays a column, not a ranking input. |
| **G3c retirement list** | The low-risk half of symbols | Its next-window site rate must sit below the all-symbol site rate, with the upper 95% bound below it | The retirement list is dropped. |

G1 can run as soon as the index can be reconstructed for past folds. G2 and G3 wait for
roughly 100 `regression_from` links carrying `symbol:` sites, the threshold the 2026-10-27
riskiness backtest decision set for re-testing site history. Until a gate passes, its output
stays unshipped: the index may be read as data, but nothing ranks, scores or prompts from it.

### Reconciling the view with the test rules

The view must stay consistent with
[Boundary-First Testing](../../design-patterns/test_strategy.md) and the workspace guide
(`CLAUDE.md`/`AGENTS.md`): a new test is added only for an observed, reproduced defect, as a
focused regression check at a public CLI/API boundary that fails before the fix and passes
after. No private-function tests (private fixtures driving public behavior are fine). Where
that pattern reads more broadly, the workspace guide supersedes it; the pattern still governs
coverage priorities, retirement evidence and timing.

- **It mandates no test.** No auto-task, prompt or gate files "add a test for symbol X" from
  the under-tested list. Features, refactors and chores add no new tests; the existing suite
  gates them.
- **It informs bug-fix test placement.** When a fix task writes its one regression test for an
  observed, reproduced defect, the view shows how thinly the defect's site symbols are
  covered. That helps choose the public-boundary test that reaches them. The test stays at a
  public CLI/API boundary; there is no private-function unit-test exception.
- **It is evidence, not a verdict, for the test-quality audit.** The recurring
  `test-quality-audit` already files proposed tasks for critical coverage gaps. It may cite
  the under-tested list, but a gap counts as critical only with a recorded defect there (a
  `regression.sites` entry whose escape is `no_test`) or an uncovered security, recovery or
  incident invariant. Low coverage alone is not one.
- **It supplies retirement candidates, not retirements.** A retirement still follows
  [Retiring a unit test](../../design-patterns/test_strategy.md#retiring-a-unit-test) and the
  CI budget's rules: name the surviving coverage, measure the cost before and after,
  and never retire the only guard of a security invariant, recovery path or known incident.
  The unit-test growth ratchet is unchanged.

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `workflow.symbol_index.enabled` | `false` | Build and read the index. Off means no routine work and every reader behaves as today. |
| `workflow.symbol_index.indexer_tool` | unset | The plugin tool that produces a generation document ([2_design.md §2](./2_design.md#2-the-indexer-boundary)). Unset disables the index. |
| `workflow.symbol_index.coverage` | unset | Where to read LCOV: a CI artifact (workflow, artifact name, branch) or a file path. Unset leaves coverage absent. |
| `workflow.symbol_index.max_staleness_commits` | `200` | A generation further behind the integration head than this is stale for admission. |
| `workflow.symbol_index.max_staleness_days` | `7` | Same, by age. |
| `workflow.symbol_index.site_window_days` | `180` | Trailing window for regression-site history. |
| `workflow.symbol_index.retain_generations` | `2` | Generations kept per workspace. |

### Surfaces

- A seeded, disabled routine `symbol-index` with a job that rebuilds the index when the base
  or the coverage report has changed ([2_design.md §4](./2_design.md#4-the-routine)).
- A read-only `orbit.codebase.symbols` tool and `orbit codebase symbols` CLI: one symbol's
  record, with generation and staleness. The test-priority lists are added to it only by the
  implementation task that follows each passed gate.
- Riskiness gains `basis: declared_lines` and a `symbol_index` block (generation, lag, resolved
  and unresolved selectors) only after G1 passes. The task API and dashboard otherwise keep
  [ORB-19361]'s shape.

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

## Concerns and honest limitations

- **Site history is thin.** At about 100 site-bearing links, a few hundred symbols out of
  roughly 31,000 carry a label. G2 and G3 intervals will be wide, and a gate may stay
  unevaluable for months. An unevaluable gate counts as not passed.
- **Coverage lags the base.** The Coverage job runs on `main`, which trails `agent-main` until
  the next release merge. Coverage is attached only where a symbol's body is unchanged between
  the covered commit and the base. Recently edited symbols have no coverage, and they are the
  ones most likely to regress.
- **No branch coverage today.** The job runs on a pinned stable toolchain, and its LCOV carries
  line and function records only; cargo-llvm-cov's branch mode needs nightly. ws_orbit starts
  on line coverage.
- **Coverage is aggregate.** LCOV says how often lines ran across the whole suite, not how
  many distinct tests reached them. A per-test count needs per-test profiles, which costs
  roughly a profile merge per test. It is deferred (Open questions).
- **Declared footprints are mostly `file:` selectors.** `declared_lines` then mostly measures
  declared files in lines. Few footprints name symbols, so the per-symbol precision mostly
  serves the view, not admission.
- **Renames lose history when a body changes in the same window.** Lineage follows a
  symbol by indexer evidence or an unchanged body digest; a rename that also edits the body
  starts a new history ([2_design.md §5](./2_design.md#5-staleness-and-renames)).
- **Fold reconstruction approximates admission time.** G1 indexes each fold's boundary commit,
  not each task's exact base. Symbol sizes drift slowly within a week, but the approximation
  is disclosed with the result.

## Phasing

1. **Index and routine.** Store tables, the indexer contract, the routine and job, lineage,
   and the read tool. Size and digests only, with no consumers. Record the measured build cost
   and storage per generation.
2. **Coverage.** LCOV fetch and the indexer's mapping, coverage states, and the test-reference
   fallback.
3. **G1 backtest.** An analysis task reconstructs the index per fold and reports G1, with
   scripts and results attached as artifacts.
4. **Admission wiring**, only if G1 passes: the `declared_lines` basis in `riskiness.rs`, an
   estimator version bump, and the forecast docs note.
5. **Site history.** Symbol change events per landed delivery, site resolution through
   lineage, and shrunk rates in the read tool. This may land with phase 1, since it only
   accumulates data.
6. **G2/G3 backtests and the view**, once about 100 site-bearing links exist: run the gates,
   then ship only what passed.

No implementation task for these phases was found. [ORB-19370] reconciled the design's test
guidance with the workspace guide, and [ORB-19371] specified publication fencing; both are
completed design repairs, not index implementation.

## Open questions

1. **Coverage on `agent-main`?** Dispatching the Coverage workflow on the integration branch
   would close the lag but doubles its runner cost. Is fresher coverage worth that before G3b
   has shown that coverage matters?
2. **Per-test coverage counts?** Daniel asked how many tests cover each symbol. Aggregate LCOV
   cannot answer that. Per-test profiles can, at a cost to measure first. Should the count wait
   until G3b shows coverage is predictive?
3. **Indexer without a plugin?** A configured command needs a sandboxed, granted execution
   path. [Script steps](../script-steps/1_scope.md), designed by [ORB-19294], now have
   manifests and grants ([ORB-19983]) and granted Linux execution ([ORB-19985]), while
   declared file inputs and macOS execution remain later work. Until the indexer's file
   contract can use that path, it stays a plugin tool.
4. **Generation per base commit, or per N deliveries?** Rebuilding on every advance is
   cheapest to reason about. If a build costs minutes, a coarser cadence trades staleness for
   load.

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

## Task References

- [ORB-19358] — wrote this scope and design
- [ORB-19361] — replaced the per-file maximum with a size-based riskiness fit (declared files, diff lines)
- [ORB-19376] — stabilized finite fits and advanced the size estimator to version 3
- [ORB-19370] — reconciled the proposed view with the workspace's test rules
- [ORB-19371] — specified publication fencing for overlapping index builds
- [ORB-19276] — made Orbit always record `regression.sites`
- [ORB-19442] — attributed the Regressions ranking (the G3a baseline) to `regression.sites` and admitted `dir:` sites
- [ORB-18690] — shipped per-task riskiness estimator version 1
- [ORB-18691] — made the backlog forecast consume riskiness for regression rework
- [ORB-18383] — recorded lines added and deleted per landed delivery
- [ORB-19294] — designed script steps, a granted execution path for workspace scripts
- [ORB-19983] — added script manifests, parsing and digest-bound grants
- [ORB-19985] — executed granted script steps on Linux under the script sandbox profile

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
