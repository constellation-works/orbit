---
title: Operation Mode — Design
owner: codex
last_updated: 2026-09-05
last_validated: 2026-09-05
status: Draft
feature: operation-mode
doc_role: design
type: design
summary: Source-verified existing controls and gaps that a proposed operation mode would compose.
tags: [operation-mode, pipelines, configuration]
paths: ["crates/orbit-core/assets/jobs/**", "crates/orbit-core/src/adapter/engine_host/v2_host/**", "crates/orbit-config/src/**"]
related_features: [activity-job, routines, auditability]
related_artifacts: [ORB-11314]
---

# Operation Mode — Design

**Proposed feature; documentation only.** This file describes existing seams
verified at checkout `8da5a925f313ac9ae8ac29f8dcf0b9c56c869656` on 2026-09-05.
There is no implemented global operation-mode setting. New behavior belongs
in [the proposal](./3_vision.md). Versioned workspace resources are evidence of
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
Exact persisted fields and migrations require a later implementation review.

## 2. Preparation and its narrow promotion exception

[Task-pilot](../../../crates/orbit-core/assets/jobs/task_pilot_pipeline.yaml)
already implements deterministic prepare → read-only worker partitions →
deterministic apply → success guard. Automatic discovery chooses proposed/backlog
tasks with empty `context_files`, excluding no-diff tags; explicit IDs can audit
tasks with populated selectors. Limits are 50 tasks, partitions of five, five
pilot workers, and three active pipeline runs. The worker uses the `system` crew,
so this mechanism need not consume Astra for routine inspection.

[Source preparation](../../../crates/orbit-core/src/adapter/engine_host/v2_host/task_pilot/source.rs)
fetches and pins a landing-branch revision without moving primary HEAD. The
[pilot contract](../../../crates/orbit-core/assets/activities/task_pilot.yaml)
has no lifecycle or repository-write authority. It reports scope, dependency,
duplicate, already-landed, utility, and current-contract findings as data.

[Apply](../../../crates/orbit-core/src/adapter/engine_host/v2_host/task_pilot/apply.rs)
validates selectors at the pinned source, isolates invalid/stale partitions, and
rechecks task state under write locks. Its `task_snapshot_drift` compares context,
status, title, and tags; it is not a full description/criteria/plan freshness
check. Ordinary apply changes selectors, leaving orchestration recommendations
advisory. There is no reusable general promotion-readiness certificate today.

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
| [Workspace pilot routine](../../../.orbit/routines/task_pilot.yaml) | Enabled in the checked-in definition, with the same cron and a host pin. `*/40` fires at minutes 0 and 40 each hour, giving alternating 40/20-minute gaps, not a uniform 40-minute interval. |
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

## 5. Concerns & Honest Limitations

This inspection reads source and checked-in resources, not live host telemetry.
It establishes extension seams, not that enabling a preset is already safe.
General grant enforcement, comprehensive preparation freshness, an admission
policy snapshot, and cross-retry budgets still need implementation. Scheduler
timing depends on the external sweep clock, host role/pin, and capacity.
Current numeric defaults describe this revision and may change independently.

## Task References

- [ORB-11314] — verifies current controls to ground the operation-mode proposal.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
