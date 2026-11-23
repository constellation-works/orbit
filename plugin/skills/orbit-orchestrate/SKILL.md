---
name: orbit-orchestrate
description: Prepares and supervises an Orbit backlog. Deduplicates and pilots tasks (task-pilot), assigns crews, promotes ready work, dispatches with `orbit run ship` and `orbit run auto` drains, diagnoses failed or stuck `jrun-*` runs, and routes CI/QA sweep findings into repair tasks. Use for delivery operations across tasks, jobs, activities and workflow runs. Use orbit for one assigned task and orbit-setup for machine configuration or scheduler installation.
---

# Orbit Orchestrate

Keep authorized work moving using durable task, run and delivery evidence.
A managed implementation worker stays within its assigned task; this skill
is for the operator supervising work.

## Operating loop

1. Inspect the owning workspace with `orbit attention --json` and `orbit run readiness --json`.
2. Before filing a finding, search the owning task store with `orbit tool run orbit.search --input '{"query":"<distinctive terms>","kind":"task","all":true,"workspace":"<selector>"}'`.
3. For authorized filing, create the task as `proposed`. Use `orbit tool run orbit.task.add --input '{"workspace":"<selector>","title":"<title>","description":"<description>","complexity":"<level>"}'` with the task fields described in [Search, then author](references/loop-author.md#search-then-author).
4. Prepare the task with `orbit run task-pilot <task-id>`. Inspect the task and its assessment with `orbit task show <task-id>` before choosing a crew or promoting it.
5. Record the selected crew with `orbit tool run orbit.task.update --input '{"id":"<task-id>","crew":"<crew>"}'` when the task needs a crew assignment. Follow [Select an allowed crew](references/loop-crew.md#select-an-allowed-crew).
6. With promotion authority and readiness confirmed, run `orbit task update <task-id> --status backlog`. With dispatch authority, run `orbit run ship <task-id>` or `orbit run auto --for <window>`.
7. Verify delivery with `orbit run show <run-id> --json` and `orbit task show <task-id>`. Route failures through the reference that matches the evidence.

## Rules for supervision

- Discover the owning workspace and use its returned selector; see
  [tool surface](../orbit/references/tool-surface-routing.md). Never substitute a local
  store for an unavailable owner.
- Follow the user's scope, crew choices and concurrency/build budget. After
  authorized preparation succeeds, promote promptly. An active drain can claim
  backlog work immediately, so finish scope changes before exposing it.
- Creation, promotion and completion are distinct. `--complete` authorizes
  delivery for work admitted in that invocation's window; it does not itself
  authorize promotion, future windows or releases; `run auto --approve-proposed`
  is the separate opt-in for a drain to approve qualifying proposed tasks, and
  `run auto --auto-fix` the separate opt-in to repair its own PRs' failed
  required checks and conflicts, which never merges.
- Existing continuous-delivery authorization may cover them; apply the user's
  actual instructions.
- Verify persisted preparation, actual diffs, required checks and merge state.
  Post-merge review/QA findings become repairs under continuous delivery; do not
  invent an extra pre-merge approval gate. Repository protections still apply.
- Start every supervision pass (each wake of a long-running session) with
  `orbit.attention` (`orbit attention`) on the owning host for the workspaces
  you supervise.
- Clear each `pilot_warning` and `pilot_hold`: repair the task
  (criteria, context, dependencies; `required_tools` is set only at filing, so
  re-file to change it), then re-pilot only if the hold stays, or record on the
  task why it stays. A `context_files` edit by you on a proposed or backlog task
  re-pilots it on its own.
- Treat an empty result as clear only when `degraded` is empty and the read
  reached the owner: a federated read that misses the owner can return
  nothing. A caller holding neither the agent nor the operator capability
  is refused with `capability_denied` and does not receive an empty
  attention envelope.
- Diagnose blocked work before retrying. Preserve user interventions and active
  work. A stopped admission loop does not mean its children were cancelled.
- Record `orchestrator` separately from execution `crew`. Neither attribution
  field grants authority. Leave a durable handoff when a lane cannot proceed.

## Choose the reference

| Need | Read |
|---|---|
| Scope completion authority and dispatch windows | [`references/authorization.md`](references/authorization.md) |
| Match an observed failure to a remedy | [`references/common-failures.md`](references/common-failures.md) |
| Deduplicate or file a task | [`references/loop-author.md`](references/loop-author.md) |
| Pick a crew, take over a task or record who orchestrated it | [`references/loop-crew.md`](references/loop-crew.md) |
| Establish authority and inspect workspace readiness | [`references/loop-inspect.md`](references/loop-inspect.md) |
| Pilot, promote and dispatch ready work | [`references/loop-promote.md`](references/loop-promote.md) |
| Reconcile an open PR that predates the drain | [`references/loop-stalled-prs.md`](references/loop-stalled-prs.md) |
| Correct course while a task is running, verify steering delivery or configure its bounds | [`references/mid-run-steering.md`](references/mid-run-steering.md) |
| Compare implementer models when selecting a crew | [`references/model-ranking.md`](references/model-ranking.md) |
| Choose a recent-run count for analytics | [`references/orchestration-analytics-windows.md`](references/orchestration-analytics-windows.md) |
| Authorize auto-fix or interpret its repair limits | [`references/orchestration-auto-fix.md`](references/orchestration-auto-fix.md) |
| Read the drain card's backlog forecast | [`references/orchestration-backlog-forecast.md`](references/orchestration-backlog-forecast.md) |
| Interpret delivery behavior and verify completion | [`references/orchestration-completion.md`](references/orchestration-completion.md) |
| Ship tasks or start an authorized drain | [`references/orchestration-dispatch.md`](references/orchestration-dispatch.md) |
| Resize, extend, stop or read the outcome of a live drain | [`references/orchestration-drain-control.md`](references/orchestration-drain-control.md) |
| Start or stop a kept drain, or resolve a paused or open-breaker hold | [`references/orchestration-drain-keeper.md`](references/orchestration-drain-keeper.md) |
| Use a crew allowlist or select the owner of a pull drain | [`references/orchestration-drain-routing.md`](references/orchestration-drain-routing.md) |
| Requeue or resume a task after a failed run | [`references/orchestration-failed-runs.md`](references/orchestration-failed-runs.md) |
| Add an authorized review to one run or diagnose a review refusal | [`references/orchestration-per-run-review.md`](references/orchestration-per-run-review.md) |
| Run or re-run task-pilot, interpret stale assessments or admit a task with an empty footprint | [`references/orchestration-pilot-admission.md`](references/orchestration-pilot-admission.md) |
| Trace the pinned pilot checkout, routine and partition outcomes | [`references/orchestration-pilot-apply.md`](references/orchestration-pilot-apply.md) |
| Use --approve-proposed and interpret the resulting pilot decisions | [`references/orchestration-proposed-approval.md`](references/orchestration-proposed-approval.md) |
| Explain drain eligibility, capacity and waits | [`references/orchestration-readiness.md`](references/orchestration-readiness.md) |
| Diagnose overlapping reservations or size a task hierarchy | [`references/orchestration-reservations.md`](references/orchestration-reservations.md) |
| Inspect admission's CPU and memory forecasts and accuracy | [`references/orchestration-resource-forecast.md`](references/orchestration-resource-forecast.md) |
| Admit a retry of a stopped owner landing | [`references/orchestration-retry-landing.md`](references/orchestration-retry-landing.md) |
| Operate under a workspace claim or supervise an unattended window | [`references/orchestration-unattended.md`](references/orchestration-unattended.md) |
| Investigate a blocked task or requeue a failed run | [`references/recovery-blocked-run.md`](references/recovery-blocked-run.md) |
| Check finding freshness, record operator coverage or file a repair | [`references/recovery-ci-findings.md`](references/recovery-ci-findings.md) |
| Verify that a landed repair is deployed | [`references/recovery-deployment.md`](references/recovery-deployment.md) |
| Inspect a completion failure after PR publication | [`references/recovery-pr-completion.md`](references/recovery-pr-completion.md) |
| Decide who may dispatch and implement a repair | [`references/recovery-repair-authority.md`](references/recovery-repair-authority.md) |
| Resume a stopped candidate or salvage an interrupted review repair | [`references/recovery-salvage-candidate.md`](references/recovery-salvage-candidate.md) |
| Close a rescued blocked task after a verified landing | [`references/recovery-settle-rescued-task.md`](references/recovery-settle-rescued-task.md) |
| Admit, deliver or recover an explicit source repair | [`references/recovery-source-repair.md`](references/recovery-source-repair.md) |
| Tell a working agent from an abandoned wrapper or verify model routing | [`references/run-debugging-agent-processes.md`](references/run-debugging-agent-processes.md) |
| Interpret JSON from run show, logs, events or trace | [`references/run-debugging-audit-output.md`](references/run-debugging-audit-output.md) |
| Read the v2 audit trail, transcript blobs and failure classes | [`references/run-debugging-audit-trail.md`](references/run-debugging-audit-trail.md) |
| Find, answer or verify a decision request on held work | [`references/run-debugging-decisions.md`](references/run-debugging-decisions.md) |
| Read a typed delivery refusal and choose the next step | [`references/run-debugging-delivery-holds.md`](references/run-debugging-delivery-holds.md) |
| Check branch, divergence and conflict evidence for a failed run | [`references/run-debugging-git-state.md`](references/run-debugging-git-state.md) |
| Trace parent and child failures or interpret history, incidents and failure rows | [`references/run-debugging-history.md`](references/run-debugging-history.md) |
| Tell an inherited red base from candidate damage in hosted CI | [`references/run-debugging-hosted-checks.md`](references/run-debugging-hosted-checks.md) |
| Verify a recorded PID or clean up a process group after authorized cancellation | [`references/run-debugging-live-processes.md`](references/run-debugging-live-processes.md) |
| Inspect a base refresh or confirmed hold after an unchanged-head rerun fails | [`references/run-debugging-no-edit-rerun.md`](references/run-debugging-no-edit-rerun.md) |
| Inspect step recovery attempts before reading raw audit files | [`references/run-debugging-recovery-evidence.md`](references/run-debugging-recovery-evidence.md) |
| Write and verify a run diagnosis using the report template | [`references/run-debugging-report.md`](references/run-debugging-report.md) |
| Check whether a corrected reply triggered another attempt | [`references/run-debugging-response-correction.md`](references/run-debugging-response-correction.md) |
| Route check or conflict repairs for review deliveries without a handoff | [`references/run-debugging-review-delivery.md`](references/run-debugging-review-delivery.md) |
| Check task state, historical invocations or replay a pilot assessment | [`references/run-debugging-task-state.md`](references/run-debugging-task-state.md) |
| Identify the first failing step from run state, audit events and captured logs | [`references/run-debugging-triage.md`](references/run-debugging-triage.md) |
| Investigate an agent step stopped or refused with upgrade_pending | [`references/run-debugging-upgrade-refusals.md`](references/run-debugging-upgrade-refusals.md) |
| Diagnose a failed local required-validation command and its replay evidence | [`references/run-debugging-validation-replay.md`](references/run-debugging-validation-replay.md) |
| Choose checkpoint resume or a replay from step zero | [`references/run-debugging-whole-run-replay.md`](references/run-debugging-whole-run-replay.md) |
| Work through typical orchestration decisions | [`references/walkthroughs.md`](references/walkthroughs.md) |
| Author or diagnose version 3 auto-task jobs, waits and accepted results | [`references/workflows-auto-tasks.md`](references/workflows-auto-tasks.md) |
| Cancel a run or choose an inspection route for a stuck run | [`references/workflows-cancel-and-diagnose.md`](references/workflows-cancel-and-diagnose.md) |
| Check CI log selection, completeness and checkout attribution | [`references/workflows-ci-log-evidence.md`](references/workflows-ci-log-evidence.md) |
| Check whether a fresh failure warrants reassessing a completed repair | [`references/workflows-ci-reassessment.md`](references/workflows-ci-reassessment.md) |
| Trace a CI sweep from filing through pilot admission and host routing | [`references/workflows-ci-sweep.md`](references/workflows-ci-sweep.md) |
| Inspect resource overrides or author a version 2 job | [`references/workflows-custom-jobs.md`](references/workflows-custom-jobs.md) |
| Trace completion and auto-fix inputs through the pipeline | [`references/workflows-delivery-inputs.md`](references/workflows-delivery-inputs.md) |
| Diagnose task-pilot start prerequisites or owner-only evidence | [`references/workflows-pilot-holds.md`](references/workflows-pilot-holds.md) |
| Re-run a task with a candidate or deliberately discard it | [`references/workflows-preserved-candidate.md`](references/workflows-preserved-candidate.md) |
| Inspect jobs and activities or submit and observe a job | [`references/workflows-run-jobs.md`](references/workflows-run-jobs.md) |
| Look up the purpose and authority of a shipped pipeline | [`references/workflows-shipped-pipelines.md`](references/workflows-shipped-pipelines.md) |

Use [task authoring](../orbit/references/task-authoring-contract.md) when filing work.
Use [orbit-setup](../orbit-setup/SKILL.md) when the cause is host configuration,
installation, scheduler setup or maintenance; do not load those procedures
for ordinary dispatch.
