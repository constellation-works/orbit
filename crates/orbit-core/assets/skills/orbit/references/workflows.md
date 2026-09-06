# Jobs, activities, and runs

Orbit's execution layer. This covers the mechanics; for deciding *what* to
dispatch see [orchestration.md](orchestration.md), and for scheduling it see
[automation.md](setup/automation.md).

## Concepts

- **Job** — a deterministic, multi-step pipeline (schemaVersion 2 YAML) from the
  installed catalog. Jobs compose activities. Discover with `orbit job list` /
  `orbit job show <id>`.
- **Activity** — one named step definition referenced by a job's step list
  (`agent_implement`, `task_pilot`, `git_commit`, `git_push`, `pr_open`,
  `worktree_setup`, `reserve_locks`, ...). Activities are never invoked directly
  by CLI; a job's step list references them. `orbit activity list` shows the
  catalog.
- **Run** — one execution, with a `jrun-*` id, a durable state bundle under
  `.orbit/state/job-runs/`, and an audit trail.

For every task-backed agent activity, Orbit computes
`effective_tools = deduplicate(activity.tools union task.required_tools)`. Task
requirements are immutable after creation. The
activity list remains the baseline; an empty task requirement list preserves it
exactly. When one agent activity selects a batch, Orbit unions the requirements
from every selected task into that same effective list. Admission rejects
invalid required names before provider launch, and
the run envelope, `ORBIT_ACTIVITY_TOOLS`, and audit evidence carry the effective
list. Tool inclusion does not bypass later role, capability, policy, sandbox,
subprocess, or authentication checks.

## Running a job

```bash
orbit job list                                   # catalog
orbit job show <job_id>
orbit run job <job_id> --input key=value --json
orbit run job <job_id> --input crew=<name> --json # override the run's crew for this run
orbit run job <job_id> --wait                    # block until terminal; nonzero unless it succeeded
orbit run history --json
orbit run history -j <job_id>
orbit run show <run_id> --json
```

There is no `--crew` flag on `run job` — crew selection is always a run input.
That picks the run's resolved crew (`resolved_run_crew` in `orbit run show
--json`); an individual activity can still route elsewhere via an explicit
activity `crew` or `system_crew: true`, which overrides even an explicit
request. [run-debugging.md](run-debugging.md#verify-model-routing-before-reading-logs)
covers reading `activity_provenance` for what actually dispatched.

**Runs are asynchronous by default.** `orbit run job` submits to a detached
worker and returns as soon as the run is durable — it prints the run id and the
inspection commands, and does *not* claim the eventual outcome. Add `--wait` to
block on it.

Equivalent catalog commands exist as `orbit job list|show|run|replay|resume`.
`replay` re-runs from step 0 against the current definition; `resume` creates a new linked run using persisted checkpoints where
resumable, preserving the original attempt. Read both run records; a resume is
not a rewrite of failed history.

## The shipped pipelines

| Job | Purpose |
|---|---|
| `task_pr_pipeline` | Implement a task in a worktree and open a PR. |
| `task_local_pipeline` | Implement in a worktree and merge to the configured local base without a PR; optional push. |
| `task_auto_pipeline` | Discover ready backlog tasks and ship them. |
| `task_gate_pipeline` | Gated shipment with windowing and starvation handling. |
| `task_pilot_pipeline` | Read-only agent preflight plus deterministic validated-selector apply; it defaults to no lifecycle promotion. Its zero-input source snapshot uses the owning workspace's `[workflow] base_branch`; pass a non-empty `base_branch` run input to inspect another branch. |
| `task_triage_pipeline` | Diagnose tasks blocked by failed runs. |
| `epic_pipeline` | Ship an epic and its descendants against one worktree. |
| `workspace_ship_pipeline` / `workspace_auto_pipeline` | Workspace-scoped wrappers that resolve mode and base branch, then invoke the pipelines above. |
| `auto_task_scheduler_pipeline` | Mint tasks from due auto-task definitions. |
| `ci_failure_sweep_pipeline` | File GitHub Actions findings as proposed, pilot them, and admit only current warning-free repairs to backlog; never implements them. |
| `dependabot_alert_sweep_pipeline` | Collect Dependabot/code/secret-scanning evidence and file remediation tasks. |
| `worktree_gc_pipeline` | Reclaim settled worktrees. |

Inspect any of them with `orbit job show <id>` before invoking — the step list is
the contract.

CI-sweep filing is deliberately non-executable: `file_ci_failure_tasks` always
creates `proposed` tasks. The CI job invokes `task_pilot_pipeline` for each new
task and retries matching tasks that a prior pilot left proposed, carrying
explicit promotion authority into its deterministic apply boundary. Invalid or
empty selectors, pilot failure, duplicates, already-landed
work, conflicts, and warnings leave that task proposed without blocking other
pilot children. A standalone task-pilot run has no promotion authority. The
source run/job/SHA/step remains in the task description, while parent and child
run state retain the pilot run ID, result, and admission decision.

### The `completion` input

Every pipeline above that ships a task takes a `completion` input, defaulting to
`review`. `orbit run ship --complete` / `orbit run auto --complete` set it to
`done` on the submitted run, and it propagates unchanged through
`workspace_auto_pipeline` → `task_auto_pipeline` → `task_gate_pipeline` → the
leaf pipelines, and into `epic_pipeline`. Because the workspace drain reads it
from its own input each iteration, work discovered mid-window inherits the same
authorization.

Under `completion: done`, the leaf pipelines gain a terminal step
(`task_complete`, or `pr_complete` for PR mode) that performs the guarded
`review -> done` transition — in local mode only after the merge and push steps
succeeded, and in PR mode only after the PR is verified merged. A run submitted
without the flag carries no `completion` key at all, so its persisted input is
identical to a pre-`--complete` submission. See
[orchestration.md](orchestration.md) for the authorization semantics.

## Cancelling

```bash
orbit run cancel <run_id>
```

For a run that is stuck rather than merely slow, diagnose before killing:
[run-debugging.md](run-debugging.md) covers matching a run id to its process
group and the safe termination order.

## Diagnosis

- A `jrun-*` id that failed, stuck, or was cancelled →
  [run-debugging.md](run-debugging.md) for the full flow: run bundle, audit
  trail, logs and blobs, failure classification, task and git state, kill
  procedure, report format.
- A known failure signature, once the failing step is identified →
  [common-failures.md](common-failures.md).
- Host-level incident, service warning, or missing run output →
  [operational-logs.md](operational-logs.md).

**Safety, up front:**

- Files under `.orbit/state/job-runs/` and `.orbit/state/audit/` are evidence.
  Never edit them to "fix" a run.
- Never kill a process before matching run id → `pid`/`pgid`/task id(s)/command;
  terminate the process group for that run id only. Never kill a parent auto or
  gate run without verifying it owns the same task(s).
- Top-level `state: failed` is not a diagnosis — find the first failed step or
  activity.
- Task state and run state are the durable handoff. Never parse agent prose in
  their place.
- If Orbit's own tooling or diagnostics mislead you, record friction
  ([friction.md](friction.md)).

## Custom jobs and resource overrides

Use `orbit job show <id>` to inspect effective installed job definitions and
`orbit activity list` to discover registered activities before changing them.
Workspace resource overrides can shadow shipped global resources, so the
binary's version alone does not prove which pipeline ran. `orbit workspace sync
--check` reports managed-resource drift; customized files are preserved for
deliberate reconciliation.

A job uses `schemaVersion: 2`, `kind: Job`, `metadata.name`, and a `spec` with
`default_input` and ordered `steps`. A simple step names an `id`, a
`target: activity:<name>`, and `default_input`. Templates can reference
`input.<name>` and `steps.<step-id>.output.<field>`. The shipped jobs demonstrate
conditionals, loops, retries, and recovery activities. Copy an installed example
that matches the intended operation; validate the effective catalog before
submitting it. A routine can only target a job, not an activity directly.

An agent step's brief, selected crew, filesystem profile, allowed tools, and
completion envelope are separate contracts. A successful provider exit alone
is insufficient when the step requires structured completion output. Keep
required tools exact and minimal, and declare read-only filesystem profiles
explicitly for inspection work. File meaningful failures rather than treating
an empty/malformed agent response as successful completion.
