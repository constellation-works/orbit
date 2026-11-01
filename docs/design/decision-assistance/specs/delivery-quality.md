---
title: "Proposal: delivery-quality grader"
owner: codex
last_updated: 2026-11-01
last_validated: 2026-11-01
status: Draft
feature: decision-assistance
doc_role: spec
type: design
summary: "Proposed advisory grading of delivered code across six evidence-backed dimensions, with independent calibration before use."
tags: [decision-assistance, agent-scorecards]
---

# Proposal: delivery-quality grader

Status: **proposed for adoption; no live consumer, grading run or activation is authorized** ([ORB-20000]).
The default-off shadow runtime is in the repository ([ORB-20006]); it is not an authorization to run it.
The [scope](../1_scope.md) remains parked and every existing consumer remains
off by default. This is a new evaluation candidate, distinct from the private
task-authoring quality collector. Its versioned offline questions, contracts and scoring are implemented by the
[evaluation protocol](../2_evaluation.md#delivery-quality-offline-contract-decision-assistance-eval3).
A private runtime consumer and bounded owner evidence builder are implemented.
They remain off without an explicit host assistance policy; no live grant,
paid run, backfill, public display or crew-ladder adoption is authorized.

## Purpose and limits

Completion, review and regression outcomes describe how work turned out.
They do not directly measure whether a change is understandable, follows the
repository's conventions, handles failures well or leaves suitable boundaries.
Add an inspectable quality profile alongside those outcomes.

Grade the delivered change, not the agent's explanation of it. A grade is an
advisory judgment, not proof of correctness or a substitute for review, tests,
security analysis or later regression evidence. Good structure cannot cancel
a demonstrated correctness failure. Low grades do not create defects, change
task state, reject a delivery, adjust crew rankings or route future work.

Start with **implemented code deliveries**. Proposed tasks, pilot assessments,
reviews, documentation-only work and other roles need their own rubrics.
Do not apply the programming rubric to all agent activity or reward a
no-code delivery with an artificial perfect score. Failed or abandoned attempts
retain their existing outcome counts; an unavailable code grade never removes
those failures from a crew's outcome metrics.

## Grading unit, timing and attribution

The host freezes one unit per delivered task candidate, keyed by owner/workspace,
task, implementation attempt, base revision and published/landed revision.
One PR serving several tasks needs a verified task-to-change mapping; if that
mapping is ambiguous, retain a joint change assessment without allocating
individual scores. A merge alone supplies no attribution.

Collect after delivery through an optional asynchronous consumer, outside the
delivery gate. A later fix is a new unit, never a replacement for the original
grade. Retain the published candidate and landed patch identities; if landing
introduced changes, record their provenance and qualify attribution.

Use durable run/provider records for implementer identity, not the task's
current mutable crew. Do not charge reviewer repairs or integration edits to
the original implementer. When contribution boundaries cannot be reconstructed,
mark attribution unavailable and exclude the unit from per-crew comparisons.
A change-level assessment may remain readable with that limitation.

Freeze the evidence available at collection. Later regressions remain separate
outcomes and may be used to evaluate predictive usefulness, never leaked back
into the original input or used to overwrite the original grade.

## Six dimensions and anchored scale

Repository rules and the task's accepted scope take precedence over a generic
style preference. Compare the change with its base and relevant surrounding
contracts. Do not penalize an implementer for inherited defects outside its
scope; do assess a changed path that extends or depends on a broken contract.

| Dimension | Question | Evidence expected |
|---|---|---|
| Scope and contract fit | Does the change implement the requested behavior without unrelated or missing work? | Frozen task/criteria, attributable diff, public contracts and explicit limitations. |
| Robustness | Does it preserve failure, cancellation, cleanup, concurrency and boundary behavior relevant to this change? | Changed control flow, affected callers and observed positive/refusal evidence. |
| Structure | Are responsibilities, interfaces and dependencies placed at suitable existing boundaries? | Relevant module/caller context, architecture rules and dependency changes. |
| Standards and conventions | Does it follow the repository's actual coding, API, error and test conventions? | Applicable versioned guides, neighboring patterns and deterministic lint/guard results. |
| Verification quality | Do checks exercise the changed guarantee and its important failure boundary without weakening coverage? | Test diff, selection, complete results and any missing platform or validation evidence. |
| Clarity and maintenance | Are names, data flow and documentation understandable, with no unnecessary machinery or avoidable duplication? | Attributable code and affected documentation; scope and complexity context. |

Each applicable dimension uses the same ordinal **1–5** vocabulary:

| Grade | Anchor |
|---|---|
| 1 — deficient | Fundamental departures from the dimension's contract; substantial redesign or missing essential work is indicated by the evidence. |
| 2 — weak | Material gaps remain; fixing them requires more than a localized clarification or cleanup. |
| 3 — adequate | Meets the applicable task and repository standard; ordinary competent work with no evidenced material gap. |
| 4 — strong | Handles the relevant difficult cases and boundaries particularly well, with evidence beyond the adequate anchor. |
| 5 — exemplary | A reference-quality solution for this task's constraints, supported by specific evidence; extra size or novelty alone earns nothing. |

Each dimension also permits `not_applicable` and `insufficient_evidence`.
Non-applicability needs a host-reviewed rubric applicability rule, not a model's
convenient excuse to omit a weak dimension. A missing result, refusal, outage,
invalid answer or incomplete packet is never zero, adequate or not applicable.
Confidence is separate from grade and must be calibrated; a precise-looking
probability is not evidence of calibration.

Use narrow predicates or comparisons where labels show that the ordinal
question is unreliable. Do not infer verified defects or severity from a low
grade. The existing finding-verification path still owns those decisions.

## Evidence packet and coverage

The host assembles a bounded immutable packet containing:

- Frozen task scope and criteria; base/candidate/landed identities and
  attributable diff, including deletions and relevant moves.
- The applicable repository instructions and architecture/convention excerpts,
  identified by revision, plus necessary caller and interface context.
- Deterministic validation outcomes with exact source/selection and explicit
  passed, failed, skipped, blocked and not-run states. A test added is not proof
  that it ran, and a green aggregate is not proof of every claimed property.
- Host-selected evidence references, coverage manifest and data provenance.
  Agent summaries are attributed claims, never trusted proof.

Keep implementer/reviewer names, provider/model identity, existing fleet rank,
cost and later outcomes out of the model-facing packet where unnecessary.
The host keeps those fields for attribution and bias evaluation. Comments,
code, guides and logs are untrusted source data, never grader instructions.
Apply the service's host grant, redaction, credential, budget and timeout
boundaries before any external transmission.

A grader cannot browse omitted source. If the required context cannot fit,
abstain or partition into explicitly named, independently complete change units.
Freeze the partition and its obligations before grading; never select only
the easy files. A complete subunit packet does not conceal omissions from a
claimed whole-delivery packet. Preserve the global coverage manifest outside
each subunit and show all ungraded obligations.

A whole-delivery profile is complete only when every required unit/dimension
has admissible evidence under a versioned aggregation policy. Until such a
policy is evaluated, retain subunit results separately and label the delivery
partial. The existing service refuses `suggested` for omitted/truncated
packets; this proposal does not relax that rule.

## Proposed typed result and service fit

Reuse the [service](service.md)'s predicate and string-choice boundary.
For the first prototype, encode each grade as a choice among
`deficient`, `weak`, `adequate`, `strong`, `exemplary`,
`not_applicable` and `insufficient_evidence`; the host maps the first five
to display values 1–5. This does not assume native numeric-score support in
the current Orbit adapter. Any future result-format extension needs its own
compatibility review.

Store unit identity, evidence digest and manifest, rubric/question/builder
versions, grader identity/revision, typed answers and confidence/disposition,
usage, latency and estimated cost with price provenance. Retain provider
refusal, unavailable, invalid, stale and uncertain states distinctly.
Select dimension evidence references in the host-built questions; a model
cannot invent paths, quotations or finding IDs.

The current service persists typed answers, not free-form model explanations
or chain of thought. Keep that contract. Human adjudication may retain its
own concise evidence references and rationale under the evaluation contract.
A future model-generated critique would need a separate scoped data and
validation contract; it is not implied by a grade.

Deduplicate on frozen unit, packet, rubric, grader and policy identity.
Restarts and retries cannot publish duplicate current grades or replace a
newer result. Regrading with a changed rubric/model is a separately versioned
assessment. Do not mix its scores with an earlier calibration cohort.

## Display and comparisons

The first useful output is a per-delivery **quality profile**, with each
dimension's grade, assessed scope, coverage and uncertainty. Show it separately
from the existing [agent scorecard](../../agent-scorecards/2_design.md).
There is **no overall average, 0–100 score, ranking term or routing multiplier**
in the first iteration. Ordinal grades and missing dimensions cannot safely
be combined by silently averaging the available values.

After validation and explicit adoption, a later proposal may show per-crew
distributions with unit counts, missing/partial/unattributed coverage,
rubric/grader versions and comparable task-type/complexity cohorts. Do not
compare a UI implementation directly with an infrastructure repair, reward a
crew for receiving easier work, or count one delivery repeatedly because it
had several commits or grading attempts.

Outcome evidence stays visible beside quality judgments. Do not count an
existing review finding again as an independent quality penalty, hide a real
regression behind a high style grade, or call the grade causal evidence that
one model is better. Any future composite score or crew-selection use needs
a separate product decision and held-out evidence of benefit.

## Independent evaluation and phased adoption

The prior failure/task-quality screens in the scope **did not evaluate this
grader**. Their negative results remain a reason to require evidence, not a
delivery-grader accuracy estimate or permission to restart parked consumers.

1. **Offline rubric and packet development.** Use permitted frozen changes
   across task types and complexity, including adequate simple fixes,
   overengineered changes, inherited poor code, partial/multi-agent deliveries
   and incomplete validation. Record the eligible population and sampling
   method. Synthetic cases exercise schemas, not grader accuracy.
2. **Independent human calibration.** Two qualified humans grade each dimension
   blind to model grades and author identity; a third adjudicates disagreements.
   Preserve disagreement and abstention. A stronger model can be a comparison
   baseline, never a substitute for human ground truth. Fit prompts/anchors on
   development data only.
3. **Fresh held-out evaluation.** Register targets and sample sizes before
   opening a family/time-separated holdout. Measure per-dimension exact and
   adjacent-grade agreement, weighted ordinal agreement against the human
   baseline, confusion matrices, materially over/under-graded cases, coverage
   and abstention, repeatability, latency and cost per useful assessment.
   Predeclare which metrics decide adoption and acceptable uncertainty;
   do not invent universal thresholds or treat a small sample as calibrated.
4. **Bias and evidence controls.** Include irrelevant author/model-label and
   formatting/verbosity perturbations, adversarial instructions in source,
   correct minimal solutions, known deficient changes and changes whose needed
   evidence was removed. For pairwise questions, swap presentation order.
   Measure judge/author-family effects, task-mix differences and whether
   abstention works. Never tune against the unopened holdout's answers.
5. **Private shadow, then optional advisory display.** Only after the required
   evaluation and explicit operator grant, collect a representative sample
   under bounded spend. Count every eligible delivery, including ungraded
   cases; report backlog/budget/outage/size gaps so selective coverage cannot
   improve a crew's apparent quality. Review human usefulness and harmful
   disagreement before enabling a separate advisory surface.

The [evaluation protocol](../2_evaluation.md) supplies additive offline question,
ordinal-scoring, blinded-label, frozen-sampling and coverage tooling. Its synthetic
fixtures exercise mechanics only; existing classifier metrics and frozen formats
are not repurposed and no human accuracy or live adoption is established.
Any live evaluation needs permission for the evidence and paid requests.
None of these phases changes merge/review/admission authority or turns on
the parked service. Source/rubric changes that invalidate calibration require
new validation; preserve prior records and an effective off switch.

## Private runtime capability

The application consumer ([ORB-20006]) is `application::delivery_quality`; composition uses
only host-global private shadow assistance settings; an advisory policy does
not adopt this consumer. First enablement freezes an eligibility start time per
policy epoch: only runs created since that boundary enter the owner scan. No
historical backfill is implied. Owner sweeps reconcile one durable run
page (at most 32 task units) and process at most one service work item per tick.
The cursor cycles through immutable run IDs, including runs that were still live
on an earlier pass. Batched task mappings page independently. Replicas and dry
runs do no work. With no active grant, the source, consumer ledger and adapter
are untouched. Delivery, review, routing, rankings and experiment draws do not
call this consumer or depend on its answer.

`freeze_delivery_quality` freezes metadata and a packet digest without queueing;
`run_delivery_quality_with_source` performs bounded owner admission. The
public `delivery_quality_profile` boundary exposes six independent typed
choices and ordinal values with applicability, gaps, uncertainty, frozen unit
and actual dispatch provenance. There is no aggregate. This first rubric has
no calibrated confidence threshold, so even valid recorded ordinal replies
remain **uncertain**; no production comparison decision is thereby admitted.
The consumer retains bounded metadata receipts in host-global
`state/delivery-quality/v1/<workspace-digest>/`. Raw packets and replies remain
in the existing service store under its bounded retention policy; missing or
expired service evidence is a gap, never a cached grade. Off and changed-policy
reads fence publication. Changed rubric, source version, policy or adapter
creates a separate assessment; a changed immutable packet under the same
identity is refused rather than overwritten.

The durable builder reads host delivery observations, dispatched task-scope
blobs and CLI audit identities, immutable Git trees and exact host validation
artifact references. Dispatch audit events now optionally capture resolved
effort, a fingerprint of the resolved dispatch specification, and a bounded,
redacted task-scope blob. Tier/type refer to that dispatch snapshot. Historical
fields are unavailable; current task crew, experiment mode or provider settings
never repair them. The configuration fingerprint identifies the dispatch
specification, and is distinct from the experiment overlay digest.

The builder preserves deletion/move paths, frozen scope, versioned guides,
selected file/symbol context at base and candidate, and validation selections
and outcomes. Missing objects, binary diffs, oversized guides/context, unresolved
directory selectors, unavailable selections, unverified publication and
unmapped integration contributions remain visible gaps. This version chooses
whole-unit abstention for overflow; it neither truncates a unit into completeness
nor aggregates partition grades. Reviewer/shared contributions cannot support
individual attribution. Documentation-only and verified no-change units are
inapplicable to the programming rubric and retain their ordinary outcome metrics.
Source text, including apparent instructions in code or logs, is evidence only.
Metadata unrelated to the rubric stays outside the model-facing packet.

The `delivery_quality::ladder` read port uses the offline
`delivery-quality-join/1` field set. Its host-grant-bearing receipt is separately
versioned `delivery-quality-producer-sample/1`, so it cannot be mistaken for
an offline evaluator `delivery-quality-sample/1` envelope. Both use the same
outcome-independent `sha256-sort/1` selection keys.
Stored assignments must match the run capture's experiment/arm/segment/digest
and the actual provider/model/effort/configuration treatment. Hypothetical
`live=false`, forced, explicit-crew, surface-conflict, unavailable and
mismatched records retain separate exclusions. A later ladder consumer must
supply separately recorded use-specific adoption and evaluated version identities;
a generic grader policy is insufficient. The existing phase-1 `shadow` and
`guardrail` delivery-quality refusals, and `propose`/`auto` mode refusals,
remain unchanged.

Trial frames freeze all enrolled identities before grading, then select every
eligible candidate and a same-size seeded control sample within matching
strata. `sha256-sort/1` uses the seed, cohort, epoch, eligibility watermark and
task key; the immutable receipt retains probabilities, selected IDs and control
shortfall. Retrying a cohort cannot redraw it. Growth requires a new cohort
epoch; previous frames remain readable. `run_delivery_quality_sample` admits
only selected, frame-bound assessments, and the read-only comparison snapshot
revalidates the private shadow policy before and after reading, then exports
enrolled/selected/unsampled counts plus applicable, valid-grade,
weak-or-deficient and explicit gap counts per arm and dimension. Mixed grader,
source or actual-treatment epochs are refused; duplicate tasks and shared
change units never gain extra votes. There are no interval decisions or
promotion/demotion actions.

Run the isolated manual fixture with a fresh directory:

```sh
scripts/build-budget.py -- cargo run -p orbit-core --example delivery_quality_fixture -- .orbit/tmp/delivery-quality-fixture
```

It uses real local Git changes, isolated stored experiment assignments and
recorded adapters, with no credentials, live requests or human labels. Its
full-valid comparison data are explicitly synthetic mechanics, separate from
the uncalibrated service replies. The 10-candidate/90-control frame selects
10 controls: fully valid synthetic grades give 100% sample coverage per arm,
with 80 unsampled controls retained separately. This establishes plumbing and
accounting only, never grading accuracy or adoption.

## Task References

- [ORB-20006] — implements the default-off delivery-quality shadow runtime.
- [ORB-20000] — authors this proposal and its parent-design links; opens a review PR.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
