---
title: Review Gate — Design
owner: codex
last_updated: 2026-10-04
last_validated: 2026-10-04
status: Accepted
feature: review-gate
doc_role: design
type: design
summary: Shipped review contract — captured timing, the before-PR gate, what validation records establish, lineage budgets, managed completion, delivery coverage, surfaces, and rollback.
tags: [review-gate, review-policy, automation, delivery, operations]
paths: ["crates/orbit-config/src/operation.rs", "crates/orbit-core/src/application/review/**", "crates/orbit-core/src/application/automation/after_landing.rs", "crates/orbit-store/src/driver/sqlite/review/**", "crates/orbit-automation/src/review/**", "crates/orbit-engine/src/executor/automation/vcs/review_gate.rs"]
related_features: [automation-triggers, activity-job, auditability]
related_artifacts: [ORB-11333, ORB-11528, ORB-11545, ORB-13890, ORB-13896]
---

# Review Gate — Design [ORB-11333]

This file describes what shipped. A preference edit grants no authority and
changes no schedule except the one `after-landing` names (§1); a verdict grants
no merge permission.

## 1. Preferences: the `[operation]` review keys

Preferences resolve **built-in → global → workspace**. Unknown keys and
out-of-range values fail config load; the operation-mode keys removed on
2026-09-21 are warned about by name and ignored so an older `config.toml`
keeps loading.

| Key | Values (default) |
| --- | --- |
| `operation.review_policy` | `none` (default), `after-landing`, `before-pr` |
| `operation.review_crew` | crew name (before-PR reviewer; crew of after-landing review tasks) |
| `operation.review_reviewer_starts` | 1..=10 (3) |
| `operation.review_repair_cycles` | 0..=10 (2) |
| `operation.review_minutes` | 1..=1440 (90) |

`orbit config show` lists the explicit `operation.*` values with their file
provenance and the layer that supplied each effective value.

`before-pr` holds PR creation for a fresh reviewer (§3); it needs an explicit
`review_crew`, and admission escalates `review_crew_unconfigured` until one is
set.

`after-landing` is carried out by the workspace's shipped `delivery-code-review`
delivery auto-task [ORB-13896]. The policy is its switch: while the policy is
`after-landing` that consumer is enabled whatever its own `enabled` field says,
so it freezes landed base-branch deliveries into batches and mints one review
task per batch at its threshold or maximum wait. `orbit auto-task list` and
`show` report it as enabled by the policy, and toggling it off does not stop it;
setting the policy to `none` or `before-pr` does. When `review_crew` is set it
is the crew of every review task the consumer mints; unset, the definition's
template crew (`system`) applies. Changing `review_crew` changes only future
mints, never the consumer's epoch or its retained debt. The consumer admits only
on the machine that owns the workspace (see [delivery automation
operations](../automation-triggers/5_operations.md)).

Because nothing else performs after-landing review, `orbit doctor` reports a
`review-after-landing` row whenever the policy is `after-landing`, and
`orbit config show` prints the same line (`review_after_landing` in `--json`):
whether the consumer is present and enabled, whether this host owns it, whether
it is wedged on a closed action or stalled, whether its branch and review crew
resolve, its scheduling state, and when its last batch was minted or covered.
Anything short of healthy — the definition missing, owned by another machine or
by none, wedged, stalled, held for an operator (`definition_changed`,
`needs_attention`, `retry_deadline_expired`), on a branch that does not resolve,
or naming a crew that does not — is an `error`, so `orbit doctor` exits nonzero.
Under any other policy the row is `skipped`.

The three `review_*` budgets bound one delivery run lineage (§5). The
resolved-policy version is 2; a version-1 snapshot fails closed and must be
replaced.

## 2. Captured timing

Review timing is captured once per delivery run and never re-read. Every
submission in the delivery family (`workspace_auto_pipeline`,
`task_auto_pipeline`, `task_gate_pipeline`, `task_pr_pipeline`,
`task_local_pipeline`) carries a versioned `review`
snapshot in its immutable input: timing and its source, the configured
reviewer crew and its source, the lineage budget, and the policy version. A
parent-authorized child inherits its parent's snapshot exactly; any other submission resolves from the
workspace preferences at that moment. Ordinary input naming the
reserved `review` key is refused, and a resume keeps its persisted input, so
rolling a preference back to `none` never weakens a gate that is already
active and switching to `before-pr` never gates a run already admitted.
`before-pr` is refused at submission for `task_local_pipeline` delivery,
with no exemption: epic assembly was the one caller that gated a combined
candidate later, and it is retired [ORB-12491].

## 3. The gate

`task_pr_pipeline` runs three steps after the final base
synchronization and before push/PR creation: `review_gate_admit`,
`review` (`agent_review_repair`), and `review_gate_settle`. Under `none`,
`after-landing`, or a checked no-diff exemption the gate reports
`applies: false` and publication proceeds unchanged with no certificate.

Admission pins the candidate (base and head commits and trees, every
implementation commit with the attribution Git recorded), digests each task's
meaning (title, description, criteria, plan, selectors, tags, relations, type;
never comments, summaries, status, or priority), resolves the configured
review crew on this host inside the run's `allowed_crews`, reserves a reviewer
start against the lineage ledger, and writes `review-manifest.json` on every
task under the run's authority. An unconfigured, unresolvable, or excluded crew
escalates (`review_crew_unconfigured`, `review_crew_unavailable`,
`review_crew_excluded`); the gate never substitutes the implementer. A
retried admission in the same run resumes its open attempt for the same
candidate and task meaning without consuming another start; a different
candidate, or an attempt another run of the lineage left open, is released
as `incomplete` first (§5).

The deterministic gate writes manifests, certificates, settlement comments,
and any repair-driven selector changes as `system`, regardless of the
operator identity that opened the runtime. Run ownership still authorizes
artifact writes; it does not identify their author. Reviewer identity and crew
remain in the manifest and certificate, and `review-report.json` retains the
reviewer agent attribution. Any history generated by a system write uses the
same system actor; settlement does not add synthetic history stubs.

The reviewer is a fresh invocation with its own instruction, tool allowlist,
and 60-minute wall clock, independent of what the lineage has already spent.
It reads the manifest, verifies claims against code, repairs only concrete
in-scope defects directly in the worktree, runs validation, and persists
`review-report.json` (schema version 1: verdict, findings with dispositions,
validation records with `passed` / `failed` / `denied` / `not_run`, the
`role` each is evidence of, and optional `check` identity, escalation). It
never runs Git writes, changes task lifecycle, approves, or merges.

The instruction carries the report's exact JSON Schema, and
`orbit.task.artifact.put` validates a `review-report.json` on attach,
refusing a mismatch with the offending field named so the reviewer can fix it
while it still runs [ORB-13890]. Both the validator and settlement read
through `ReviewReport::parse`, which accepts benign shape drift that leaves
the meaning unambiguous — a bare-string `disposition`, enum labels in another
case or with `-`/space separators, `pass`/`fail`/`skipped` outcomes, a single
path string, a numeric finding id or string `schema_version`, a missing
`schema_version`, `summary`, or finding `severity`, and `null` lists — and
refuses anything else.

Admission and settlement retry transient failures (three attempts,
exponential backoff) and the reviewer step retries once; each then gets one
`step_failure_recovery` diagnosis before the run fails. A retried reviewer
continues the same attempt and must verify or revert edits an interrupted
invocation left in the worktree. Decisions are refusals that neither retry
nor recover: a settled non-pass verdict, an exhausted budget, an
unconfigured, unavailable, or excluded crew, and a local route.

## 4. What the validation records establish [ORB-11528] [ORB-11545]

An honest reviewer records more than the checks that had to pass, so each
validation record also carries a `role` saying what it is evidence of, and
`orbit_automation::review::validation_evidence` decides what the set
establishes. Settlement and delivery coverage both read that one function, so
a certificate cannot mean one thing when it is issued and another when it is
spent.

| `role` | Meaning | Passing requires |
| --- | --- | --- |
| `required` (default) | A check the final candidate must pass | `passed`; `failed`, `denied`, and `not_run` all block |
| `expected_failure` | A negative control — the superseded assertion, the pre-fix reproduction | `failed`; any other outcome contradicts the claim |
| `excluded` | An action outside the authorized scope, deliberately not performed | `not_run` or `denied`; actually running it contradicts the exclusion |
| `superseded` | A diagnostic attempt a later required check replaced | a later record that is `required` and `passed` and names the same check: the same `command` (whitespace ignored), or the same non-empty `check` identity when the command or environment was corrected. A record without `check` is matched by its command, so a missing optional field never downgrades a pass. An unrelated later pass, a corrected command with no shared identity, or a related check that did not pass is not a replacement |

At least one `required` record must have passed, so a set of controls and
exclusions alone is never coverage. Every role other than `required` must
carry a `note`; an unexplained reclassification is refused rather than
trusted. A record written before this contract carries no role and is read as
a required check, so older evidence keeps its conservative meaning. The
certificate keeps every raw observation with its classification — a superseded
failure is preserved, never erased — and the verdict comment discloses the
breakdown. A denied required check keeps its own `validation_unavailable`
reason: the runner refused, which is neither a defect in the candidate nor
evidence about it.

The contract version stays 1: a record carrying no role decides exactly as it
did before, so older role-less evidence is not reinterpreted. A superseded
attempt now requires the later required pass that names the same check; a
certificate that treated an unrelated later pass as a replacement becomes
incomplete when coverage re-reads the records. Candidates already refused
under the old rule recover through a fresh run, not by editing stored evidence.

Settlement rechecks the checked-out head against the admitted candidate,
reads every bundle task's report with its artifact provenance — reports that
predate the attempt are ignored, the rest merge into the most severe verdict
with every distinct finding and validation record — (none, a wrong attempt,
another contract version, or an unreadable report is `incomplete`), commits every
uncommitted change as one repair commit authored `<family>-reviewer
<<family>-reviewer@orbit.local>` with the Orbit committer and an
`Orbit-Review-Attempt` trailer, and cross-checks the claim: a pass with
open findings, a claimed repair that changed nothing, a claimed clean pass
that changed the tree, repairs outside the task selectors, a spent repair
cycle, validation records that do not establish the candidate (§4), or any
task-meaning change other than selectors added through the task API
downgrades the verdict to `incomplete` with the reason recorded. Verdicts are
`passed_without_repairs` (`independent_review`), `passed_with_repairs`
(`independent_review_with_self_authored_repairs`; the repairs were validated,
not independently reviewed), `changes_required`, and `incomplete`. The
certificate (`review-gate.json`, recorded immutably in the host store and
indexed by final candidate tree when passed) binds verdict, reviewer
identity, base, reviewed and final candidate, implementation and repair
commits, findings, validation, consumed budget, and escalation. Settlement is
idempotent: a replay reconciles the recorded certificate.

Settlement persists in order — repair commit, ledger charge, certificate,
then each task's `review-gate.json` and verdict comment — and a replay
resumes from whichever step last persisted. A head one commit past the
admitted candidate is adopted as the attempt's repair only when that commit
has the candidate as its sole parent, carries this attempt's trailer, and has
the reviewer author and Orbit committer; its paths are judged as they were
before the commit, so a drive-by still downgrades. A ledger that settled
without a certificate is re-judged against the ledger as it stood before its
own charge, with the reviewer runtime it recorded. The certificate is issued only
when that judgement reproduces the recorded verdict and repair count;
otherwise the replay refuses with `settlement_diverged`. Nothing is committed
or charged twice. A recorded certificate is restored onto every bundle task
that lacks it before the replay reports its verdict. A pass still refuses
task-meaning or head drift first.

A pass returns `reviewed_head_sha` / `reviewed_base_sha`; `pr_open` refuses
(`review_gate_stale`, phase `stale-review-gate`) when the checked-out head or
pinned base differ. A non-pass refuses the step; the failure handoff
releases the attempt if it has no verdict yet (§5), commits leftover reviewer
work under the reviewer identity, pushes the candidate branch, blocks the task
with `review_gate_escalation`, and opens no PR.
Passing grants no lifecycle transition; `completion: review` still stops at
the handoff.

## 5. Budgets

The ledger is keyed by workspace, sorted task set, base branch, and delivery
run lineage: the first run of the resume chain (`retry_source_run_id`). A
resumed run shares its source's ledger, so resuming never resets the budget;
a fresh delivery run of the same tasks — the re-admission after a block —
starts a new lineage with a full budget [ORB-13890]. Settlement uses the
lineage its admission named. The first budget written on a lineage is
captured; a later config change cannot expand or replace it. Reviewer starts
are reserved before a reviewer launches.

The lineage is charged reviewer process runtime, not wall time. The engine
reports the start and end of every reviewer dispatch — each retry and the
post-recovery re-attempt included — and the attempt accumulates what they
add up to. Retry backoff, `step_failure_recovery`, the gate's own steps, and
time between a run's end and its resume are never charged. A reviewer whose
end was never reported (its process died with the run) is charged up to the
run's end, never past its start plus its own wall-clock bound.

No attempt stays open after its reviewer step fails or its run ends. The
failure handoff releases every attempt the run admitted that has no verdict —
for a bundle as for a single task, before any task-shaped check — settling it
`incomplete`, marked released, with the reviewer runtime charged so far. Run
finalization releases whatever a terminating run still holds, whether it
failed, was cancelled, or was found dead and interrupted, so a fresh lineage
never strands the old one open; the ledger indexes the run holding each
lineage (store migration `lineage_holder_run`) to find them. The next
admission of a lineage also releases an attempt another run left open. A
resumed run that reuses the admission checkpoint may still settle a released
attempt with its verdict, keeping the runtime already charged and adding the
reviewer runtime it records; a released attempt that a later start superseded
is stale.

`review_minutes` gates admission only: once the lineage's charged reviewer
runtime reaches it, no new start is admitted. It does not shorten an admitted
reviewer, whose own activity timeout bounds each invocation, and a pass that
overran the remainder still settles on its evidence. Repair cycles settle
with the attempt. Exhaustion refuses admission (`review_budget_exhausted:
review_starts_exhausted | review_minutes_exhausted`). Provider token/cost
caps are not enforced; usage stays unknown.

## 6. Managed completion and landing

`pr_complete` under a gate pins the provider-reported PR head against the
reviewed head before merging (`review_gate_stale` on a moved head), then sends
that SHA as the `sha` precondition on GitHub's synchronous REST merge mutation.
A push between inspection and mutation is rejected by the provider. Gated runs
wait locally for pending checks; they never enable auto-merge or enter a merge
queue. Queue-only branches and other unsupported synchronous merges fail closed,
leaving the task in review. Ungated runs retain ordinary `gh pr merge` and
repository-enabled auto-merge. See the
[provider merge contract](https://docs.github.com/en/rest/pulls/pulls#merge-a-pull-request).

A conflicting reviewed PR is never merged as rebased, unreviewed content
[ORB-13890]. `complete_pr` runs with `re_review_on_conflict`: it rebases the
branch locally through the pinned `git_rebase` (a real conflict still reaches
`pr_conflict_recovery`), publishes nothing, completes no task, and returns
`re_review_required` with the rebase checkpoint. The pipeline then reviews the
rebased head with a new start in the same lineage (`re_review_gate_admit`,
`re_review`, `re_review_gate_settle`), republishes the reviewed final
candidate under a lease on the old published head (`re_push`), and completes
it (`complete_reviewed_pr`). A second conflict there, a caller without the
flag, or a non-pass re-review keeps the published PR and the task in review.
`complete_pr` is skipped on review-only and no-diff routes, and a `when:` may
not read a skippable step's output, so `re_review_gate_admit` and
`re_review_gate_settle` always run: with `re_review_after: complete_pr` the
admission reads that step's checkpoint from the run's recorded pipeline (a
resume inherits it), answers `re_review_not_required` unless it asked for a
re-review, pins the recorded rebase base, and fails when the worktree head is
not the recorded rebased head. Its `applies` gates the remaining steps.

After the verified merge, completion reads the merge commit,
fetches it, and records a landing: `fast_forward`, `squash`, `merge_commit`,
or `rebase` when the landing started from the reviewed base tree and produced
the reviewed final tree, otherwise uncovered with `base_changed`,
`candidate_changed`, `objects_missing`, or `mapping_unknown`. An externally
completed merge (including one observed while polling) is recorded as uncovered
with `external_landing_race`; the landing audit includes `managed_merge`, which
requires a conditional mutation response matching the subsequently observed
merge commit. A landing is never fabricated.

## 7. Delivery coverage

Delivery observation asks the host store for passed certificates whose final
tree equals the landed tree, verifies the certificate objects still exist and
every task still has the reviewed meaning, and lets
`orbit_automation::review::exclusion` decide: same base tree, same final
tree, no contradicting managed landing. Only a `landed_code_review_v1`
consumer excludes; QA counts every landing. Excluded landings leave
`pending`, live in the consumer's `excluded` list, do not count toward the
threshold, and are absent from `examined_deliveries`. An exclusively excluded
prefix advances the covered cursor without an examination receipt so
observation cannot stall; interleaved exclusions travel with the next frozen
batch as readable context (`exclusions`) and retire with that examined
range. Later edits, task drift, a different base, an unreviewed conflict
repair, missing objects, or an external landing race keep the landing an
ordinary obligation. Exclusions apply when a landing is first
observed; a certificate that arrives later does not rewrite pending debt.

## 8. Surfaces

`orbit config show` reports the review policy, crew, and budgets with the
layer that supplied each. `orbit task show --json`, the task API, and the
task detail view carry a `review` block: verdict, assurance, reviewer
(including `same_model_as_implementer`), base/reviewed/final candidate,
implementation and repair commits, findings, validation, consumed and
remaining budget, landings, and stale-gate reasons. Auto-task inspection and
the automation panel show `excluded` landings with their certificate. Audit
rows `review.gate` cover admit, settle, and landing. Task artifacts
`review-manifest.json`, `review-report.json`, and `review-gate.json` are the
durable evidence.

## 9. Compatibility and rollback

Existing runs without a `review` snapshot behave exactly as before. The
seeded cron `code-review` auto-task and any custom definition stay untouched;
migrating to delivery-triggered review remains the explicit edit described in
[delivery automation operations](../automation-triggers/5_operations.md).
To roll back, set `review_policy` to `none` or `after-landing` (which also
enables the `delivery-code-review` consumer, §1): future
submissions capture the new timing, admitted runs keep their gate, and
certificates, ledgers, and landings stay readable. An older binary cannot settle an
in-flight gate; drain gated runs with a supporting binary before downgrading.

## 10. Concerns & Honest Limitations

- Coverage is exact-tree only: a landing that is semantically identical but
  not byte-identical to the reviewed candidate stays an ordinary review
  obligation. Content-equivalence coverage is not implemented.
- Reviewer repairs are validated, not independently reviewed. A
  `passed_with_repairs` verdict carries the weaker assurance
  `independent_review_with_self_authored_repairs` and says so.
- Provider token and cost caps are not enforced; only starts, repair cycles,
  and reviewer runtime are bounded, so reviewer spend stays unknown.
- A resume charges an attempt from the resumed run's start, which includes
  the skipped steps before the reviewer; the overcount is bounded by that
  replay and never includes time no run was working.
- `before-pr` has no meaning on the local-only delivery route and is refused
  at submission rather than downgraded.
- A denied required check is not evidence either way: it keeps its own
  `validation_unavailable` reason instead of counting as a failure.

## Task References

- [ORB-11333] — implements independent review policy, the before-PR gate, lineage budgets, and delivery coverage.
- [ORB-11528] — adds validation-record roles to the certificate contract.
- [ORB-11545] — tightens what a superseded validation record may claim.
- [ORB-12491] — retires epic assembly, the one caller that gated a combined candidate later.
- [ORB-13890] — closes failed attempts, keys budgets per delivery run lineage, adds gate retry/recovery, tolerant report reading, and the completion re-review.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
