---
title: "Evaluation protocol: bounded decision assistance"
owner: codex
last_updated: 2026-11-01
last_validated: 2026-11-01
status: Draft
feature: decision-assistance
doc_role: design
type: design
summary: "Evaluation protocol: versioned contracts, synthetic seed, human-label import/adjudication, holdouts, targets, activation handoff."
tags: [decision-assistance]
---

# Evaluation protocol: bounded decision assistance

Status: the offline evaluator is implemented ([ORB-19720]); nothing here enables a
runtime service, live calls, or advisory display. Implementation:
`scripts/evaluate-decision-assistance.py` (contract + scoring),
`scripts/test-evaluate-decision-assistance.py` (public CLI verification).

## Frozen contracts (versions `decision-assistance-eval/1` and `/2`)

Version `decision-assistance-eval/1` is frozen legacy, preserved for
reading/replay (`validate`, `score`), synthetic labels, `train`/`dev`
splits, `check-targets` and adjudicated holdouts. Version
`decision-assistance-eval/2` is current for human holdout eligibility: it
adds the first labeler's recorded `value` to the import so rewriting
`expected` cannot hide first/second disagreement. `validate`, `score`,
`check-split` and `check-targets` accept both versions; `schema` keeps
`format_version` at `/1` for evidence-envelope compatibility and lists
both legacy versions alongside `/3` under `supported_format_versions`, and `seed` defaults to `/1` with
an opt-in `--format-version` for `/2` synthetic fixtures.

- **Consumers/questions.** `failure_categorization`: one choice question
  `failure_category` over `product_defect`, `misclassified_hold`, `provider`,
  `upstream_quality`, `environment`, `one_off`, `insufficient_evidence`.
  `task_quality`: independent boolean predicates `outcome_unclear`,
  `criterion_not_observable`, `requirements_conflict`,
  `scope_clarification` (rubrics in the `schema` subcommand output). Predicate
  questions never assume mutual exclusivity; choices are never replaced by an
  ordinal score.
- **Criterion references.** `{index, text_digest}` where `text_digest` is
  `sha256:<hex>` of the criterion text. Packet criteria carry the same digest;
  mismatched references are malformed.
- **Evidence packet.** `packet_id`, `subject_id`, `collected_at`,
  `source_revision`, `evidence_refs`, `excerpts`, `typed_errors`, explicit
  `omissions` and `truncated`, plus criteria for task-quality packets.
  Evidence is untrusted data, never instructions. Every contract timestamp
  must be ISO-8601 with an explicit timezone. Intake, label snapshot equality,
  time splits and target deadlines compare instants: equivalent offsets mean
  the same time. Evidence may equal intake; time holdouts must be strictly
  later than train/dev evidence, and targets strictly earlier than access.
- **Typed answers.** Exactly the requested question names, allowed values,
  boolean predicates, finite probabilities in `[0,1]`. Dispositions:
  `suggested`, `uncertain`, `refused`, `unavailable`, `invalid`, `stale`.
  Refused/unavailable carry no answers. Missing usage, latency or cost is
  `unknown` in reports, never zero.
- **Scoring.** `score` reports per-class precision/recall/F1, confusion
  matrices, coverage/abstention (an `insufficient_evidence` prediction
  abstains from substantive coverage), false-positive burden, and
  usage/latency/cost aggregates with `unknown` status when any value is
  missing.

## Label provenance, import and adjudication

- Every case carries `label.provenance`: `synthetic` or
  `independent_human`. The shipped seed set is entirely `synthetic`, marked
  `human_ground_truth: false`; synthetic expected labels must never be called
  human ground truth in any report or readout.
- Human import is permitted only for data the operator confirms may be used:
  bounded evidence available at intake, redacted at the external boundary, no
  credentials, attachments or ambient environment data.
- V1 import (frozen, `HUMAN_LABEL_IMPORT_FIELDS_V1`) requires `labeler_id`,
  `labeler_role`, `labeled_at`, `intake_snapshot_at` and
  `second_label_or_adjudication`. A v1 `second_independent_label` has no
  recorded first value, so independent agreement cannot be verified from
  `expected`: it is legacy/unverified, never proven independent human ground
  truth, and `check-split` rejects it for holdout eligibility with a
  re-authoring error. V1 `adjudication` records remain eligible when they
  retain both `original_labels`, a third adjudicator and a matching accepted
  value, because the disagreement is explicitly recorded.
- V2 import (current, `HUMAN_LABEL_IMPORT_FIELDS_V2`) adds the first
  labeler's recorded `value`. `check-split` enforces it only on `/2`
  datasets. A v2 `second_independent_label` retains its `labeler_id`,
  `labeler_role`, `labeled_at` and `value`; its labeler must differ from the
  first, its `value` must match the imported first `value`, and changing
  `expected` cannot make disagreeing labels agree. Two independent labelers
  label each human case blind to model output and to each other. When they
  disagree, the record must be an adjudication with `adjudicator_id`,
  `adjudicator_role`, `adjudicated_at`, both `original_labels` (each
  retaining labeler identity, role, timestamp and value) and a
  `rationale_category`; the first original's value must match the imported
  first `value`, and the adjudicator must be a third person. The adjudicator
  sees both original labels and the evidence available at intake, not later
  outcomes.
  The adjudicated `value` is the accepted label. Unresolvable
  cases are labeled `insufficient_evidence` or excluded with a recorded
  reason — never force-labeled. Author dismissals of advisory flags are
  feedback, not ground truth, until independently re-labeled this way.
- Migration v1 → v2 (supported re-authoring path). For each v1
  second-label holdout case: retrieve the first labeler's independently
  recorded value from the original labeling records, add it as
  `label.value`, keep the recorded second `value`, set the dataset and
  manifest `format_version` to `decision-assistance-eval/2`, and re-run
  `check-split` with a fresh `holdout_id`. If the original first value is
  unavailable it cannot be reconstructed: never infer it from `expected`
  (which may have been overwritten to the second value), never grandfather
  the v1 record as ground truth. Re-label blind or exclude the case;
  `check-split` fails safely until the v2 record verifies.

## Splitting: family and time holdouts

A split manifest (`check-split`) assigns each case to `train`, `dev` or
`holdout` and is rejected when:

- a dataset case has no entry in `assignments` (a missing, empty or partial map);
- the same `family_id` (task family or normalized failure family) or subject
  appears on both sides of the holdout boundary;
- a `holdout_id` was previously opened/used (reused holdouts are rejected);
- the manifest `intake_snapshot_at` is missing or not ISO-8601 (the leakage
  bound needs a snapshot to compare against);
- any evidence `collected_at` is after the manifest `intake_snapshot_at`
  (no leakage from later outcomes);
- a holdout case is not `independent_human` labeled;
- with `split_strategy_time`, the earliest holdout evidence is not after the
  latest train/dev evidence.

Run all split and target checks for an evaluation from the same working
directory. A successful `check-split` **opens** its `holdout_id`: it writes
the ID and current UTC `opened_at` to
`.decision-assistance-eval/holdouts/<sha256-of-holdout-id>.json` beneath that
directory. Every later check of that ID is refused, including
a re-check of the identical manifest or a copy at another input path. Invalid
manifests do not open a holdout. Records are created exclusively, so
concurrent split checks cannot both accept the same ID; a write failure makes
the check fail and may leave a reservation that later checks refuse. Preserve
these records with the evaluation evidence. History is local to this
directory; another directory or machine must retain the same
records to enforce the same history. The optional `--prior-holdout-ids` and
manifest `previously_opened_holdout_ids` remain additional rejection lists.

## Targets before holdout access

Before a holdout is opened, write a target file and validate it with `check-targets`.
The command records nothing, so keep the file as the registration record. The file
names the baseline (`baseline.name`: the existing workflow/report without model
assistance) and holds one target entry per question with `min_precision`,
`min_recall` and a positive integer `min_samples`.

`check-targets` ([ORB-19760] scopes which of these checks the command enforces)
enforces that every question has an entry, that `min_samples`
is a positive integer, that `min_precision`/`min_recall` lie in `[0,1]` when
present, and that `baseline.name` is non-empty. A target file that is unreadable,
is not valid JSON, or has the wrong shape (a non-object document or `baseline`, or
a `targets` value that is not an array of objects with string `question` fields)
exits 2 with the same `{ok, errors}` report and no traceback. It reads the recorded opening
time automatically and rejects `registered_at` at or after holdout access.
Opening and registration times are compared as ISO 8601 instants with explicit
timezones. A target file can name its `holdout_id`; before that ID has been
opened, registration is allowed. Without an ID, the sole recorded holdout is
used; if several holdouts have been opened, the target file must name one.
Before any holdout has opened, targets can be checked without an ID.
Unreadable or malformed records fail the check. The optional
`--holdout-opened-at` adds a deadline for externally recorded access; a later
supplied time cannot override an earlier recorded opening.

Manual steps. No command checks these; the operator records them with the
registration and the readout:

- **Threshold presence.** Confirm each entry has both `min_precision` and
  `min_recall`; `check-targets` does not require them.
- **Sample sizing.** Set each question's `min_samples` from the baseline's class
  prevalence so rare classes (for example `one_off`) are not judged on a
  handful of cases. `check-targets` only checks that the value is a positive
  integer.
- **Baseline measurements.** Record the baseline's measured per-class errors,
  abstentions and cost with the registration. The target file does not carry
  them and `check-targets` does not validate them.
- **Deviations.** A deviation from the registered targets requires a new target
  registration and a fresh, unopened holdout.

## Baseline comparison and readout

Score the baseline and the candidate on the identical frozen holdout with the
same scorer version. Report per-class precision/recall, coverage/abstention,
false-positive burden (extra investigator/author work per suggestion),
confusion matrices, latency, cost per answered case, and every metric that is
`unknown`. A candidate wins only on the pre-registered targets it meets; this
comparison is manual, because `score` reports metrics and does not read the
target file. A well-formed answer is not evidence of correctness.

## Delivery-quality offline contract (`decision-assistance-eval/3`)

This additive offline family ([ORB-20004]) implements the [delivery-quality proposal](specs/delivery-quality.md)
([ORB-20000]).
It remains **proposed for adoption**. It does not extend the native service's numeric
answer types, change a runtime consumer, activate paid calls, or authorize a ladder
routing action. `/1` and `/2` retain their original consumers, validation and scoring;
`schema` still emits `/1` at the top level and advertises `/3` separately under
`delivery_quality`. `delivery-schema` emits just the new contract. The CLI dispatches
`/3` to the stdlib-only `delivery_quality_eval.py`, `delivery_quality_sampling.py`,
`delivery_quality_study.py` and `delivery_quality_seed.py` modules beside the existing
entrypoint. Existing classifier public checks remain the admitted regression suite;
the following seed commands are manual mechanics QA, not a new accuracy test suite.

### Questions, packets and answers

`delivery-quality-questions/1` and `delivery-quality-rubric/1` freeze six independent
string-choice dimensions: `scope_contract_fit`, `robustness`, `structure`,
`standards_conventions`, `verification_quality`, `clarity_maintenance`. `delivery-schema`
exports each question and all five anchors. Choices are `deficient`, `weak`, `adequate`,
`strong`, `exemplary`, `not_applicable`, `insufficient_evidence`. Only the first five map
to ordinal positions 1–5 inside this scorer. Gate choices are separately declared as
`deficient`/`weak`. Grade, optional `probability` confidence, service `disposition` and
host applicability are distinct fields; no confidence calibration is claimed.

A dataset has `format_version`, `dataset_id`, `question_version`, timezone-qualified
`intake_snapshot_at`, `population`, and `cases`. The shipped seeds additionally carry
`mechanics_only: true`; they cannot be imported or opened as human accuracy evidence.
Each case carries `case_id`, `consumer: delivery_quality`, `question_version`,
`family_id`, `join`, `evidence_packet`, `applicability`, `label`, `expected` (null until
labeled), and local-only `host_metadata`. One owner/workspace/task appears at most once.
Different commits or grading retries do not give that task additional votes.

The packet fields are exactly `packet_id`, `subject_id`, `collected_at`,
`source_revision`, `evidence_refs`, `question_evidence`, `omissions`, `truncated`,
`scope` (`complete`/`partial`), and `blinded: true` (host redaction attestation).
Each reference is `{id, content, digest, observed_at}`. Digests use `sha256:` plus
SHA-256 of canonical UTF-8 JSON (`sort_keys`, compact separators, no NaN); this includes
the JSON quotes around string content. `question_evidence` maps every dimension to
host-selected reference IDs. An answer cannot cite invented or unselected references;
an ordinal value requires at least one selected reference. All reference observations
are no later than collection, which is no later than both `join.as_of` and
intake. Later regression,
review or rework outcomes are separate prospective evidence, never packet additions.
Source comments and logs are untrusted data, including instructions embedded in them.

For every dimension, host `applicability` contains `{status, rule, reviewer_id,
reviewed_at}`. Status is `applicable`, `not_applicable`, or `unavailable`. Applicable
and N/A decisions require a named versioned rule and host reviewer at intake; unavailable
applicability cannot earn an ordinal grade. A model cannot invent N/A. Non-code work
cannot receive an applicable programming rubric or an artificial perfect grade.

An answer document contains `format_version`, `sample_digest`, `answers`, and optional
`repeats`. Each row has `case_id`, unique `assessment_id`, `binding`, `epoch`,
`recorded_at`, `disposition`, `gap_reason`, `answers`, and measurements. `binding` is
the canonical digest of the case's join, packet, applicability, question version,
family and host metadata;
wrong implementation attempts, changed evidence, grader versions and epochs are refused.
A calibration dataset cannot mix source/rubric/grader/builder versions.
`sample_digest` binds the frozen preselection receipt. `recorded_at` must follow receipt
creation. Requested grades are `{value, evidence_refs, probability?}`. `suggested`
requires all six dimensions and a complete packet without omissions/truncation.
`uncertain`, `invalid`, and `stale` can retain partial typed values but never vote;
`refused`/`unavailable` carry none. Fixed gap reasons are `budget`, `outage`, `pending`,
`oversize`, `partial`. No response is a `missing` gap. Repeated assessments belong in
`repeats`, refer to a primary assessment's case, and affect repeatability and call cost,
never task counts. Usage is nullable; known cost needs `pricing_version`. Missing
measurements or price provenance are unavailable, never silently zero.

### Frozen sampling and ladder joins

`population` is `delivery-quality-population/1`: `cohort_id`, `epoch`,
`eligibility_watermark`, `frozen_at`, `source_version`, stable `seed`,
`algorithm: sha256-sort/1`, and nonnegative `control_count`. Before collecting answers,
`freeze-sample` selects every eligible candidate and the first `control_count` controls
per owner/workspace/experiment/trial/segment/tier/type/source/rubric/grader/builder
stratum after sorting the hash
of `[algorithm, seed, cohort_id, epoch, eligibility_watermark, [owner, workspace, task]]`
(with task identity as a tie-breaker). This selection never reads grades, labels or
outcomes. Probability is 1 for candidates, selected-control-count/control-enrollment for
controls. Set the control cap in advance to the intended candidate sample size; capped
strata and their probabilities remain visible. No later response can redraw controls.

The exclusive `--output` receipt (`delivery-quality-sample/1`) retains the population,
every task/case, binding, cohort flags, eligibility, selection, probability, frame digest
and actual creation time. Retry with identical inputs returns the existing receipt;
changed epochs, attempts, evidence, watermarks or rosters are refused. Preserve this
receipt on restart and across hosts, and before answers. It is a host-produced record,
not a cryptographic proof that an offline file's author told the truth about timing.

`delivery-quality-join/1` is an additive export from `ladder-export` (also included in
`score`). Every row retains **all** `join` fields, with old missing capture data explicitly
null: `owner`, `workspace`, `experiment_id`, `trial_id`, `assignment_arm`,
`segment_digest`, `overlay_digest`, `excluded`, `forced`, `live`, `task_id`,
`implementation_attempt`, `change_unit`, `admission_tier`, `admission_type`,
`actual_run_id`, `actual_provider`, `actual_model`, `actual_effort`,
`configuration_fingerprint`, `source_version`, `rubric_version`, `grader_version`,
`builder_version`, `as_of`, `attribution`, `non_code`.
`change_unit` contains immutable `base_revision`, `candidate_revision`, optional
`landed_revision`, and `patch_digest`; an unavailable unit is null. Available attribution
requires the actual implementation attempt/run/provider/model/effort/fingerprint and
unit. Attribution can instead be `shared`, `partial`, `unavailable`. A repeated shared
unit is unattributed for individual task counts even if its input rows claim otherwise.
Keep joint assessments readable; never allocate the same grade to multiple tasks.

`overlay_digest` identifies assignment policy and remains distinct from the configuration
fingerprint of the crew that **actually executed**. A shadow assignment is never evidence
of candidate execution. `live: false` is hypothetical, null is uncaptured; neither enters
live coverage. Excluded/forced/non-code flags also admit null for uncaptured old data;
eligibility requires explicitly false flags, never an assumption from null. Forced, excluded, hypothetical/uncaptured and non-code flag combinations
remain separate cohorts, with full enrollment counts. Missing current capture fields
are never recovered from today's mutable configuration.

Per comparable arm/cohort and dimension, the export shows enrolled, eligible,
preselected, unsampled, assessment-recorded and assessed counts. Assessed means a
recorded typed dimension value, including abstention; valid ordinal is a separate count. Eligibility requires live capture, no forced/excluded/non-code flag, candidate or
control arm, and captured experiment/trial/segment/overlay. For each dimension:

- Coverage denominator = **frozen preselected eligible tasks minus host-approved N/A
  tasks with available task attribution and captured admission tier/type**. Unknown
  applicability, partial/unattributed units, missing old admission data and all selected
  operational gaps remain in the denominator. N/A from an unapproved model answer is
  refused. Non-code and otherwise ineligible cohorts remain population records outside
  this programming-sample denominator.
- Coverage numerator = selected, attributable, completely evidenced, applicable
  `suggested` ordinal results. Repeats add no votes. A useful grade cannot hide missing
  required evidence or missing dimensions. Each dimension's denominator is independent.
- Bad-grade numerator = valid `deficient` + `weak`; bad-grade denominator = valid ordinal
  results for that dimension. Counts support a separately adopted guardrail's proportion
  and interval calculation, without averaging dimensions or embedding a routing action.
- Every gap is explicit: missing, partial, unattributed, capture/applicability unavailable,
  budget, outage, pending, oversize, refusal, unavailable, invalid, stale, uncertain,
  insufficient evidence. Approved N/A, ineligible and unsampled remain separate counts.
  Exclusive coverage states prioritize unavailable attribution/capture, host applicability,
  then response/packet state; rows also retain the original assessment disposition,
  gap reason and typed answers so simultaneous limitations are inspectable.

`min_grade_coverage` and the inter-arm coverage-gap check use that per-dimension
**preselected** denominator. Example: 10 enrolled candidates and 90 enrolled controls;
all candidates and 10 seeded controls selected and valid => candidate coverage 10/10,
control coverage 10/10, both **100%**. Controls still show enrollment 90, selected 10,
unsampled 80. This is not 11% failed coverage. Removing one selected control result makes
control coverage 9/10 = 90%; it leaves 80 unsampled controls visible. Full-enrollment
sampling rates and applicability rules remain visible beside sample coverage. Selection
bias, different sampling probabilities and missingness still limit population inference.
The current ladder implementation only admits off/shadow ladder modes and refuses
`delivery_quality=shadow` and `guardrail`; this offline export does not implement or
authorize that later integration.

### Blinded labels and fresh holdouts

`export-labels` emits `delivery-quality-blind/1`: opaque `blind_id`, bounded packet
without task/packet identity, dimension applicability rules, the rubric, intake and
sample/export digests. Join, expected answers, human labels, author/model attribution,
family and control labels stay local. `host_metadata.author_ids`, author/grader model
families and perturbation pair IDs remain available only for attribution and host bias
analysis. The host must redact identifiers embedded in source/reference content before
attesting `blinded`; dropping metadata keys alone cannot redact arbitrary source text.
Give independent labelers separate copies; do not show model answers or the other's label.

`import-labels` accepts `delivery-quality-labels/1`, matching export/sample digests and
`records: [{blind_id, label}]`. A human `label` has `provenance: independent_human`,
`human_ground_truth: true`, `intake_snapshot_at`, two `independent_labels`, and nullable
`adjudication`. Each original has `labeler_id`, qualification `role`, `kind: human`,
`qualified: true`, `independent: true`, `blinded: true`, `labeled_at`, and six typed
`values`. Labelers must differ, must not be change authors, and cannot be models.
Qualifications, independence and permitted evidence use are host attestations, not
facts this offline tool can discover. Both originals remain intact even when they
abstain or disagree. Agreement becomes expected; unresolved disagreement becomes
`insufficient_evidence` for that dimension. Optional adjudication uses the same fields,
a third human identity, `rationale_category` and a timestamp after both originals; its
values become expected. Existing independent labels cannot be overwritten through import.
A new study/version is required to relabel. Synthetic imports use `provenance: synthetic`,
`human_ground_truth: false`, `kind: fixture_person`, and retain that status permanently.
They can exercise adjudication mechanics but cannot become human ground truth.

Before access, `register-targets` exclusively persists `delivery-quality-targets/1`
with `holdout_id`, `purpose: human_evaluation`, actual registration receipt time,
declared `registered_at`, named baseline plus explicit measurements/unknowns, and frozen
study identity (question/rubric/grader/builder/source versions, cohort/epoch/watermark,
sample frame digest). Each of the six targets requires positive `min_samples`,
`min_exact`, `min_adjacent`, `min_weighted_agreement`, `max_material_overgrading`,
`max_material_undergrading`, `min_coverage`, `max_abstention`,
`max_human_disagreement`, `min_repeatability`. Rates are finite in [0,1]. Predeclare
`uncertainty.method: wilson95+family-bootstrap95/1` and `max_interval_width`.
Declare all controls listed by `delivery-schema` and `comparison: absolute_ordinal`.
No pairwise comparison is implemented; any later pairwise protocol must predeclare
presentation-order swaps. These thresholds have no universal defaults and no routing effect.

`check-targets` checks without registering; opening requires the stored registration.
It never overwrites an existing target digest. `check-split` requires
`delivery-quality-split/1`, matching purpose/holdout/target digest/intake, full `sample`
receipt and `assignments` for every case (`train`, `dev`, `holdout`), with nonempty
holdout and development populations and `split_strategy: family_and_time/1`.
Family, subject and task identities cannot cross the holdout boundary; earliest
holdout evidence must be strictly later than the latest development evidence. Human
holdouts require independent human labels. Reused IDs **or identities under a new ID**
are refused. A human holdout ID already opened in the legacy `/1` or `/2` ledger is
also refused; changing contract versions cannot refresh it. Future evidence and changed source/grader/epoch/frame identities are refused.
Successful opening creates an exclusive receipt, bound to dataset/manifest/target digests,
and never rolls it back. Rechecking the same opening is refused. Target changes or
post-opening registrations fail even with a backdated declared timestamp. The opening
reports actual per-dimension labeled sample sizes and whether the registered sample-size
requirements are met; opening alone does not certify sufficient samples or adoption.

Receipts live under `.decision-assistance-eval/delivery/<purpose>/{targets,openings}/`
in the evaluation working directory. Preserve the whole ledger across retries and hosts;
run study commands in one directory. Opening checks serialize across IDs with Unix
`flock`; local filesystem owners can delete files, so this is not tamper-proof archival
storage or permission to copy a holdout into a fresh directory. Keep host-controlled
access history with the human study. `purpose: synthetic_mechanics` has a disjoint
receipt namespace, requires `mechanics_only: true` and synthetic labels, and always
returns `accuracy_eligible: false`. It exercises irreversible opening/reuse mechanics
without weakening human-holdout admission.

### Ordinal readout and uncertainty

`delivery-quality-scorer/1` emits no composite or fleet ranking. Each dimension reports:

- Exact and adjacent (absolute ordinal distance <= 1) agreement, a full 5x5 confusion
  matrix (rows expected, columns predicted), material over/under-grading (distance >= 2),
  counts and 95% Wilson intervals for these proportions. Only paired valid ordinal
  predictions and ordinal accepted labels enter agreement; every missing/abstained case
  remains in coverage. Zero observations produce null metrics, never perfect agreement.
- Quadratic weighted agreement `1 - mean(((predicted-human)/4)^2)` and quadratic weighted
  Cohen kappa `1 - observed_disagreement/independent_marginal_disagreement`. Kappa is
  unknown when marginal disagreement is zero. Intervals use 1,000 family-cluster
  percentile bootstrap draws with seed 0. Fewer than two families, or fewer than 950
  defined bootstrap estimates, leave the interval explicitly unavailable. Wilson
  intervals are descriptive task proportions; clustering and a small or biased cohort
  limit inference. No claimed calibration follows from an interval alone.
- Coverage and abstention on the selected dimension frame, retained human disagreement
  and abstention (including N/A and insufficient evidence), human inter-rater ordinal
  agreement, adjudication counts, and repeatability against primary recorded answers.
  Independent repeats never increase enrollment, coverage, or sample size.
- Call latency and token/cost totals/means, with unknown status if any call lacks a
  measurement. Known zero is distinct from missing pricing/usage. Cost per useful
  assessment divides **all** call costs, including retries/repeats, by unique selected
  attributable tasks with at least one valid applicable dimension. Per-dimension useful
  cost instead divides that same spend by that dimension's valid task count; these
  six costs must never be added or averaged. Missing spend or no useful result yields
  null. This is a cost denominator, not a quality composite.
- Host-only author/grader model-family strata and paired perturbation stability, without
  ranking families or asserting a causal model effect. Unknown family attribution stays
  null; it is never inferred from current settings. Control fixtures cover minimal good
  work, deficient changes, overengineering, inherited poor code, shared/partial
  attribution, missing validation, author/model labels, verbosity, formatting, source
  prompt injection and evidence removal. Synthetic controls show mechanics, not judge
  robustness or human accuracy.

For adoption, qualified humans and the operator must still supply permitted frozen
real changes, a representative population and power/sample-size rationale, qualification
and independence attestations, blind dual labels plus adjudication, measured baseline,
a fresh held-out readout against the predeclared targets/uncertainty, bias controls,
repeat runs, and real usage/price provenance. Fit prompts only on development data.
`score` does not decide whether targets authorize adoption; that remains a separate
recorded operator decision, as do any live study, advisory surface or ladder guardrail.

### Reproducible public CLI fixtures

Run from a fresh directory beneath `.orbit/tmp/` so opening records stay in scratch.
Set `ROOT` to this checkout before changing directory. No network or model is used.

```bash
ROOT="$PWD"
mkdir -p .orbit/tmp/delivery-demo
cd .orbit/tmp/delivery-demo
python3 "$ROOT/scripts/evaluate-decision-assistance.py" delivery-schema > schema.json
python3 "$ROOT/scripts/evaluate-decision-assistance.py" seed --format-version decision-assistance-eval/3 > dataset.json
python3 "$ROOT/scripts/evaluate-decision-assistance.py" freeze-sample --dataset dataset.json --output sample.json > freeze.json
python3 "$ROOT/scripts/evaluate-decision-assistance.py" seed --format-version decision-assistance-eval/3 --answers --dataset dataset.json --sample sample.json > answers.json
python3 "$ROOT/scripts/evaluate-decision-assistance.py" validate --dataset dataset.json --answers answers.json --sample sample.json
python3 "$ROOT/scripts/evaluate-decision-assistance.py" score --dataset dataset.json --answers answers.json --sample sample.json > report.json
python3 "$ROOT/scripts/evaluate-decision-assistance.py" ladder-export --dataset dataset.json --answers answers.json --sample sample.json > join.json
python3 "$ROOT/scripts/evaluate-decision-assistance.py" export-labels --dataset dataset.json --sample sample.json > blind.json
```

The default `ladder` seed is exactly 10 candidates and 90 controls. Repeat
`freeze-sample` to verify stable selections/receipt. Delete one selected answer in a
copy of `answers.json` to observe 90% coverage in that arm. Mutate a choice, evidence
reference, epoch or implementation attempt in copies to observe exit 2 refusals.
`seed --format-version decision-assistance-eval/3 --scenario controls` supplies the
recorded minimal/overengineered/inherited,
adversarial, operational-gap and cohort fixtures; generate its frame and answers with
the same commands. Its deliberately wrong recorded answers demonstrate material errors,
not measured grader behavior. All its unknown latency/usage/pricing remain unknown.

For label/split mechanics use
`seed --format-version decision-assistance-eval/3 --scenario study`, freeze its frame, then:

```bash
python3 "$ROOT/scripts/evaluate-decision-assistance.py" study-seed --dataset study.json --sample study-sample.json --output-dir inputs > inputs.json
python3 "$ROOT/scripts/evaluate-decision-assistance.py" import-labels --dataset study.json --sample study-sample.json --labels inputs/labels.json > labeled.json
python3 "$ROOT/scripts/evaluate-decision-assistance.py" register-targets --targets inputs/targets.json
python3 "$ROOT/scripts/evaluate-decision-assistance.py" check-split --dataset labeled.json --manifest inputs/split.json
```

The synthetic study preserves unresolved disagreement and a third-person adjudication.
Reopening is refused; registering modified/backdated targets after opening is refused.
In copies, duplicate a labeler or use an original as adjudicator, substitute a model or
self-label, overlap a family, put development evidence after holdout evidence, inject
future evidence, or change the manifest to `human_evaluation`: each must be refused.
A separate fresh working directory is for an independent fixture run, never a workaround
for an opened human holdout. Fixture targets intentionally exceed fixture sample size;
the opening reports unmet sizes and no accuracy eligibility.

## Live activation and budget handoff (not authorized here)

Live evaluation or advisory activation requires a separate operator decision
that names: consumer and question/policy versions, mode (`shadow` before
`advisory`), workspace scope, evidence destination and redaction owner,
per-run and total budget caps with a pricing version, timeout/retry limits,
the targets met on the fresh holdout, and the off-switch owner. Paid or live
API evaluation is not authorized by this protocol or its filing. The real
historical human-labeled corpus and live quality/cost readout are later
operator-coordinated evidence, not prerequisites for this offline delivery.

## Task References

- [ORB-20004] — adds the offline ordinal evaluation contract for delivery-quality grades.
- [ORB-20000] — proposes separate delivery-quality questions and evaluation; no evaluator changes in that task.
- [ORB-19760] — scopes which `check-targets` checks the command enforces; the remaining target registration rules are manual steps.
- [ORB-19720] — implements these evaluation contracts, seed set and scoring.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
