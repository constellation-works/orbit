---
title: Operation Mode — Operations
owner: claude
last_updated: 2026-09-07
last_validated: 2026-09-07
status: Accepted
feature: operation-mode
doc_role: operations
type: design
summary: Shipped operation-mode contract — typed preferences, scoped grants, grant-bound drains, bounded recovery, the before-PR review gate with delivery coverage, surfaces, observability, and rollback.
tags: [operation-mode, automation, authorization, recovery, operations]
paths: ["crates/orbit-config/src/operation.rs", "crates/orbit-core/src/application/operation/**", "crates/orbit-core/src/application/review/**", "crates/orbit-store/src/driver/sqlite/operation/**", "crates/orbit-store/src/driver/sqlite/review/**", "crates/orbit-automation/src/members/**", "crates/orbit-automation/src/review/**", "crates/orbit-engine/src/executor/automation/vcs/review_gate.rs"]
related_features: [automation-triggers, activity-job, routines]
related_artifacts: [ORB-11545, ORB-11528, ORB-11333, ORB-11332, ORB-11331, ORB-11330]
---

# Operation Mode — Operations [ORB-11332]

This file describes what shipped. [The vision](./3_vision.md) keeps the
remaining proposals. Nothing here activates automation: a preference edit
changes no schedule and grants no authority, every shipped routine and
auto-task definition stays disabled, and no existing run or `--complete`
invocation is reinterpreted.

## 1. Preferences: `[operation]` in `config.toml`

Preferences resolve **built-in supervised → global → workspace → run**. An
explicit `preset` at a layer resets the preset-managed fields to that
preset's defaults *before* that layer's own explicit fields apply; an
omitted preset preserves inherited fields. The review fields and the
delivery cap are independent: a preset never resets or infers them. Unknown
keys and out-of-range values fail config load.

| Key | Values (default) | Preset-managed |
| --- | --- | --- |
| `operation.preset` | `supervised` (default), `autonomous` | selector |
| `operation.preparation` | `manual` / `automatic` (supervised `manual`, autonomous `automatic`) | yes |
| `operation.preparation_due_seconds` | 1..=86400 (300) | yes |
| `operation.leaf_ceiling` | 1..=500 (supervised 5, autonomous 10) | yes |
| `operation.promotion` | `separate_approval` / `automatic` (supervised `separate_approval`, autonomous `automatic`) | yes |
| `operation.completion` | `review` / `done` (supervised `review`, autonomous `done`) | yes |
| `operation.recovery` | `existing` / `scheduled` (supervised `existing`, autonomous `scheduled`) | yes |
| `operation.recovery_episodes_per_task` | 0..=10 (2) | yes |
| `operation.recovery_minutes_per_task` | 1..=1440 (30) | yes |
| `operation.review_policy` | `none` (default), `after-landing`, `before-pr` | no |
| `operation.review_crew` | crew name (before-PR review only) | no |
| `operation.review_reviewer_starts` | 1..=10 (2) | no |
| `operation.review_repair_cycles` | 0..=10 (2) | no |
| `operation.review_minutes` | 1..=1440 (30) | no |
| `operation.delivery_cap` | `review` (default), `done` | no |

`orbit config show` lists the explicit `operation.*` values with their file
provenance; a preset-managed key reset by a workspace preset shows `null`
with a `built-in` source because no explicit value survived. The *effective*
values and their winning source (`workspace`, `global`, `preset:autonomous@workspace`,
`run`) come from `orbit operation explain`. That explanation keeps current
(or preview) preferences distinct from an active grant's captured policy:
delivery, preparation, recovery, review, caps, and limiting reasons that
describe live authority are projected from the grant snapshot. Preference
edits and `--preset` previews apply to a future grant only.

`before-pr` holds PR creation for a fresh reviewer (§10, [ORB-11333]); it
needs an explicit `review_crew`, and the explanation reports
`review_crew_unconfigured` until one is set. `review_crew` applies to that
before-PR reviewer only: `after-landing` review is not run by this policy but by
the `delivery-code-review` auto-task, which mints its tasks with the crew in its
own template, so setting `review_crew` does not change who reviews landed work
(see [delivery automation operations](../automation-triggers/5_operations.md)).
The three `review_*` budgets bound one delivery candidate lineage. `delivery_cap` defaults to `review`, so
an autonomous `completion = done` preference is capped at review until the
workspace explicitly raises the cap. The cap is disclosed in the explanation.
The resolved-policy version is 2; version-1 grants fail closed and must be
replaced.

## 2. Authority: grants

```sh
orbit operation explain [--preset autonomous ...]     # preview; changes nothing
orbit operation enable --task ORB-1,ORB-2 --for 2h --right prepare,promote
orbit operation list | show <ID> | stop [--id <ID> --if-revision N] | revoke [...]
orbit run auto --grant <ID> [--for 30m] [--concurrency N]
```

Enablement is the one moment authority is created. It validates a finite,
non-empty task set (proposed or backlog tasks, at most 50), a window of at
most 24 hours, and at least one right; it resolves the effective policy
once (config layers plus the request's run layer), refuses an explicit
escalation past the delivery cap (`--completion done` or `--right complete`
under `delivery_cap = review`), captures the numeric
limits (leaf ceiling bounded by the leaf job's hard limit), and persists
the grant with its versioned policy snapshot. One active grant per
workspace; enabling a replacement requires stopping the old one first. The
`enable`, `stop`, and `revoke` verbs are governed operator operations on
every surface.

A grant carries separate rights. `prepare` accelerates in-scope preparation
in the shared state evaluator; `promote` lets fresh positive evidence move
proposed in-scope work to backlog; `complete` lets a bound drain capture
`done` when the cap allows it. No right authorizes merge or bypasses
dependencies, reservations, capacity, task approval rules, or repository
gates.

## 3. Grant-bound drains and admission

`orbit run auto --grant <ID>` submits the existing `workspace_auto_pipeline`
with an `operation` snapshot in its immutable run input: grant id and
revision, policy version, absolute expiry, effective completion, and captured
limits. The window is the intersection of the request and the grant's
remaining time; the ceiling is the intersection of the request, the captured
ceiling, and the job's hard limit; completion is `done` only with the
`complete` right and a `done` effective completion. Ordinary job input that
names the reserved `operation` key is refused.

Each classifier iteration rechecks the live grant: stop, expiry, and
revocation set `free_slots` to zero and close the drain window
(`expired_reason` becomes `grant_stopped`, `grant_expired`, or
`grant_revoked`); only the finite scope is offered; and fresh positively
assessed proposed work inside the scope is promoted first (see §4). The
Store rechecks everything again inside the transaction that creates each
detached child: grant admission at that instant, exact revision, scope,
an unclaimed task, and free leaf capacity. Refusals carry a reason
(`grant_stopped`, `grant_expired`, `grant_revoked`, `grant_revision_changed`,
`outside_grant_scope`, `task_claimed`, `capacity_saturated`) in the child
dispatch output and the `pipeline.invoke` audit row. Children inherit
exactly the parent's snapshot at that path; nested gate and PR children
inherit it transitively, so completion and recovery see the same bounds.

Live concurrency changes on the coordinator narrow the captured ceiling but
never widen it. Restart reads the same snapshots; retuning global or
workspace preferences affects future grants only. A replacement window sees
the previous grant's still-live children as claims and cannot double-claim
their tasks.

## 4. Promotion evidence

Promotion under a grant needs, for the exact current task version: the
`promote` right with an automatic promotion preference in the captured
policy, satisfied dependencies, a task the preparation contract deems
eligible (no no-diff disposition), and an accepted preparation assessment
from the shared state consumer whose `ready` flag is set and whose resulting
material fingerprint equals the task's current fingerprint at the current
landing-branch head. The write happens under the task lock after a fresh
recheck, records an `operation_promoted` history event, and audits
`operation.promotion` with the decision. Withheld reasons are reported per
task: `assessment_missing`, `assessment_unready`, `assessment_stale`,
`unmet_dependency`, `special_disposition_withheld`, `promote_right_missing`,
`promotion_separate_approval`, `status_changed`, `grant_no_longer_admits`.

## 5. Stop, expiry, revocation

| Control | New admissions and promotion | Admitted work |
| --- | --- | --- |
| Expiry (absolute deadline, derived at every check) | refused (`grant_expired`) | keeps captured bounds, including completion |
| `stop` (compare-and-set, replay is `unchanged`) | refused (`grant_stopped`); bound drains get the existing admissions stop | keeps captured bounds |
| `revoke` | refused (`grant_revoked`); bound drains stopped | loses privileged actions: the guarded `review -> done` transition is refused at the transition itself |

None of these cancel running children; `orbit run cancel` remains separate.
A revoked grant keeps its stop evidence. Neither transition resets a
deadline or a budget.

## 6. Bounded recovery

A per-task ledger in the host store spans engine step-recovery hooks,
resumed runs, and terminal-run triage. Before a recovery hook is dispatched
for a run carrying an `operation` snapshot, Core reserves an episode (a retry
of the same run and step reuses it) and later settles its wall time; crashes
and timeouts count. Triage reserves an episode when it lists a bound
candidate and settles it when dispositions apply. When the captured
`recovery_episodes_per_task` or `recovery_minutes_per_task` is spent, the
hook is skipped with the original error authoritative, triage lists the task
under `exhausted` with the reason, and the task gains a durable
`recovery_budget_exhausted` history event (recorded once). Runs without a
snapshot keep the pre-existing unbounded behavior. Provider token/cost caps
are not enforced: usage remains unknown.

## 7. Scheduling constraints

Operation mode owns no timer. The operator-enabled state routine remains the
single cadence owner. With an admitting grant, Core hands the shared
evaluator `MemberConstraints`: the grant scope and a due interval
(`preparation_due_seconds` for preparation members when the preference is
automatic and the grant carries `prepare`; zero for incidents when recovery
is scheduled). In-scope members become due sooner; every other member keeps
the routine's own timing. The explanation names the cadence owners and
reports `no_enabled_preparation_routine` / `no_enabled_triage_routine` when
a preference has no owner to act through.

## 8. Surfaces and observability

- CLI: `orbit operation explain|enable|list|show|stop|revoke`, `orbit run auto --grant`, and
  `orbit run readiness`, whose `capacity.operation` block and per-task
  reasons (`outside_grant_scope`, `grant_*`) reflect a live grant-bound drain.
  Operation mode is an operator control on the CLI and dashboard; agents read
  grant state from readiness rather than MCP tools.
- Dashboard: the Operations → Auto-drain view has an **Operation Mode** panel
  showing every field with its source, the grant, caps, limiting reasons,
  and governed Stop/Revoke controls with compare-and-set. When a grant is
  active the panel shows the captured grant policy separately from current
  preferences (future grants). Enablement is deliberately CLI only.
- Audit: `operation.grant` (enabled/stopped/revoked/rejected),
  `operation.promotion`, `operation.recovery`, `operation.completion`,
  plus the existing `pipeline.invoke` and admissions-stop rows. Run inputs
  carry the snapshot; the classifier's `operation` output and the drain
  window's `expired_reason` explain each iteration. Usage and cost stay
  unknown unless measured elsewhere.

## 9. Compatibility and rollback

Existing installs, custom jobs and routines, and omission of `--complete`
behave exactly as before; `--complete` remains the separate blanket
authorization and cannot be combined with `--grant`. Runs without an
`operation` snapshot are never bound. To roll back: `orbit operation stop`
(or `revoke`) the active grant, let admitted children settle, and remove
`[operation]` keys if desired; readable grant, ledger, and audit records
remain. An older binary rejects the unknown `[operation]` keys at config
load and cannot enforce grants, so stop or revoke with a supporting binary
before downgrading.

## 10. Independent review policy [ORB-11333]

Review timing is captured once per delivery run and never re-read. Every
submission in the delivery family (`workspace_auto_pipeline`,
`task_auto_pipeline`, `task_gate_pipeline`, `task_pr_pipeline`,
`epic_pipeline`, `task_local_pipeline`) carries a versioned `review`
snapshot in its immutable input: timing and its source, the configured
reviewer crew and its source, the lineage budget, and the policy version. A
parent-authorized child inherits its parent's snapshot exactly; a grant-bound
run resolves from the grant's captured policy; any other submission resolves
from the workspace preferences at that moment. Ordinary input naming the
reserved `review` key is refused, and a resume keeps its persisted input, so
rolling a preference back to `none` never weakens a gate that is already
active and switching to `before-pr` never gates a run already admitted.
`before-pr` is refused at submission for ordinary `task_local_pipeline`
delivery. A parent-authorized `epic_pipeline` child may still assemble
locally onto the epic branch: it inherits the captured snapshot unchanged,
and the epic's own before-PR gate remains mandatory on the combined
candidate. Caller-shaped input cannot claim that assembly exemption. The
epic pipeline itself refuses `before-pr` when its route resolves to local.

### The gate

`task_pr_pipeline` and `epic_pipeline` run three steps after the final base
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

### What the validation records establish [ORB-11528] [ORB-11545]

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
allowance, validation records that do not establish the candidate (above), or any
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

### Budgets

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

### Managed completion and landing

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

### Delivery coverage

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

### Surfaces

`orbit operation explain` and the dashboard operation panel show the review
policy, crew, and budgets with their sources and report
`review_crew_unconfigured`. With an active grant those live fields come from
the captured snapshot; current or preview preferences remain visible as the
next grant's defaults. `orbit task show --json`, the task API, and the
task detail view carry a `review` block: verdict, assurance, reviewer
(including `same_model_as_implementer`), base/reviewed/final candidate,
implementation and repair commits, findings, validation, consumed and
remaining budget, landings, and stale-gate reasons. Auto-task inspection and
the automation panel show `excluded` landings with their certificate. Audit
rows `review.gate` cover admit, settle, and landing. Task artifacts
`review-manifest.json`, `review-report.json`, and `review-gate.json` are the
durable evidence.

### Compatibility and rollback

Existing runs without a `review` snapshot behave exactly as before. The
seeded cron `code-review` auto-task and any custom definition stay untouched;
migrating to delivery-triggered review remains the explicit edit described in
[delivery automation operations](../automation-triggers/5_operations.md).
To roll back, set `review_policy` to `none` or `after-landing`: future
submissions capture the new timing, admitted runs keep their gate, and
certificates, ledgers, and landings stay readable. An older binary rejects
the new `[operation]` keys at config load and cannot settle an in-flight
gate; drain gated runs with a supporting binary before downgrading.

## 11. Still proposed

Standing or dynamic scopes, federation-wide enrollment, provider cost
reservations, content-equivalence coverage beyond exact trees, a second
independent review of reviewer repairs, and automatic migration of legacy
sweeps remain in [the vision](./3_vision.md).
