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
| `task_pilot_pipeline` | Read-only agent preflight plus deterministic validated-selector apply; it defaults to no lifecycle promotion. An omitted optional `base_branch` binds as empty at the prepare activity boundary, then preparation resolves the owning workspace's `[workflow] base_branch`; pass a non-empty run input to inspect another branch. |
| `task_triage_pipeline` | Diagnose tasks blocked by failed runs. |
| `epic_pipeline` | Ship an epic and its descendants against one worktree. |
| `workspace_ship_pipeline` / `workspace_auto_pipeline` | Workspace-scoped wrappers that resolve mode and base branch, then invoke the pipelines above. |
| `auto_task_scheduler_pipeline` | Mint tasks from due auto-task definitions. |
| `ci_failure_sweep_pipeline` | File GitHub Actions findings as proposed, pilot them, and admit only current warning-free repairs to backlog; never implements them. |
| `dependabot_alert_sweep_pipeline` | Collect Dependabot/code/secret-scanning evidence and file remediation tasks. |
| `worktree_gc_pipeline` | Reclaim settled worktrees. |
| `agent_invoke_pipeline` | One operator-admitted agent invocation for exploration or debugging, run on the host outside the executor sandbox. Submit it with `orbit run agent` / `orbit_agent_invoke`, never `orbit run job`: it needs a per-invocation local admission or an explicit workspace-scoped remote callers-file grant (strict key-bound by default, or explicitly cooperative on a same-account SSH operator channel), changes no task, and is not resumable. See [tool-surface.md](tool-surface.md). |

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
run state retain the pilot run ID, result, and admission decision. Filing
clusters failures by a normalized error signature that prefers a concrete test
or panic identity over ANSI styling, generic runner/cargo/nextest trailers, and
assertion payload help text; the raw excerpt stays in the description.

CI evidence schema 2 binds each failure row to one failed job: both log scopes
select that job, and checkout provenance carries its job ID. A separate
`diagnostic_unit` retains a complete runner command from its `Run` group through
its nonzero process completion, up to 256 KiB (`kind: runner_command`,
`complete: true`). When retention exceeds that limit, collection can instead
supply `kind: runner_failure_regions`, `complete: false`, with
`command_complete: true` and `selection_complete: true`: the command boundaries
and every recognized failure anchor were scanned, but the command was not fully
retained. Regions keep the command header, anchored test failures, panics,
assertions and compiler errors, their next 11 context lines, summaries and exit.
Gaps carry byte-omission markers; large left/right assertion payloads retain a
512-byte prefix and explicitly count omitted payload bytes. The structured
`command_bytes`, `retained_source_bytes`, `omitted_bytes`,
`assertion_payload_omitted_bytes` and `failure_anchor_count` make these limits
auditable. A failure-anchor count includes repeated reports of the same test.
The standalone log tool exposes this alternative as `failure_regions`.

Exactly one failing command and one known failed step are required; all primary
command log columns, including omitted lines, must match that job and step.
Filing validates the evidence contract and uses selected evidence for signatures;
`log_excerpt`, `log_truncated`, and byte counts still describe the head/tail
display. Complete command descriptions use a 4,000-byte diagnostic display cap.
Failure-region descriptions retain the entire bounded selection (at most 64 KiB)
and its omission accounting so offline workers receive every selected failure.
Partial command retention cannot establish a complete compiler set for cross-job
compiler deduplication. Existing complete-command compiler proofs are unchanged.

Each process stdout read stops with a retryable error beyond 8 MiB (at most one
4 KiB lookahead chunk); the existing process timeout still applies. Command
selection uses at most two 256 KiB full-command buffers, two 64 KiB region
buffers and a 16 KiB line buffer. Only left/right assertion payloads may exceed
the line buffer; other overlong/invalid lines fail closed. Explicit source
truncation notices, missing command boundaries, multiple failing commands,
unknown/ambiguous failed steps, incomplete checkout identity, exhausted read
budgets and failure-region overflow defer that job. Oversized commands are never
labelled fully retained. No extra queries or retries
are introduced; complete siblings still file. `max_job_log_reads` caps failed-job
reads across the snapshot (default
6, maximum 25); `max_checkout_log_reads` separately caps additional same-job
checkout reads (default 3, maximum 25). The rotating investigation slot also
rotates overflow jobs in stable ID order. Legacy schema-1 failures require
recollection because their run-wide logs and checkout scans cannot establish
job attribution. Neither event nor PR head can substitute for runner checkout.

Read-only verification: inspect the run's job metadata with `gh run view <run>
--json databaseId,jobs --repo <owner/repo>`, then use the production bounded log
reader with explicit `run`, `job`, and `scope` for each failed job. Compare each
row's job ID, diagnostic, and checkout provenance with that supplying job; repeat
with reversed metadata order and a one-job budget. Do not download whole logs
into memory or use a sweep that files tasks merely to verify collection.

### Completed CI repair reassessment

When open-owner lookup misses, filing examines at most eight completed exact-key
owners and attempts at most 32 assessments per snapshot. Metadata lookup is
bounded before task hydration. Each owner may carry `ci-repair-assessment.json`
(schema version 1) in its existing task artifacts. Completion, ancestry, a shared
key/test/path, task summaries and pilot rationale are discovery hints; none is
coverage proof by itself. The introducing seam was the original filer's exclusion
of completed owners, preserved by the later shared open-owner lookup.

The assessment contains these required fields:

- `schema_version: 1`, `task_id`, `failure_key`, `delivery_run_id`,
  `delivery_step_index`, and `landed_revision` (full commit SHA).
- `observations`: at most 32 records with string `run_id`, `job_id`, `checkout`,
  `diagnostic_sha256`, `branch`, and `ref_kind` (`integration` or `release`). The
  digest is full SHA-256 of the selected diagnostic unit's exact UTF-8 bytes,
  or the complete untruncated excerpt when no selected unit exists. No signature
  normalization is applied. Source run/job/checkout and diagnostic bytes must
  match the collector snapshot, including retained assertion details and omission
  accounting enforced by the collection contract.
- `before` and `after`: references `{path, sha256}` to validation JSON artifacts
  on that same owner. Paths are relative to its artifact bundle; digests bind
  the exact file bytes. Artifacts are limited to 1 MiB each.
- `command`: the same nonempty argv array in both validation records;
  `diagnostic_details`: concrete assertion/error substrings present in every
  observed diagnostic and failing validation output, absent from passing output;
  `coverage_reason`: why the repair fixes those particular details.

Each validation artifact has `schema_version: 1`, `task_id`, `revision`,
`command`, `exit_code`, `outcome`, `origin`, `recorded_at` (RFC3339), and captured
`output`. The before result must be `failed` with nonzero exit at the observed
checkout; the after result must be `passed` with zero exit at exactly the landed
revision. Both must identify the same owner and command. `origin` is `recovered`
for retained execution evidence or `retrospective` for a newly executed check.
An assessment never executes argv or instructions copied from a log.

The owner's PR delivery run must exist, succeed, match its recorded `job_run_id`,
and assign that owner in its original `task_ids`. The referenced successful
completion step must have `phase: complete`, `merge.merged: true`, the same
`merge.landed_commit`, and that owner among `completed_task_ids`. Aggregate
pipeline fields or agent result prose cannot substitute for that step. Git must
confirm that the observed checkout strictly predates the landed revision. The collector's observed branch head and the
available local remote-tracking (or local) branch must both contain the repair.
Git inspection reuses the bounded source reader (two seconds per command, thirty
seconds total). Missing objects or branch refs are unavailable evidence, never
an inferred ancestry success. A post-fix recurrence, changed diagnostic, or branch
without the repair therefore remains actionable. Existing release/integration
pilot dispositions and promotion authority remain unchanged.

For insufficient historical records, inspect the original collector/run evidence
and the covering delivery. Recover actual command/result evidence when available.
Otherwise choose the narrow faithful command from repository instructions and
code, reproduce at the immutable pre-fix revision and validate at the exact landed
revision in isolated extracts or fixtures with independent build outputs, and
capture both outputs as explicitly retrospective records. Attach those records and the structured assessment through
`orbit.task.artifact.put`; do not invent historical execution artifacts or edit
run state. Then reassess the same collector snapshot through the filing path.
The original insufficient state remains `unresolved` until the referenced proof
is available. An existing open owner still takes precedence.

Coverage appears in `repair_assessments` and `skipped_existing` as
`covered_by_repair`, with the owner, source provenance and evidence references.
Filing retains an idempotent `ci-repair-observations/<digest>.json` receipt on the
completed owner without changing its meaning or lifecycle, creating another
repair task, or sending it to pilot/implementation admission. Unavailable,
contradictory and over-budget assessments instead report `unresolved` with a
bounded reason and leave ordinary proposed filing and pilot checks available.
There is no extra incident store, scheduler, automatic revalidation command or
unbounded historical scan.

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
