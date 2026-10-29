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

## Frozen contracts (versions `decision-assistance-eval/1` and `/2`)

Version `decision-assistance-eval/1` is frozen legacy, preserved for
reading/replay (`validate`, `score`), synthetic labels, `train`/`dev`
splits, `check-targets` and adjudicated holdouts. Version
`decision-assistance-eval/2` is current for human holdout eligibility: it
adds the first labeler's recorded `value` to the import so rewriting
`expected` cannot hide first/second disagreement. `validate`, `score`,
`check-split` and `check-targets` accept both versions; `schema` keeps
`format_version` at `/1` for evidence-envelope compatibility and lists
both under `supported_format_versions`, and `seed` defaults to `/1` with
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

Before a holdout is opened, register a target file (`check-targets`). The file
names the baseline (`baseline.name`: the existing workflow/report without model
assistance) and holds one target entry per question with `min_precision`,
`min_recall` and a positive integer `min_samples`.

`check-targets` enforces that every question has an entry, that `min_samples`
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
- ORB-19760 — scope of the `check-targets` checks; the remaining target
  registration rules are manual steps.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
