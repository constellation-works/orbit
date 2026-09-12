---
title: Operation Mode — Design
owner: codex
last_updated: 2026-09-07
last_validated: 2026-09-07
status: Accepted
feature: operation-mode
doc_role: design
type: design
summary: Source-verified operation controls, PR delivery and review sweep seams, and gaps for the proposed review policy.
tags: [operation-mode, pipelines, configuration, review-policy]
paths: ["crates/orbit-core/assets/jobs/**", "crates/orbit-core/src/adapter/engine_host/v2_host/**", "crates/orbit-config/src/**"]
related_features: [activity-job, routines, auditability]
related_artifacts: [ORB-11314, ORB-11316]
---

# Operation Mode — Design

**Proposed feature; documentation only.** This file describes existing seams
verified at checkout `8da5a925f313ac9ae8ac29f8dcf0b9c56c869656` on 2026-09-05.
The review/delivery inventory in section 5 was verified for [ORB-11316] at
`32da9a9e57a7912fa71c665b0bdecde8fc014bf4` on the same date.
Since [ORB-11332] the `[operation]` settings, grants, and grant-bound drains
described in [Operations](./5_operations.md) exist; the seams below are the
ones that implementation composed over and remain accurate for unbound runs.
New behavior beyond that belongs in [the proposal](./3_vision.md). Versioned workspace resources are evidence of
this checkout's configuration, not proof that a host clock is currently running.

## 1. Configuration and resource ownership

[The configuration loader](../../../crates/orbit-config/src/layering.rs) merges
global and workspace TOML and reports each effective key's source. Registered
table values replace as a unit; crew fields layer recursively. Certain sandbox,
approval, and environment keys require workspace restatement. Thus an operation
mode should use the [fixed key registry](../../../crates/orbit-config/src/registry.rs)
and explicit source provenance, with authority validated in Core rather than
in CLI argument glue.

The checkout's [workspace config](../../../.orbit/config.toml) sets
`workflow.base_branch = "agent-main"`. The generic
[PR job](../../../crates/orbit-core/assets/jobs/task_pr_pipeline.yaml) has a
`main` fallback, so callers must continue resolving the workspace branch rather
than copying a generic default. Orbit task PRs target `agent-main`.

[Architecture](../../../ARCHITECTURE.md) assigns configuration to `orbit-config`,
application policy/composition to `orbit-core`, execution/retry mechanics to
`orbit-engine`, persisted contracts/drivers to `orbit-store`, and thin command
adapters to the CLI. The proposal can stay within existing dependency directions.
It would extend existing run/task records, not add a parallel policy database.
Task-pilot now persists its assessed complexity and modification selectors
through the existing task-bundle mutation boundary; broader operation-mode
policy remains subject to later implementation review.

## 2. Preparation and its narrow promotion exception

[Task-pilot](../../../crates/orbit-core/assets/jobs/task_pilot_pipeline.yaml)
already implements deterministic prepare → read-only worker partitions →
deterministic apply → success guard. Automatic discovery chooses proposed/backlog
tasks whose `context_files` are empty or whose complexity is unassessed,
excluding no-diff tags; explicit IDs can audit tasks with populated selectors.
Automated scanner locations stay in the task description as evidence rather
than being minted as guessed modification targets. Limits are 50 tasks,
partitions of five, five
pilot workers, and three active pipeline runs. The worker uses the `system` crew,
so this mechanism need not consume Astra for routine inspection.

[Source preparation](../../../crates/orbit-core/src/adapter/engine_host/v2_host/task_pilot/source.rs)
fetches and pins a landing-branch revision without moving primary HEAD. The
[pilot contract](../../../crates/orbit-core/assets/activities/task_pilot.yaml)
has no lifecycle or repository-write authority. It reports scope, dependency,
duplicate, already-landed, utility, and current-contract findings as data.

[Apply](../../../crates/orbit-core/src/adapter/engine_host/v2_host/task_pilot/apply.rs)
validates selectors at the pinned source, deterministically normalizes unambiguous
bare file/directory targets, rejects untrusted partition identities, isolates
invalid/stale tasks, and rechecks task state under write locks. Each accepted
task mutation carries a stable operation identity; the task envelope and durable
receipt event share one bundle commit point, so replay reports `already_applied`
without another mutation. Invalid assessments alone enter one targeted repair
fan-out carrying their exact validation errors and the original pinned revision;
stale and storage-failed tasks require fresh preparation, and successful siblings
are not sent back to a model. The prepared material fingerprint covers task
meaning and dependency evidence, while `task_snapshot_drift` also names direct
context, status, title, and tag changes in its structured stale outcomes.
Ordinary apply atomically changes selectors and assessed complexity while
leaving crew recommendations advisory. The receipt event retains the complete
assessment rationale, confidence, evidence gaps, validation approach, and
reassessment triggers. Missing evidence leaves complexity `unassessed`, and
automatic implementation admission excludes that task until preparation can
produce an assessed result, unless it carries the exact `no-diff-expected` tag
— operational work whose durable result is not a repository diff is admitted on
the tag alone and routes on its configured crew or the workspace default
[ORB-12118]. Priority remains an independent urgency signal.
There is no reusable general promotion-readiness certificate today.

The [CI-failure admission seam](../../../crates/orbit-core/src/adapter/engine_host/v2_host/ci_failure_admission.rs)
is a **shipped narrow exception**: explicit `promotion_authorized`, matching
immutable CI filing evidence, a proposed repair, actionable selectors, and no
warning/duplicate/already-landed findings allow promotion. General autonomous
promotion would extend this concept without pretending the CI-specific input
already grants authority for arbitrary tasks.

## 3. Scheduling, delivery, and live controls

| Seam | Verified behavior |
| --- | --- |
| [Seeded task-pilot routine](../../../crates/orbit-core/assets/routines/task_pilot.yaml) | Disabled by default; cron `*/40 * * * *`, missed runs skipped, overlap forbidden, 90-minute timeout. Its four-hour prose comment is stale; the cron is authoritative. |
| [Workspace pilot routine](../../../.orbit/routines/task_pilot.yaml) | Enabled in the checked-in definition, with the same cron. `*/40` fires at minutes 0 and 40 each hour, giving alternating 40/20-minute gaps, not a uniform 40-minute interval. |
| [Workspace triage](../../../.orbit/routines/task_triage.yaml) / [ship sweep](../../../.orbit/routines/ship_sweep.yaml) | Both disabled in this checkout. Mode configuration would need deliberate scheduler integration, not just a faster config number. |
| [Auto CLI](../../../crates/orbit-cli/src/command/run/auto.rs) | `--complete` is **off by default**; it authorizes completion for all tasks admitted during the window, including later backlog arrivals. It never approves proposed work. No window means one tick. |
| [Workspace auto job](../../../crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml) | One coordinator, default five live leaf runs, detached children, slot polling at 30 seconds and idle polling at 60 seconds; one epic may run alongside leaves. A successful coordinator is not evidence that its detached children succeeded. |
| [Task auto job](../../../crates/orbit-core/assets/jobs/task_auto_pipeline.yaml) | Owns the existing route into gated task delivery; ten active runs is a job ceiling, not a promise of ten simultaneous useful workers. Nested fan-outs also exist. |
| [PR job](../../../crates/orbit-core/assets/jobs/task_pr_pipeline.yaml) | Separate commit, branch synchronization, push, PR creation, promotion to review, and optional completion phases. `completion: review` is the default; `done` enters PR completion. |
| [Live concurrency](../../../crates/orbit-core/src/application/job/run/worker_limit.rs) | Audited run-state override with optional expected revision; absolute writes otherwise use last-writer-wins. Lowering the limit cancels no child and changes no completion authority/deadline. |
| [Admissions stop](../../../crates/orbit-core/src/application/job/run/admissions_stop.rs) | Workspace-claim protected, durable stop for new admissions; existing children retain authority. Repeated stop is a no-op. Cancellation is a separate operation. |

The proposal must also retain [context conflict checks](../../../crates/orbit-core/src/adapter/engine_host/v2_host/workspace_auto.rs)
and the existing transactional admission path. A run-scoped ceiling is not a
replacement for reservations or for counting claims across coordinators.

## 4. Recovery and telemetry

[Engine recovery](../../../crates/orbit-engine/src/activity_job/job_executor/recovery.rs)
already selects step/job recovery hooks and permits one post-recovery attempt.
PR conflict recovery requires the typed recoverable-conflict error. The
[generic hook](../../../crates/orbit-core/assets/activities/step_failure_recovery.yaml)
can diagnose and repair delivery; its returned summary is advisory, and durable
task/run/Git state establishes the outcome. Existing recovery must not be
described as currently disabled merely because there is no autonomous preset.

[Task triage](../../../crates/orbit-core/assets/jobs/task_triage_pipeline.yaml)
already separates diagnosis from deterministic application. It considers tasks
blocked by coupled failed/timeout/cancelled runs, excludes human blocks, and
permits environmental re-backlog within a durable budget (default two).
Other findings remain blocked. A shared budget across step recovery, resumed
runs, and triage is a proposed addition, not a shipped guarantee.

[Reliability metrics](../../../crates/orbit-core/src/metrics/reliability.rs)
report settled-run failure rates, excluded outcomes, low samples, and recovery
engagement per step invocation/per recorded run. Recovery counts are inferred
from catalog roles; activities used in both roles are disclosed as ambiguous.
Engagement is not a recovery success rate. The
[step metrics contract](../../../crates/orbit-types/src/telemetry/metrics.rs)
includes tool invocations, optional token usage/duration, and retry counts.
[Knowledge ingestion](../../../crates/orbit-core/src/metrics/ingest.rs) no longer
populates read-token/double-read gauges for new runs. None of these measures
alone identifies an external Astra session's mechanical turns or proves total
cost per accepted change.

## 5. Existing review and delivery evidence

The [PR pipeline](../../../crates/orbit-core/assets/jobs/task_pr_pipeline.yaml)
currently runs implementation → commit → prepare branch → synchronize base →
push → open/reuse PR → promote tasks, followed by optional completion. It has
no fresh code-review/repair stage. `completion: review` stops at handoff; the
word `review` is not a reviewer verdict. The
[completion implementation](../../../crates/orbit-engine/src/executor/automation/vcs/pr/complete.rs)
checks GitHub's merged state before completing tasks, but that is not a
certificate of reviewed content. Base synchronization and conflict recovery can
change the candidate after implementation. The new gate must therefore cover
delivery transformations as well as the initial implementation diff.

The [existing review instructions](../../../crates/orbit-core/assets/skills/orbit/references/task-review.md)
are read-only: check spec compliance before quality, report findings, and do not
approve or transition the task. A reviewer allowed to repair is a **new bounded
activity contract**, not permission implied by today's reviewer label/profile.
Existing `allowed_crews` propagation in the PR job is a constraint to preserve;
it does not select a distinct review crew today.

The seeded [code-review auto-task](../../../crates/orbit-automation/assets/auto_tasks/code-review.yaml)
is disabled, scheduled by cron, and assigned to `system`. Its prompt finds the
newest completed current/legacy sweep, reads the cursor from its execution
summary, reviews the integration-branch range, and files confirmed findings.
With no prior sweep it seeds current HEAD and stops. It examines interactions
across merged changes; it neither directly repairs them nor filters by a
before-PR coverage record. These are portable defaults, not a claim about live
workspace enablement or user-edited definitions.

The [auto-task scheduler](../../../crates/orbit-automation/src/auto_tasks/scheduler.rs)
mints tasks when the [time schedule](../../../crates/orbit-automation/src/auto_tasks/schedule.rs)
is due, with `skip_if_open` dedupe. Its
[cursor state](../../../crates/orbit-automation/src/auto_tasks/scheduler.rs)
records baseline, last slot/fire, and last minted task. A fire checkpoint is
not successful review coverage; task creation and cursor update are separate
operations, with checkpoint errors reported. Delivery thresholds, immutable
review batches, and atomic review-coverage acceptance are proposed work.

The independently seeded [QA sweep](../../../crates/orbit-automation/assets/auto_tasks/qa-sweep.yaml)
is also disabled and time-scheduled. It asks workers to build/run affected user
paths and file real issues; rerunning existing tests alone is insufficient.
Review coverage must not suppress that distinct integrated-behavior check.

## 6. Concerns & Honest Limitations

This inspection reads source and checked-in resources, not live host telemetry.
It establishes extension seams, not that enabling a preset is already safe.
General grant enforcement, exact preparation freshness at promotion, the
admission policy snapshot, and cross-retry budgets shipped in [ORB-11332];
the before-PR review gate, lineage review budgets, and exact-tree delivery
coverage shipped in [ORB-11333] ([Operations §10](./5_operations.md));
provider cost accounting did not. Scheduler
timing depends on the external sweep clock, host role/pin, and capacity.
Current numeric defaults describe this revision and may change independently.
This inspection predates [ORB-11333]; the PR job now carries the review gate
and the delivery evaluator consumes certificate-backed exclusions.

## Task References

- [ORB-11314] — verifies current controls to ground the operation-mode proposal.
- [ORB-11316] — verifies review, delivery, scheduler, and QA extension seams.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
