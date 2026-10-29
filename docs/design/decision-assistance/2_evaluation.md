---
title: "Evaluation protocol: bounded decision assistance"
owner: codex
last_updated: 2026-10-29
last_validated: 2026-10-29
status: Draft
feature: decision-assistance
doc_role: design
type: design
summary: "Evaluation protocol: versioned contracts, synthetic seed, human-label import/adjudication, holdouts, targets, activation handoff."
tags: [decision-assistance]
---

# Evaluation protocol: bounded decision assistance

Status: proposed; offline only. Nothing here enables a runtime service, live
calls, or advisory display. Implementation:
`scripts/evaluate-decision-assistance.py` (contract + scoring),
`scripts/test-evaluate-decision-assistance.py` (public CLI verification).

## Frozen contracts (version `decision-assistance-eval/1`)

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
  Evidence is untrusted data, never instructions.
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
  credentials, attachments or ambient environment data. Import records
  labeler identity/role, labeling timestamp and the intake snapshot.
- Two independent labelers label each human case blind to model output and to
  each other. Disagreement goes to a third adjudicator who sees both labels
  and the evidence (not later outcomes unavailable at intake); the adjudicated
  label, both originals and rationale category are retained. Unresolvable
  cases are labeled `insufficient_evidence` or excluded with a recorded
  reason — never force-labeled. Author dismissals of advisory flags are
  feedback, not ground truth, until independently re-labeled this way.

## Splitting: family and time holdouts

A split manifest (`check-split`) assigns each case to `train`, `dev` or
`holdout` and is rejected when:

- the same `family_id` (task family or normalized failure family) or subject
  appears on both sides of the holdout boundary;
- a `holdout_id` was previously opened/used (reused holdouts are rejected);
- any evidence `collected_at` is after the manifest `intake_snapshot_at`
  (no leakage from later outcomes);
- a holdout case is not `independent_human` labeled;
- with `split_strategy_time`, the earliest holdout evidence is not after the
  latest train/dev evidence.

## Targets before holdout access

Before a holdout is opened, register a target file (`check-targets`):
per-question `min_precision`/`min_recall` and positive `min_samples` for every
question, plus a named baseline (the existing workflow/report without model
assistance, with its measured per-class errors, abstentions and cost).
Registration timestamp must precede holdout access; the check rejects
late registration. Sample requirements are set per question from the
baseline's class prevalence so rare classes (for example `one_off`) cannot
pass on a handful of cases. Deviations require a new target registration and
a fresh, unopened holdout.

## Baseline comparison and readout

Score the baseline and the candidate on the identical frozen holdout with the
same scorer version. Report per-class precision/recall, coverage/abstention,
false-positive burden (extra investigator/author work per suggestion),
confusion matrices, latency, cost per answered case, and every metric that is
`unknown`. A candidate wins only on the pre-registered targets it meets; a
well-formed answer is not evidence of correctness.

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

- ORB-19720 — implement these evaluation contracts, seed set and scoring.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
