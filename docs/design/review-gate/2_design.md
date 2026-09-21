---
title: Review Gate — Design
owner: codex
last_updated: 2026-09-21
last_validated: 2026-09-21
status: Accepted
feature: review-gate
doc_role: design
type: design
summary: Shipped review contract — captured timing, the before-PR gate, what validation records establish, lineage budgets, managed completion, delivery coverage, surfaces, and rollback.
tags: [review-gate, review-policy, automation, delivery, operations]
paths: ["crates/orbit-config/src/operation.rs", "crates/orbit-core/src/application/review/**", "crates/orbit-store/src/driver/sqlite/review/**", "crates/orbit-automation/src/review/**", "crates/orbit-engine/src/executor/automation/vcs/review_gate.rs"]
related_features: [automation-triggers, activity-job, auditability]
related_artifacts: [ORB-11333, ORB-11528, ORB-11545]
---

# Review Gate — Design [ORB-11333]

This file describes what shipped. A preference edit changes no schedule and
grants no authority; a verdict grants no merge permission.

## 1. Preferences: the `[operation]` review keys

Preferences resolve **built-in → global → workspace**. Unknown keys and
out-of-range values fail config load; the operation-mode keys removed on
2026-09-21 are warned about by name and ignored so an older `config.toml`
keeps loading.

| Key | Values (default) |
| --- | --- |
| `operation.review_policy` | `none` (default), `after-landing`, `before-pr` |
| `operation.review_crew` | crew name (before-PR review only) |
| `operation.review_reviewer_starts` | 1..=10 (2) |
| `operation.review_repair_cycles` | 0..=10 (2) |
| `operation.review_minutes` | 1..=1440 (30) |

`orbit config show` lists the explicit `operation.*` values with their file
provenance and the layer that supplied each effective value.

`before-pr` holds PR creation for a fresh reviewer (§3); it needs an explicit
`review_crew`, and admission escalates `review_crew_unconfigured` until one is
set. `review_crew` applies to that before-PR reviewer only: `after-landing`
review is not run by this policy but by the `delivery-code-review` auto-task,
which mints its tasks with the crew in its own template, so setting
`review_crew` does not change who reviews landed work (see [delivery
automation operations](../automation-triggers/5_operations.md)). The three
`review_*` budgets bound one delivery candidate lineage. The resolved-policy
version is 2; a version-1 snapshot fails closed and must be replaced.

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
restart before settlement resumes the open attempt for the same candidate
and task meaning without consuming another start; a different candidate
settles the interrupted attempt as `incomplete` first.

The reviewer is a fresh invocation with its own instruction, tool allowlist,
and 30-minute wall clock. It reads the manifest, verifies claims against code,
repairs only concrete in-scope defects directly in the worktree, runs
validation, and persists `review-report.json` (schema version 1: verdict,
findings with dispositions, validation records with `passed` / `failed` /
`denied` / `not_run`, the `role` each is evidence of, and optional `check`
identity, escalation). It never
runs Git writes, changes task lifecycle, approves, or merges.

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
| `superseded` | A diagnostic attempt a later required check replaced | a later record that is `required` and `passed` and names the same check: the same `command`, or the same non-empty `check` identity when the command or environment was corrected. An unrelated later pass, a missing/empty/one-sided identity, or a related check that did not pass is not a replacement |

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
reads the report with its artifact provenance (missing, predating the
attempt, wrong attempt, or unreadable is `incomplete`), commits every
uncommitted change as one repair commit authored `<family>-reviewer
<<family>-reviewer@orbit.local>` with the Orbit committer and an
`Orbit-Review-Attempt` trailer, and cross-checks the claim: a pass with
open findings, a claimed repair that changed nothing, a claimed clean pass
that changed the tree, repairs outside the task selectors, a spent repair
cycle, elapsed wall time beyond the captured lineage `review_minutes`
allowance, validation records that do not establish the candidate (§4), or any
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

A pass returns `reviewed_head_sha` / `reviewed_base_sha`; `pr_open` refuses
(`review_gate_stale`, phase `stale-review-gate`) when the checked-out head or
pinned base differ. A non-pass fails the step; the failure handoff commits
leftover reviewer work under the reviewer identity, pushes the candidate
branch, blocks the task with `review_gate_escalation`, and opens no PR.
Passing grants no lifecycle transition; `completion: review` still stops at
the handoff.

## 5. Budgets

The ledger is keyed by workspace, sorted task set, and base branch. It spans
retries, interruptions, candidate invalidations, and delivery lineage; nothing
resets it. The first budget written on a lineage is captured; a later config
change cannot expand or replace it. Reviewer starts are reserved before a
reviewer launches. An interrupted open attempt on a changed candidate settles
as incomplete with the wall time already spent, once. Repair cycles and wall
seconds settle with the attempt. The reviewer invocation timeout is the
captured leftover seconds (capped by the activity's declared ceiling), and a
pass that exceeds the leftover allowance is refused. Exhaustion escalates
(`review_budget_exhausted: review_starts_exhausted |
review_minutes_exhausted`). Provider token/cost caps are not enforced; usage
stays unknown.

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

Completion refuses to repair a conflicting reviewed PR (a conflict repair is
unreviewed content), and after the verified merge reads the merge commit,
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
To roll back, set `review_policy` to `none` or `after-landing`: future
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
  and wall time are bounded, so reviewer spend stays unknown.
- `before-pr` has no meaning on the local-only delivery route and is refused
  at submission rather than downgraded.
- A denied required check is not evidence either way: it keeps its own
  `validation_unavailable` reason instead of counting as a failure.

## Task References

- [ORB-11333] — implements independent review policy, the before-PR gate, lineage budgets, and delivery coverage.
- [ORB-11528] — adds validation-record roles to the certificate contract.
- [ORB-11545] — tightens what a superseded validation record may claim.
- [ORB-12491] — retires epic assembly, the one caller that gated a combined candidate later.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
