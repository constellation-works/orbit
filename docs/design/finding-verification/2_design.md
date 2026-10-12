---
title: Finding Verification — Design
owner: claude
last_updated: 2026-10-11
last_validated: 2026-10-11
status: Accepted
feature: finding-verification
doc_role: design
type: design
summary: How finding_verification_pipeline selects review findings, draws verifier crews from the complexity pools, validates and records verdicts, escalates doubtful ones, and gates promotion and drain admission.
tags: [finding-verification, review, automation, admission]
paths: ["crates/orbit-core/src/application/task/finding_verification.rs", "crates/orbit-core/src/adapter/engine_host/v2_host/finding_verification/**", "crates/orbit-core/assets/jobs/finding_verification_pipeline.yaml"]
related_features: [automation-triggers, review-gate, distributed-drain, routines]
related_artifacts: [ORB-15662]
---

# Finding Verification — Design

This doc covers the current implementation: which tasks are findings, the
job, the verdict schema and how it is recorded, crew selection and
escalation, the gate, and the state trigger. Why verification is its own job
rather than a task-pilot phase is in [4_decisions.md](4_decisions.md).

## 1. Review findings

`is_review_finding` is true for a task of type `bug` carrying one of
`REVIEW_FINDING_TAGS`: `code-review`, `delivery-code-review`,
`security-review`, `qa-sweep`. A finding is verified, and held, only in
`proposed`, `someday` or `backlog` (`VERIFIABLE_STATUSES`). A finding needs
verification there until its latest verification record is CONFIRMED and its
`finding_fingerprint` matches the current task material (§7).

## 2. The job

`finding_verification_pipeline` has three steps:

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
   dispatch or repository-write grant.
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
| `severity` | `critical` \| `high` \| `medium` \| `low` | Required for CONFIRMED. |
| `rejection` | `{reason, fixed_at?, duplicate_of?, detail}` | Required for REJECTED, forbidden for CONFIRMED. `reason` is `premise_wrong`, `already_fixed` (needs `fixed_at`), `unreachable` or `duplicate` (needs `duplicate_of`). `detail` is non-empty. |

CONFIRMED also needs at least one evidence location. A report that does not
parse, fails a rule, or cites a missing file or line leaves its task
`unverified` with reason `invalid_verdict` or `invalid_evidence`. A verifier
that completes successfully but omits a verdict leaves `missing_verdict`.
The shipped job forwards the position-aligned `fan_in.errors` array to apply
as `partition_errors`: a failed verifier leaves `verifier_failed`, with its
partition, crew and dispatch diagnostic. Failed workers retain null output
slots, so one failed partition cannot shift or discard successful siblings'
verdicts. Gemini and Grok currently fail native pinned-source inspection
before launch with `inspection_tools_unavailable`; Codex and Claude provide
that inspection surface.

The recorded **`FindingVerification`** (`schema_version: 1`) holds:

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
a positive weight are drawn. When the pool offers another crew, a crew named
by the task's filer (`created_by`, matched by crew name, model, provider or
agent family) is passed over. The draw is seeded by task and tier, so a
retried preparation draws the same crew. When the tier's pool has no enabled
member, the default crew verifies.

In the initial round, a REJECTED verdict from the `low` tier, or a verdict of
`low` confidence from any tier, is not recorded. The task is judged again by
a crew from the next tier's pool (low → medium → hard → xhard). That draw
passes over every crew that already judged the task, and the filer's. The
second verdict decides. With no next-tier pool member, or from `xhard`, the
first verdict stands. There is one escalation round. The escalation checkpoint
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
with, so `orbit config set` or the dashboard's Settings › Effective switch
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

- `orbit task approve`, an update to `backlog` or `in_progress` from
  `proposed` or `someday`, and `orbit task start` from those statuses. All
  of them refuse with the hold detail. The update check applies the update's
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

## 8. Concerns & Honest Limitations

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
- **Filer matching is by label.** A filer recorded only as a human or
  generic label matches no crew, so nothing is passed over.
