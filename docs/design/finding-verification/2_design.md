---
title: Finding Verification — Design
owner: claude
last_updated: 2026-10-29
last_validated: 2026-10-29
status: Accepted
feature: finding-verification
doc_role: design
type: design
summary: How review findings are rejected by the task pilot or the implementer by default, and how the opt-in finding_verification_pipeline selects them, draws verifier crews from the complexity pools, validates and records verdicts, escalates doubtful ones, and gates promotion and drain admission.
tags: [finding-verification, review, automation, admission]
paths: ["crates/orbit-core/src/application/task/finding_verification.rs", "crates/orbit-core/src/application/task/finding_verification_steps/**", "crates/orbit-core/assets/jobs/finding_verification_pipeline.yaml"]
related_features: [automation-triggers, review-gate, distributed-drain, routines]
related_artifacts: [ORB-15662]
---

# Finding Verification — Design

This doc covers the current implementation: which tasks are findings, the
job, the verdict schema and how it is recorded, crew selection and
escalation, the gate, the state trigger, and the default rejection routes
(§8). With the job and its routine off, the task pilot and the implementer
are how a wrong finding gets rejected; the job is the opt-in alternative.
Why each exists is in [4_decisions.md](4_decisions.md).

## 1. Review findings

`is_review_finding` is true for a task of type `bug` carrying one of
`REVIEW_FINDING_TAGS`: `code-review`, `delivery-code-review`,
`security-review`, `qa-sweep`. A finding is verified, and held, only in
`proposed`, `someday` or `backlog` (`VERIFIABLE_STATUSES`). A finding needs
verification there until its latest verification record is CONFIRMED and its
`finding_fingerprint` matches the current task material (§7).

## 2. The job

`finding_verification_pipeline` has three steps [ORB-15662]:

1. **`prepare_finding_verification`** (deterministic). It pins one
   landing-branch revision through the same source snapshot task-pilot uses,
   and fails before any agent call outside a Git work tree. It selects the
   tasks:
   - **Explicit** (`task_ids`): verifies the named tasks, and re-verifies a
     confirmed one on request. A named task that is not a verifiable review
     finding, or that another live verification run holds, settles
     `superseded` with its reason.
   - **Automatic** (no `task_ids`): every finding still owed a verdict, oldest
     first, up to `max_tasks`. The rest are counted in `excluded_by_reason`.

   It draws each task's verifier (§4) and partitions the tasks by crew, at
   most five per partition. Nothing is written.
2. **`verify_finding`** (agent, `fsProfile: reviewer`). One worker runs per
   partition, with that partition's crew, at `inspection_revision`. It
   returns one verifier report per task. It has no task-update, lifecycle,
   dispatch or repository-write grant. Its explicit `tool_disallow_list`
   withholds mutation and dispatch tools [ORB-16713], and the reviewer filesystem profile
   keeps repository inspection read-only. Shipped activities declare their
   tool policy explicitly; an empty policy in a custom activity still warns
   when loaded.
3. **`apply_finding_verification`** (deterministic). It validates each report
   (§3), then either escalates it (§4) or records it (§5). A second
   `verify_finding` fan-out and a second apply run only when there are
   escalations. A task left without a valid verdict fails the run through
   `pipeline_success_guard`.

## 3. Verdict schema

A verifier returns one **report** per task:

| Field | Type | Rule |
|---|---|---|
| `task_id` | string | Matches a task of the partition. |
| `verdict` | `CONFIRMED` \| `REJECTED` | Required. |
| `confidence` | `high` \| `medium` \| `low` | Required. |
| `evidence` | `[{location, note}]` | Each `location` is `path:line`: a relative path without `..` or empty segments, and a 1-based line. The file must exist at the pinned revision, and the line must not be past its end. |
| `reasoning` | string | Non-empty. |
| `reproduction` | string? | Optional. |
| `severity` | `critical` \| `high` \| `medium` \| `low` \| `nit` | Required for CONFIRMED. |
| `rejection` | `{reason, fixed_at?, duplicate_of?, detail}` | Required for REJECTED, forbidden for CONFIRMED. `reason` is `premise_wrong`, `already_fixed` (needs `fixed_at`), `unreachable` or `duplicate` (needs `duplicate_of`). `detail` is non-empty. |

CONFIRMED also needs at least one evidence location. A report that does not
parse, fails a rule, or cites a missing file or line leaves its task
`unverified` with reason `invalid_verdict` or `invalid_evidence`. A verifier
that completes successfully but omits a verdict leaves `missing_verdict`.
`severity` is the five-level scale review findings and `regression_from`
classifications share [ORB-19409]: a classified link carries `severity` and reads back
with `severity_source`, and the verifier confirms or adjusts the filer's
rating (source `verified`).
The shipped job forwards the position-aligned `fan_in.errors` array to apply
as `partition_errors`: a failed verifier leaves `verifier_failed`, with its
partition, crew and dispatch diagnostic. Failed workers retain null output
slots, so one failed partition cannot shift or discard successful siblings'
verdicts.

The recorded **`FindingVerification`** (`schema_version: 1`) holds:

- `source`: the route that decided it, `task_pilot` or `implementer` (§8),
  absent for this job;
- `verdict`, `confidence`, `severity`, `evidence`, `reasoning`,
  `reproduction` and `rejection` from the deciding report, with free text
  redacted;
- `verified_sha` and `base_branch`, the pinned revision;
- `finding_fingerprint`, the finding material captured at preparation (§7);
- `crews`: every verifier that judged the task, in order, each as
  `{crew, tier, verdict, confidence}`;
- `escalated`: whether more than one crew judged it;
- `run_id` and `verified_at`.

## 4. Crew selection and escalation

There is no verification-specific crew setting. The verifier of a task is
drawn from `workflow.<tier>_complexity_crews`. The tier is the task's
complexity, and an unassessed task reads as `medium`. Only pool members with
a positive weight are drawn.

`verify_finding` inspects the pinned source with a CLI's own native
file-read and command tools, which an executor turns on by declaring
`inspection_args` (the shipped Codex and Claude executors do; no other
shipped executor does). The draw therefore keeps only enabled members whose
provider has a CLI runtime and whose executor declares the field; no
provider is named in code [ORB-15792]. A tier whose pool has no such member
passes to the next tier up (low → medium → hard → xhard), and the assignment
records the tier actually drawn from. When no tier from the task's upward
has one, the default crew verifies if it can inspect. Otherwise preparation
fails with `inspection_crew_unavailable` before any verifier is dispatched.

Trust in a verdict comes from its inspection proof, not its provider [ORB-17800]: each
partition result reports `observed_revision` (`git rev-parse HEAD`) and
`observed_tree` (`git rev-parse 'HEAD^{tree}'`) from its checkout, compared
case-insensitively with the pinned revision and its tree, recomputed
host-side [ORB-17833]. A partition that fails leaves every task in it unverified with
one of two reasons:

| Reason | Cause |
|---|---|
| `inspection_proof_missing` | Either observed value is absent or empty. |
| `inspection_revision_mismatch` | Both are reported, but the revision is not the pinned one or the tree is not its tree. |

The envelope carries the revision but never its tree, so a verifier whose
tools never ran cannot forge the pair. A non-Git workspace pins no revision
and skips the check. Prepare stamps its checkpoint
`inspection_proof_required: true`, and so does the escalation checkpoint
apply writes. A checkpoint without the stamp was prepared before the proof
existed: its results are checked only when they report a proof, so a run in
flight across an upgrade keeps its verdicts.

When the pool offers another crew, a crew named by the task's filer
(`created_by`, matched by crew name, model, provider or agent family) is
passed over. The draw is seeded by task and tier, so a retried preparation
draws the same crew.

In the initial round, a REJECTED verdict from the `low` tier, or a verdict of
`low` confidence from any tier, is not recorded. The task is judged again by
a crew from the next tier's pool (low → medium → hard → xhard), under the same
inspection rule: a tier with no inspection-capable member passes upward. That
draw passes over every crew that already judged the task, and the filer's.
The second verdict decides. With no inspection-capable member from the next
tier upward, or from `xhard`, the first verdict stands. There is one escalation round. The escalation checkpoint
retains the initial finding fingerprint, even when its task re-read sees edited
material; the final write checks that original fingerprint.

## 5. Recording

`record_finding_verification` writes a verdict as one task update under the
task write lock, attributed to `finding-verification`:

- a `finding_verified` history entry whose note is the record's JSON — this
  is the trusted record; `OrbitRuntime::finding_verification` and the gate
  read only entries of this event by this actor;
- a comment whose first line is `finding-verification: confirmed` or
  `finding-verification: rejected`, followed by the record. This is what
  `orbit task show` prints;
- for REJECTED, the move to `rejected` with status event `finding_rejected`.

The update requires the status the task was prepared in. A task that left
it, stopped being a review finding, changed any fingerprinted material, or
was verified by another run since preparation settles `superseded` without a
write. The material comparison occurs under the task write lock,
so both confirmation and rejection require the prepared material. A checkpoint
without a fingerprint also settles superseded. A CONFIRMED verdict leaves
the status unchanged; the operator or the drain's proposed-task approval
promotes it.

## 6. The gate

The gate is opt-in per workspace: `[review] verify_findings = true` turns it
on, and with the key false or absent (the default) no surface below holds a
finding on verification state [ORB-15760]. The key is read from the config
files each time the gate is checked, not from the settings the runtime opened
with, so `orbit config set` or the dashboard's Settings › Review switch
applies to a running drain, clock or dashboard. The job and the state trigger
(§7) work either way, and turning the gate on does not enable the routine.

With the gate on, `OrbitRuntime::unverified_finding_hold` holds a review
finding in a verifiable status until its latest record is CONFIRMED for the
current material.
Material edits restore the hold and require a new verification; comment-only
edits and status changes retain confirmation. Promotion with a material edit
in the same update checks the resulting material. The hold's detail
names `finding_verification_pipeline`, the command to run it, and the
seeded `finding_verification.yaml` routine. It also mentions a prior REJECTED
verdict.
These surfaces enforce it:

- `orbit task update <id> --approve` (`proposed` → `backlog`), and an update
  to `in_progress` from `proposed` or `someday` (the start path). Both refuse
  with the hold detail. The update check applies the update's
  own `tags` and `type` first, so retyping or retagging in the same update
  is honoured.
- Workflow entry and pull admission, through `pilot_admission_hold_in`. The
  hold ranks ahead of the other admission holds.
- `list_backlog_tasks`, which excludes the task with reason
  `unverified_review_finding`. `orbit run readiness` and the drain summary
  report that reason.
- Drain approval of proposed tasks (`--approve-proposed`) and drain
  promotion, which hold the task as `unverified_review_finding`.

To take a finding without verification, the operator changes its type from
`bug` or removes its review tag, or turns the gate off for the workspace. A task filed straight into `backlog` is not
refused at creation, but every admission surface above holds it.

## 7. State trigger

The trigger kind `finding_unverified` targets
`job:finding_verification_pipeline`. The seeded `finding_verification`
routine is **disabled**; enabling it is a versioned workspace decision. It
uses the shared member host:

- **Observation**: `bug` tasks in a verifiable status that still need
  verification. A task another live verification run holds is withheld as
  `already_verifying: <runs>`.
- **Fingerprint**: `finding_fingerprint`, a hash of the id, title,
  description, acceptance criteria, sorted tags, type and context files.
  Status, comments and attribution are not part of it. The same fingerprint
  binds prepared checkpoints and persisted verdicts. Evidence-selector edits,
  including task-pilot selector updates, require verification again.
- **Batching**: members are grouped by stored crew, as task-pilot does.
- **Settlement**: apply emits `member_evidence` for each recorded verdict.
  The member host reads the `apply` step, then `apply_escalations` when
  `escalation_count` is non-zero. A recorded verdict removes the task from
  observation while its fingerprint matches. An edit to a fingerprinted field
  releases a failed claim and puts a previously confirmed finding back into
  observation.

## 8. Default rejection routes

`review.verify_findings` is off and the routine disabled by default, so a
wrong finding is usually rejected by work that already reads its code
[ORB-19635]. Both routes record the same verdict through
`record_finding_verification` (§5), entering through
`OrbitRuntime::reject_invalid_finding`: REJECTED with reason
`premise_wrong`, the evidence as `rejection.detail` and `reasoning`, the
fingerprint of the task as it stands at the write, and the deciding route's
`source`, crew (in `crews`, at the task's verification tier) and `run_id`.
A task that is not a review finding (§1) is refused. A replay of a run's own
recorded rejection succeeds without a second write.

- **Task pilot.** For a review finding the pilot reads the cited code at
  `source_revision` and may return `finding_invalid: {"evidence": "..."}`
  with `path:line` citations, next to `duplicate_of`, `already_landed` and
  `release_action_required`. It means the defect as described does not
  exist there, not that it is already fixed or not worth fixing, and it is a
  read-only judgement. Apply validates it like those findings: empty
  evidence, any other task, or a non-Git workspace makes the assessment
  invalid and nothing is written. A valid one keeps the task out of
  readiness, holds drain approval as `finding_invalid`, and after the pilot
  write rejects the task with the pilot's crew and run (`verified_sha` is
  `source_revision`, confidence the pilot's). Apply reports the outcome on
  the task's assessment as `finding_rejection` (`recorded`, and `status` or
  the superseded `reason`); a rejection that does not record leaves the
  pilot write standing. A `finding_invalid` of null changes nothing.
- **Implementer.** A finding that turns out wrong only at pickup, after the
  pilot's revision, is reported by the implementer as `finding-invalid.json`
  (schema in the Orbit skill's task-execution reference): this task, its run
  or retry lineage, the pinned HEAD as `tested_head`, `reasoning`, and at
  least one `{location, note}` citing an existing `path:line` there. It
  changes nothing and runs no workspace gates. On a clean tree at the pinned
  HEAD the `git_commit` step verifies the report and has the host record the
  verdict, then returns the `verified_finding_invalid` checkpoint with
  `skipped_no_diff_expected: true`. The PR pipeline's `promote_no_diff` and
  the local pipeline's `mark_review` and `complete_tasks` skip that decision,
  so nothing is promoted or landed. A report from another run never shadows
  this run's no-diff or already-landed evidence. A claimed leaf cannot write
  the owner's task: its commit step refuses the report, and the implementer
  returns a `finding_invalid` blocker with the evidence instead.

## 9. Concerns & Honest Limitations

- **Existing backlog findings are held.** The gate applies to review findings
  already in backlog. With the routine disabled, they wait until the job runs
  or the operator retypes them.
- **Legacy confirmation is stale.** Verdicts without a stored fingerprint
  remain readable, but cannot authorize promotion or admission. In-flight
  checkpoints without a fingerprint cannot record a verdict. Existing confirmed
  findings need re-verification, including those already in backlog; with the
  routine disabled, run the job explicitly or enable the routine.
- **One escalation round.** A disagreement between tiers is decided by the
  escalation verdict. There is no third opinion.
- **A failed verifier fails the member.** A verifier that fails beyond its
  retries leaves the member failed at its fingerprint until the task is
  edited or the job is run explicitly.
- **Mixed-crew explicit runs.** The run-level crew check refuses an explicit
  run whose `task_ids` carry different stored crews. The trigger avoids this
  by batching per crew.
- **Evidence is checked for existence, not meaning.** Apply proves each
  cited line exists at the pinned revision, not that it shows the defect.
- **The proof's upgrade window.** Runs prepared by the release that first
  required the proof, before the checkpoint stamp, carry no stamp, so their
  proofless results are accepted like any pre-proof run's. The window closes
  when those runs finish.
- **Inspection needs a declaration.** A crew whose executor declares no
  `inspection_args` never verifies, even when its CLI could read the
  checkout; an operator opts it in on the executor definition.
- **Filer matching is by label.** A filer recorded only as a human or
  generic label matches no crew, so nothing is passed over.
- **The default routes have no second opinion.** A pilot or implementer
  rejection is one agent's judgement, with no escalation tier; the pilot's
  free-text citations are not checked against the revision, while the
  implementer's are. An operator who doubts one reopens the task.
- **Claimed runs do not reject.** A claimed implementer's
  `finding_invalid` blocker leaves the rejection to the owner's operator.

## Task References

- ORB-15662 — the verification job, its prepare and apply steps, and the verdict record.
- ORB-15760 — the opt-in workspace gate for verification holds.
- ORB-15792 — drawing verifiers only from inspection-capable crews.
- ORB-16713 — the verifier's explicit mutation and dispatch deny list.
- ORB-17800 — proving inspection by the observed revision and tree instead of a provider gate.
- ORB-17833 — enforcing and hardening the inspection proof with required checkpoints.
- ORB-19409 — the five-level severity scale shared with regression links.
- ORB-19635 — the task-pilot and implementer default rejection routes.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
