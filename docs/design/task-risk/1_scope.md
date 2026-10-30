---
title: "Scope: Task risk relative to model capability"
owner: codex
last_updated: 2026-10-30
last_validated: 2026-10-30
status: Draft
stage: designing
next: "Review estimands and evidence prerequisites before authorizing implementation."
feature: task-risk
doc_role: scope
type: design
summary: "Advisory task-risk estimates separating model-relative complexity, repair burden and escaped regressions."
tags: [task-risk, backlog-forecast, agent-scorecards]
---

# Scope: Task risk relative to model capability

Status: Draft; design only. No scoring, routing or implementation change is approved.

Bearing: The [backlog forecast](../backlog-forecast/1_scope.md#per-task-riskiness-estimator)
consumes the task riskiness estimator; [agent scorecards](../agent-scorecards/2_design.md)
already distinguish execution provenance and role attribution. The
[task risk display](../user-interface/2_design.md) serves the same derived estimate
in lists and details. This scope extends those mechanisms, without changing their
current contracts. [ORB-20095] requests this design.

## Problem

A single size curve cannot answer whether a task's reasoning demands exceed a
particular model's reliable capability. Building a compiler from scratch can add
interacting semantic, optimization and code-generation obligations faster than its
line count suggests. Luna and Astra may have different responses; neither equal
capability nor accelerating error growth follows from that example alone.

At base `fe97e5d54dd3cda67f426d106e95b121b77a493f`,
[`riskiness.rs`](../../../crates/orbit-core/src/application/codebase/riskiness.rs)
is estimator v3: `p = 1 - exp(-exp(a + b * ln(1 + size)))`, separately fitted to
declared paths and available review/landed diff lines. Its outcome is a done task
named by any recorded `regression_from` link. It does not measure unsuccessful
attempts, a fixed follow-up horizon, latent defect counts or intrinsic capability.
A well-reviewed weak initial implementation can land safely; a failed attempt
cannot appear in this completed-task denominator at all.

Historical evidence must retain that distinction. [ORB-19361] introduced the
size-only estimator using backtests also summarized by the
[symbol-risk design](../symbol-risk-index/1_scope.md) under [ORB-19358]. Their task
records and artifacts were inspected: they describe weekly temporal comparisons,
but expose no original analysis script or frozen input manifest. Consequently this
design does not reuse their numerical performance or significance claims as
verified results. The reported small incremental crew signal supports considering
a pooled predictor in the observed workload, not equivalence of models, no
interaction, or extrapolation to much harder tasks.

[ORB-20094] owns the newly authorized authoring-session backtest. The 2026-10-30 06:26 UTC preparation comment reports limited recent landed-diff
telemetry and explicitly holds scoring pending the reliable regression-reporting
start date. Its artifact list was still empty at readback; these are preparation
observations, not backtest results. Until receipts are published and inspected,
that run supplies no measured result here. Required receipts are script and input hashes,
cutoffs, joins, exclusions, fold-level metrics and provenance coverage. Missing
receipts prevent empirical promotion, not writing this Draft.

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

## Goal

Estimate three distinct operational outcomes conditional on task demands and
execution configuration, while retaining size as evidence. Make prediction time,
population, uncertainty and unsupported cases explicit. Keep core generic across
models and projects, and keep risk advisory: a new estimate cannot silently alter
admission, crew selection, review policy or scorecard weights.

## Shape

### Outcomes and denominators

Use a linked task → admission episode → implementation candidate → landing ledger.
An episode follows the forecast's exact run/claim retry lineage, not invocation
gaps. Preserve all episodes, including those with no candidate or landing.

| Estimand | Unit, denominator and observable label | Gap and reporting rule |
| --- | --- | --- |
| Implementation failure | Among admitted implementation episodes, probability of ending without an accepted candidate under a specified time/token/retry budget; separately report no accepted candidate by deadline D from admission. | Successful handoff is distinct from landing. Live episodes before D are censored, not successes; unresolved at D is noncompletion, not necessarily model error. Report crew-attributed failure separately from infrastructure, cancellation, specification and unknown causes. |
| Substantive repair burden | Expected number of distinct confirmed correctness/scope findings against the first submitted candidate per assessed candidate; separately expected repair work per admitted episode, including failed attempts. | Deduplicate finding IDs and repair lineages; cosmetic edits and false rejects are excluded, unclassified findings separate. No review means unobserved, not zero. Repair hours/tokens and change volume are separate units, never automatically a count of true defects. |
| Escaped recorded regression | Among landed task deliveries with known landing time, probability of at least one qualifying recorded regression within H days of landing, and expected distinct regression count/severity distribution in that horizon. | Conditional on landing under the observed review/test regime. Unfiled or misattributed defects are invisible; never call this total defect probability. Non-landed episodes remain in the failure population, not clean regression observations. |

Propose H = 30 days for the primary escaped-risk evaluation, with 7/90-day
sensitivity analyses when supported; D and the episode resource budget must be
fixed in the study manifest before evaluation. Use half-open intervals
`[landing_at, landing_at + H)` in UTC. Label a regression by its first recorded
qualifying link time, not its eventual repair completion. Preserve both event time
and when the system first knew it; if only current links survive, mark historical
as-of labels unreconstructable and restrict them to a descriptive analysis.

One culprit delivery contributes at most one binary event. Counts deduplicate
`(culprit delivery, regression finding)` across replayed links and repair tasks;
multiple culprit tasks each receive a conditional event, so aggregate counts are
not unique workspace incidents. Report both counts rather than summing them as
incident totals. Resolve culprit landing/candidate exactly from delivery lineage;
ambiguous multiple landings stay unattributed. Exclude withdrawn/invalid findings
under a versioned adjudication rule with as-of knowledge; show raw-link sensitivity.
Report severity categories and missing/default severity separately. A weighted
severity cost requires an approved weight schedule and sensitivity analysis.

A defect-count model is a hypothesis about a future adjudicated label. Today the
observable targets are findings, repairs and linked regressions. There is no
identified conversion from these proxies to all latent defects. Probability of
one or more events saturates; expected count, repair-work quantiles and severe-event
risk retain information about costly tails beyond that saturation.

### Assignment-time complexity and provenance

Before model assignment, freeze a model-blind assessment against the task revision
and repository base. Each dimension is ordinal 0–3 with a rationale and cited
requirements; unknown is null, never 0. Keep the vector, not only a sum or today's
low/medium/hard/xhard routing label.

| Dimension | 0 | 1 | 2 | 3 |
| --- | --- | --- | --- | --- |
| Algorithmic/reasoning demand | Mechanical mapping | Established local algorithm | Several invariants or nontrivial algorithm | New algorithm/proof or coupled global reasoning |
| Interactions and boundaries | One isolated behavior | Several local behaviors | Cross-component/state transitions | Many coupled states, protocols or concurrency boundaries |
| Novelty | Exact existing pattern | Adaptation of pattern | Unfamiliar subsystem/domain | New architecture or poorly specified domain |
| Correctness requirements | Simple local invariant | Multiple explicit invariants | Compatibility/recovery obligations | Strong adversarial, safety or distributed invariants |
| Verifiability difficulty | Deterministic complete oracle | Good focused oracle | Partial or costly oracle | Sparse oracle, rare events or unavailable target environment |
| Expected scope | One bounded edit | One component | Several components | New subsystem or open-ended scope |

Snapshot assessment ID/version, assessor provenance, timestamp, input digest,
confidence per dimension, missing fields, expected path/line range and its basis.
Keep impact (blast radius, severity and exposure) separately from difficulty;
a trivial edit can have high impact. Delivered size is a later measurement.
For the compiler example, record high novelty/interactions and uncertain scope;
do not manufacture a percentage or infer a rating from which model was assigned.

Later assessments are new immutable snapshots. Retrospective assessments must
be blind to model, outcome and final diff and are marked retrospective; they
cannot impersonate prospective snapshots. Measure inter-rater agreement on a
blinded sample, test sensitivity to adjacent ordinal levels and missingness,
and compare a measurement-error/latent-complexity treatment with the raw rubric.
Train-only imputation and explicit missing indicators are baselines; uncertainty
in expected size is integrated over a range, not replaced by landed size.

Record actual execution model identifier/version, provider, reasoning effort,
configuration digest, tool/context budgets, retries, prompts/toolchain versions
and implementer versus reviewer/repair roles per invocation and candidate.
Use frozen run or claim-settlement provenance where present, never today's crew
alias mapping. Unknown version/configuration remains unknown. Multiple models
contributing to a candidate form a sequence or mixed-provenance stratum, not all
credit to the last model; keep the first implementer outcome and the final system
outcome distinct. Retry allowance is an assignment-time treatment; realized
retries are post-assignment outcomes, unavailable to the initial prediction.

### Hierarchical responses and identification

Fit a fleet → versioned capability class → actual model/version hierarchy, with
partial pooling for both intercepts and complexity/size responses. Classes are
revisable groupings based on independent, dated capability evidence available at
the cutoff; they are not names such as “always strong” nor ranks inferred from
future task outcomes. Unknown classes pool directly to the fleet. Learn class
separation and within-class variation; allow rankings to cross by demand and
change over time. Model releases split strata unless evidence supports pooling.

A candidate burden model, not a selected implementation, is:

```text
N_i ~ NegativeBinomial(mu_i, dispersion)
log(mu_i) = alpha_model + f_model(complexity_vector)
            + beta_model * log(1 + size_i) + regime_terms
(alpha, response coefficients)_model ~ class distribution
class parameters ~ fleet distribution
```

Use this only for observed counts with a defined observation process. Compare
Poisson, overdispersed and hurdle alternatives for excess zeros. Fit failure as
a separate binary/competing-outcome model; compare fixed-horizon logistic or
complementary-log-log regression with survival models for escaped events.
Compare pooled size-only, model intercepts, shared complexity, model-specific
linear slopes, bounded splines and complexity × size interactions. Shrink added
terms and constrain their flexibility before holdout evaluation. A log link does
not establish exponentially growing defects; nonlinear shapes must earn support.
Monotonicity can be a sensitivity model, not proof that every measured curve must
increase. Avoid a complex latent joint model until the labels can identify it.

For assignment-time prediction use frozen requirements, expected size and the
proposed execution/review regime. For review-time prediction use the identified
candidate's actual size and review/test evidence available then. Final size is
partly caused by model choice, decomposition and repairs: conditioning on it can
hide a model's total effect or induce selection bias. Publish these as different
prediction stages; never use review-time accuracy to claim routing accuracy.

Reviewer model, review mode, tests selected/executed, test quality, repair actors,
budget and repository/time effects belong in the observation ledger. Report
initial candidate burden and residual post-review risk separately. Planned regime
can condition an assignment prediction; realized tests/repairs are mediators,
not pre-assignment adjustment variables. A stronger reviewer may find more issues
and reduce escapes, so a higher repair count need not mean a worse implementer.
Do not multiply an inferred defect rate by an assumed detection probability.

Routing chooses models based on expected difficulty, cost, availability and
unrecorded operator knowledge; landing and review selection further filter the
sample. Observational coefficients are conditional predictions, not causal
capability effects. Stratification/propensity weighting helps only with measured
confounders and overlap; report effective sample sizes and weight truncation,
and decline model contrasts outside common support. Controlled, separately
authorized comparisons need randomized assignment within safe overlapping task
strata, fixed budgets/tools/review rules, blinded independent adjudication,
predefined stopping and severity safeguards, and task-level clustering. Replay
benchmarks supplement but cannot replace prospective production outcomes.

### Uncertainty and unsupported cases

Return intervals for the mean probability/count and predictive intervals for
burden, with support counts, event counts, censoring, missing provenance and
local complexity/size ranges per model/class. Include training cutoff, outcome
maturity, observation cutoff, model/label/rubric versions and effective support
after pooling. Quantify fold/task clustering and rubric uncertainty, not just
optimizer error. A zero-event small sample never establishes zero risk.

Cold-start models may use a broad class/fleet prior only within task support,
explicitly labelled prior-dominated, with sensitivity to class assignment. A new
architecture, unsupported complexity/size combination, unknown critical inputs,
unresolved provenance or unstable fit returns an unsupported/provisional state
with reasons; no calibrated percentage is invented. A supported size-only estimate
may be shown separately under its original label, never as a validated capability
estimate. Advisory next steps are to clarify/split the task, seek stronger review
or run a bounded evaluation; any escalation or model change remains an operator
decision. Sparse evidence must not create an automatic cheap-model preference.

### Reproducible temporal and shadow protocol

1. **Freeze the study.** Store script/environment hashes, data snapshot hash,
   source revision, random seeds, UTC extraction time, confirmed reporting-coverage
   intervals by channel, cohort rules, H/D/budgets,
   folds, label versions, deduplication joins, feature availability times and
   exclusions. Publish an attrition table from admissions through candidates,
   reviews and landings, including failed/non-landed work. Audit examples of
   each join against run, claim, candidate, landing and relation history.
2. **Reconstruct as-of time.** At each weekly origin T, train only on inputs and
   labels known at T. The simple primary escaped-risk cohort uses landings no
   later than T−H; test on the following week's landings and score after H
   plus a declared ingestion grace period. Features are frozen at assignment
   or the named candidate stage, not reconstructed from mutable current tasks.
   Retraining never sees future links, metadata edits, current crew aliases or
   future model-class assignments. Delay final scoring until the test cohort
   matures. Older proxy-only records remain a separate sensitivity cohort.
3. **Handle incomplete observation.** Primary fixed-horizon analysis excludes
   immature labels from evaluation with counts, without calling them negatives.
   Alternative survival/competing-risk analysis keeps censored follow-up and
   reports assumptions and censoring-weight support. Repository observation
   loss and reporting delay can be informative; sensitivity bounds and a
   missingness audit accompany results. Late-recorded regressions beyond H
   measure a different target; report horizon sensitivity instead of relabeling.
4. **Compare fairly.** Reproduce current v3 as a legacy reference and refit a
   size-only baseline to exactly the new horizon/cohort. Use declared/expected
   size for assignment comparisons and candidate size for review comparisons.
   Add class/model intercepts, shared complexity, varying slopes and nonlinear
   interactions one at a time; ablate size, complexity, configuration and review
   regime. Tune priors, knots and thresholds only within earlier training folds.
   Cluster all attempts of a task and related repair chains; purge boundary
   overlap where a chain could leak the held-out outcome.
5. **Report support and performance.** For each fold/model/class and demand
   stratum show tasks, episodes, landings, events, reviewed candidates, unknowns,
   follow-up distribution and common complexity/size support. Report calibration
   curves/intercept/slope, log loss, Brier, ROC-AUC and PR-AUC for binary outcomes;
   count deviance/log score, MAE, overdispersion and predictive-interval coverage
   for burden, plus severe-event calibration and repair-work quantiles. Use
   paired time-block/task-cluster uncertainty intervals and fold distributions;
   report sparse subgroup results as inconclusive, not equal. Aggregate AUC alone
   cannot promote a model or settle capability differences.
6. **Shadow prospectively.** Freeze predictions before assignment and again at
   candidate submission, logging which features were available. Keep existing
   behavior unchanged. Wait for mature horizons and compare replay with shadow
   performance, latency and missingness; monitor drift by release and task mix.

Ship gates apply separately to each estimand and stage. Before opening the final
holdout, register minimum events/effective support from a precision/power study,
acceptable calibration error, burden-coverage tolerance, practical improvement
and subgroup noninferiority margins with an accountable approver. No promotion
is allowed while these are unset. Require reproducible joins and no discovered
leakage; a paired 95% interval for improvement in the registered primary proper
score must clear its practical margin against the matched size-only baseline,
with calibration and supported safety subgroups within registered limits across
temporal and mature shadow cohorts. A useful pooled fit need not earn model
contrasts. Each added interaction must beat its simpler ablation or be removed.
Missing provenance/overlap, insufficient events, unstable intervals, degradation
or unmeasured severe tails means no-ship for that claim; keep the old estimator
and publish the inconclusive result. Causal ranking requires the controlled
comparison above even when predictive gates pass.

### Configuration

These are proposed study/serving controls, not new configuration keys or defaults
installed by this document. Keep current `workflow.regression_risk` semantics.

| Control | Initial proposal | Meaning |
| --- | --- | --- |
| Outcome horizon | 30 days; 7/90 sensitivity | Fixed follow-up, independent of training lookback |
| Attempt deadline and budgets | Required per protocol | Defines noncompletion and comparable execution opportunities |
| Rubric/class/label versions | Immutable study manifest | Reconstructs features, grouping and outcomes as known then |
| Support/calibration gates | Required before holdout | No silent use of current small fit-support threshold for model comparisons |
| Serving mode | Off, then authorized shadow | Advisory display activation requires separate approval |

### Surfaces, compatibility and performance

Propose an additive versioned `task_risk` payload with `stage`, `prediction_at`,
`candidate_revision`, `estimands`, `horizon_days`, `population`, `state/reasons`,
`estimate/interval`, burden units, provenance/support summaries and evidence
cutoffs. Keep legacy `riskiness` unchanged during migration; do not repurpose its
`probability`, resurrect deprecated crew adjustment fields or mix denominators.
New readers show “Recorded regression within 30 days, conditional on landing,”
“Noncompletion under this budget,” and “Expected confirmed repairs” separately.
Unknown is unavailable, never 0%; prior-dominated and out-of-distribution states
must be visible in keyboard-accessible detail as well as tooltips. Impact and
severity are labelled separately from likelihood. Older clients retain the legacy
view; internal task IDs are not user-facing labels.

Any changed estimate on the same facts bumps the estimator version. Changed
label/horizon/rubric semantics require explicit schema/model metadata and a
migration note. A forecast consuming new completion, burden or escaped-risk
semantics must bump its own model version and calibrate separately; it cannot
silently swap a fixed-horizon conditional probability for the existing rework
probability or double-count failures/repair episodes already simulated. Until
that integration is independently validated the forecast uses the old input.
Scorecard ranking and admission policy stay unchanged.

Build one immutable fit per workspace/evidence cutoff, off the request path;
batch-score task lists. Cache by estimator, label/rubric/class versions, training
cutoff, configuration digest and prediction stage; candidate estimates also key
base/head and snapshot ID. Bound caches and fit complexity, invalidate on new
model versions/labels or snapshot changes, expose stale timestamps, and avoid
per-row history scans or provider calls. Before serving, measure cold build,
warm list/detail latency, memory and concurrent refresh on representative
workspace sizes against existing behavior; register budgets and test timeout,
missing-evidence and rollback paths. Retain v3 artifacts/readers so rollback is
an explicit version switch, not deletion of historical predictions.

## Non-goals

- **Production changes.** This task writes a design and index only; it dispatches
  no implementation and does not alter scoring, routing, admission or models.
- **Permanent rankings.** No hard-coded Luna/Astra ordering, exponential law,
  or causal capability score from observational regression links.
- **Unobservable guarantees.** No promise of total latent defect counts, complete
  reporting or reliable extrapolation beyond the evidence.
- **Unrelated permissions.** Office permission repair is independent of this work.

## Concerns and honest limitations

- Existing run/claim history, review settlements and landed diff rows support
  partial reconstruction, not necessarily immutable rubric/configuration or
  relation-known-at history. Audit coverage before choosing a model family.
- Reporting and verification practices change with reviewers, tools and time.
  A fixed horizon equalizes opportunity, not detection; severity labels may be
  defaults. Stronger testing can increase observed burden while reducing escapes.
- Small samples can make partial pooling look precise because of a restrictive
  prior. Show prior sensitivity, leave-model-out calibration and pooled support
  separately from direct support; do not hide extrapolation behind shrinkage.
- Higher-dimensional curves cost data and fitting time. Begin with the simplest
  supported comparison; independence between interacting architectural failures
  is not assumed by using line count as a predictor.
- The historical numerical summaries lack a reproducibility chain in the
  inspected records. New receipts may revise the hypothesis, but cannot supply
  missing prospective complexity measurements retrospectively without caveats.

## Phasing

Each slice needs a separately authorized task; these are prerequisites, not a
backlog dispatch. Core contracts remain generic and provider names remain data.

1. **Evidence audit and study manifest.** Inventory run/claim, review, regression
   relation and landed-diff sources against the ledger above. Consume the
   [ORB-20094] receipts when available. Deliver coverage/attrition tables and an
   exact replayable export with hashes; verify sampled joins and identify which
   outcomes can actually be fitted. No numerical claim ships from summaries alone.
2. **Prospective snapshots and labels.** Add immutable assignment rubric,
   execution configuration and event-known-at records where absent. Independently
   verify missing/changed model provenance, retries, multi-model repairs and
   non-landed episodes through public contracts; audit rater disagreement and
   adjudication. Preserve unknowns in old records. Follow repository test admission
   rules; new-feature verification can use recorded API QA without speculative tests.
3. **Offline and shadow comparison.** Fit simple baselines before hierarchical
   alternatives, using the frozen temporal protocol. Publish executable receipts,
   support, uncertainty and gate decisions per stage/estimand. No-ship is a valid
   deliverable. Prerequisites: mature labels and agreed precision/ship thresholds.
4. **Advisory API/UI.** Only after evidence approval, implement additive contracts,
   state handling, bounded caching and accessible disclosures. Verify old/new
   clients, unavailable/stale states, list/detail parity, performance and rollback;
   capture changed views under the workspace screenshot rules. No routing change.
5. **Forecast integration, separately approved.** Validate a joint episode/repair
   simulation without duplicate burden, version forecasts and calibration, and
   shadow whole-backlog outcomes before opt-in rollout. Roll back on calibration,
   support or performance breaches. Any later automatic policy proposal requires
   separate approval and controlled-comparison evidence; it is outside this scope.

## Open questions

1. Which episode deadline/budget and practical calibration/precision margins are
   useful enough to justify a prospective study? They must be chosen before scoring.
2. Can relation history recover first-known times and invalidation decisions at
   sufficient coverage, or must fixed-horizon evaluation begin prospectively?
3. Does a class hierarchy improve held-out cold starts over fleet-only shrinkage
   without imposing a misleading capability ordering?
4. Which repair findings can be adjudicated consistently, and which severity or
   work measures have enough observed support for a separate burden estimate?

## Acceptance (feature level)

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

## Task References

- [ORB-20095] — requests this model-relative task-risk Draft and generated index.
- [ORB-20094] — authorized the authoring-session backtest and owns its receipts.
- [ORB-19361] — introduced the size-only riskiness estimator and recorded prior evidence.
- [ORB-19358] — scoped the symbol-risk index and summarized the historical backtests.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
