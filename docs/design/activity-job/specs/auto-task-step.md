---
title: "Spec: Auto-task steps and durable waiting"
owner: codex
last_updated: 2026-10-30
last_validated: 2026-10-30
status: Draft
feature: activity-job
type: design
summary: "Auto-task targets in existing jobs, with durable CEL waiting, keyed task binding, accepted evidence and expiry; phases 1 and 2 built."
tags: [activity-job, condition-triggers, auto-tasks]
---

# Spec: Auto-task steps and durable waiting

This is an extension to existing jobs. Phase 1 (§7) has shipped:
`target: auto-task:<name>`, `wait_until`, `expire_when`, `reuse_when`,
`require_result` and the CEL facts below load, are checked and are explained.
Such a job declares `schemaVersion: 3` ([ORB-20237]). Phase 2 ([ORB-20158])
runs it: keyed bind or mint, parked waits, admission rechecks and
partial-branch checkpoints. No readiness adapter exists yet, so a step with
`require_result` holds. It replaces the unmerged proposal for
a separate auto-sequence definition type.
**Auto-sequence** may be an informal name for a job composing auto-tasks; it has
no separate directory, loader, trigger carrier, commands or lifecycle.

## Why This Exists

A condition-triggered routine should be able to start a `release-readiness` job
that obtains full QA and security evidence before invoking release preparation.
Those tasks can wait for an empty board, run in sequence or in parallel, reuse
recent evidence, and expire when the opportunity or maximum wait passes.

Existing jobs already own ordering, parallel joins, run identity, audit and
recovery. What is missing is a step that binds an auto-task and waits for its
accepted completion, plus durable conditional waiting. Source checked at
`8217460c0`: `JobV2StepBody` is in
`crates/orbit-types/src/workflow/activity_job/job_v2.rs`; named targets resolve in
`crates/orbit-engine/src/activity_job/catalog.rs`; `run_step` in
`crates/orbit-engine/src/activity_job/job_executor/step.rs` evaluates `when` once
and treats false as a successful skip. Waiting cannot reuse that meaning.

## 1. Job structure and the new target

Add `target: auto-task:<name>` as an authoring form in the existing job schema.
Core resolves the named definition and performs deterministic observation and
keyed task admission through the existing engine/Core boundary. This is not an
agent activity and never runs an agent to decide whether to mint.

The step obtains one compatible auto-task instance and waits for its durable
completion contract. It does not return success just because minting succeeded.
Its task is executed through normal approval, readiness, drain, host and
delivery policy; naming an auto-task is not dispatch or promotion authority.

Use existing job structure:

- A normal step list is sequential. A condition does not remove that order.
- Put independently eligible members inside an existing `parallel` block. Each
  branch may wait on its own condition. `join: { mode: all }` waits for all required
  branches before later steps; reuse existing `any`/`quorum` only when those
  weaker requirements are intentional.
- Do not introduce implicit dependency rewrites, another `needs` field, or a
  sequence-specific execution graph. `self.prev` means the previous sibling in
  the current sequential list; it is absent on a first/independent branch step.

An orchestration job does not acquire a task worktree or task delivery slot for
the children. Their bindings are step checkpoint data, not the parent run's
`input.task_ids` or `task_delivery` ownership. Parent failure must not apply
delivery finalization to unrelated child tasks.

## 2. CEL waiting preserves existing `when`

| Field | Meaning |
|---|---|
| Existing `when` | Evaluate once; false skips successfully, without retry. Existing grammar and template semantics stay unchanged. |
| Proposed step `wait_until` | CEL readiness predicate; false waits, unknown waits with the missing fact shown. Re-evaluate until true or expired. Absent means immediately ready once job structure reaches the step. |
| Proposed step `expire_when` | CEL terminal predicate; true ends this unfinished step as `timeout`. It does not cancel its bound task. |
| Proposed job `spec.expire_when` | Finite outer CEL deadline for jobs containing auto-task steps; prevents an unbounded wait even if a step never becomes ready. |

Initially the new step wait fields are admitted only on auto-task targets. Job
expiry is admitted only when such a target exists. A `when` guard remains an
explicit skip, including on an auto-task step; readiness prerequisites should
use `wait_until` instead. A skipped step does not produce accepted prerequisite
evidence. No new expression is fed to the shipped equality-only `when` parser.

Use the type-checked subset from the
[condition-trigger design](../../condition-triggers/1_scope.md), with its size,
cost, literal-read and unknown-value rules. `now` is the owner's pass timestamp.
The additional wait environment is typed:

| Fact | Meaning |
|---|---|
| `job.started_at`, `job.source_revision` | Persisted admission time and optional pinned workspace source SHA for this job execution. Readiness jobs pin their configured integration branch at admission. |
| `self.wait_started_at`, `self.status`, optional `self.task_id` | This step's immutable activation time, observed state and task binding. |
| Optional `self.prev` | Previous sibling's status and optional `end_time`; absent for a first step. |
| `steps['id']` | Literal references to earlier completed/observed steps in this branch or after a parallel join; no forward, self or cross-live-branch references initially. |
| `board.empty` | No tasks in backlog, in-progress or review, as defined below. |
| `result` in `reuse_when` only | Candidate evidence's typed status, source revision, completion time, contract, definition digest and task/run IDs. |

Auto-task step status includes `waiting`, `queued`, `running`, `done`, `held`,
`failed`, `timeout`, `cancelled` and `skipped`. `done` is a normalized accepted
completion, distinct from raw task status and the job runner's `success` state.
`end_time` is when this step settles in the job, including reuse; the result's
`completed_at` retains the original evidence time. Optional facts require
`has()` unless short-circuit checking proves they exist. These are CEL reads,
not template rendering of the entire expression before evaluation.
The wait projection can report an earlier ordinary step's successful settlement
as `done`, but a skipped step remains `skipped`; a successful no-op is not
accepted task evidence. This observation does not rewrite existing run states.

The referenced auto-task's own workspace condition/guard remains an additional
admission guard; its `self` still names its standalone history. Its cron/interval
slots do not restrict an explicit job request. Job mints do not consume its
standalone schedule cursor. Missing or disabled definitions hold admission, and
task/host/file conditions without a bindable subject are refused explicitly.

## 3. Board conditions, time spacing and maximum wait

`board.empty` is a proposed read matching the release-prep unfinished-task gate:
no backlog, in-progress or review tasks. It does not mean only "no eligible
backlog". Proposed, blocked and someday tasks are excluded and shown as such in
explain; use explicit status counts if they should block. Scheduled tasks retain
their stored status and count according to that status. Open release-blocking
findings are also checked by the result contract below.

Exclude only the current step's bound task from its mint/admission board view.
Sibling children and outside work count normally; a child cannot block its own
start, while a security branch may intentionally wait for an active QA task.
The orchestration job, clock and long-lived drain are not task-board entries.
Recheck the step and inherited guards immediately before admitting a newly
minted child to execution, through the same owner admission fence. A true mint
snapshot is not permission to start later under false conditions. Persist a
step-bound admission hold rather than changing task status to simulate waiting.
Binding unrelated existing work never adds new execution restrictions to it.

Examples use CEL directly:

```yaml
# Simple predecessor-done and empty-board condition.
wait_until: "self.prev.status == 'done' && board.empty"

# Terminal upper bound: stop waiting after eight hours.
expire_when: "now >= self.wait_started_at + duration('8h')"
```

The spaced expression is
`self.prev.status == 'done' && now >= self.prev.end_time + duration('30m') && board.empty`.
Adding 30 minutes to `now` and comparing it to an already-past end time does not
impose that delay. A maximum window inside `wait_until` alone is also insufficient:
false still means waiting after the window closes. `expire_when` supplies the
separate terminal meaning, with no `delay_minutes` or `max_wait_minutes` fields.

Each step's wait clock starts when job structure activates it, including the
readiness wait, task queue and wait for completion. A parallel branch activates
with its container, not after an earlier branch. Persist that time once. Waiting
through downtime, pauses or retries consumes wall time; it never resets it.

Every job containing auto-task steps requires a finite `spec.expire_when`:
initially `now >= timestamp('<RFC3339>')` or
`now >= job.started_at + duration('<positive finite duration>')`, at most seven
days from admission. These restricted ordinary CEL shapes make the outer bound
recoverable without proving that arbitrary code eventually becomes true. Step
expiry may use general admitted CEL within that bound. More complex timing is
an expression, not an additional timer language.

Reconcile trusted terminal receipts, then expiry, then new admission. A receipt
first observed after downtime may satisfy a known deadline only if its trusted
settlement time preceded that deadline; later completion cannot evade expiry.
For a dynamic step expiry with no recoverable time boundary, do not backdate
acceptance. Expiry wins against a simultaneous new admission.

A timed-out job/step admits no further children and retains its task bindings,
partial results and diagnostic. Already-running children continue under their
own pipeline timeouts. Late results are retained but never silently reopen the
parent. Cancellation or recovery of a child needs its own normal authorization.

## 4. Binding, dedupe, reuse and accepted results

Once ready, the step first reuses an accepted result satisfying `reuse_when`,
if supplied; otherwise it binds a compatible open instance or performs one
keyed mint. Match workspace, definition/template digest, inputs, required result
contract and source revision. An incompatible open instance yields an explained
hold, not another task. Referenced guards also apply to reuse. Without
`reuse_when`, a historical completion cannot discharge the step.

Use the existing auto-task definition-instance admission fence and dedupe policy
across normal schedule slots and job requests. Concurrent `skip_if_open` requests
bind one compatible task, with independent durable obligations per requesting
job. Reserve `(run lineage, step id, dynamic invocation identity)` and its input
digest before minting, and resolve that key after an ambiguous reply. Retry or
resume must not remint; loop/fan-out invocation identity cannot collapse distinct
items. First-phase enablement excludes auto-task targets inside loops/fan-out;
sequential and parallel lists are bounded to 32 auto-task steps per job.

Generic steps require the referenced definition's declared completion contract
(normal structured task completion when no stronger contract is declared). An optional
`require_result: release-readiness/v1` requires a system-validated receipt with
task/run, definition digest, source SHA, time, evidence references and readiness
`passed`, `blocked` or `unavailable`. A done audit that files unresolved
release-blocking findings is held, not accepted as a done prerequisite. Missing
or unreadable evidence is unavailable. An audit may finish without code changes;
neither a PR nor a Git merge is necessary for accepted no-diff completion.

Core validates registered typed contracts and provenance; the workspace or
owning plugin defines release-blocker policy and QA/security evidence adapters.
There is no release-specific severity rule in generic job execution. Agent
final messages are advisory and cannot certify a prerequisite. A target's typed
step output is generated from accepted durable evidence, not an agent envelope.

The readiness trigger's `auto_tasks['release-prep'].last_succeeded_at` must use
that definition's declared accepted-result history, not task `done`, mint time
or PR merge time. The existing release-prep probe may finish successfully while
reporting a busy board or nothing to release. Those outcomes must not advance
the readiness-completion timestamp or satisfy the preparation step. Its adapter
accepts the actual eligible preparation handoff, including the canonical task
reference; the same typed contract is required below. Missing history and an
unavailable history read remain distinct, as in condition triggers.

Pin the integration revision once for release-readiness. Reused checks must
cover that exact revision; immediately before preparation, revalidate required
receipts, age limits and source freshness, even if earlier steps checkpointed
success. A branch advance or evidence ageing holds the run and identifies the
stale proof. Recovery cannot skip that revalidation or certify a newer candidate
with old evidence; a fresh bounded run or audited explicit recovery is needed.

## 5. Durability and worker lifecycle

Use the existing versioned job checkpoint for frozen definitions, source fire,
source SHA, activation times, task/run bindings, action keys, accepted receipts
and expiry. Successful top-level steps already checkpoint today; pending task
bindings and partial parallel branches require additional durable checkpoints
before their external effects. A checkpoint failure must prevent a new mint or
admission, not merely log a warning and proceed.

The current parallel executor uses scoped threads and can replay an incomplete
compound block on resume. A new auto-task branch must therefore restore its own
binding and accepted result inside that replay; it cannot rely on completed
top-level checkpoints alone. A succeeded sibling must not remint or rerun.

Introduce a durable parked-wait continuation in the existing job runtime. Waiting
does not hold a provider process, thread per pending child, task worktree/context
lock or drain leaf slot. The existing owner clock revisits due parked runs;
observing waits neither submits a fresh logical execution nor resets deadlines.
Worker loss and normal parking must be distinguishable so orphan reconciliation
does not report a parked job as interrupted. This is necessary runtime work,
not a property supplied by adding a target-name prefix alone.

Normal job identity, `max_active_runs`, audit and sequential/parallel failure
rules remain authoritative. A parked live execution still counts against its
job's active-run ceiling. Failed branches do not acquire new implicit exception
policy: existing parallel join/failure behavior decides the job outcome. Failure,
human archive or cancellation of a child never automatically recreates it. Run
recovery preserves successful evidence and human interventions, and does not
reset expired waits without an audited extension or new execution.

Definition edits affect future executions; active runs retain frozen job and
target definitions while enforcing current authority and enablement. Owner
coordination may observe ordinary follower-executed children via owner receipts.
No cross-workspace task binding or follower-owned coordination is introduced.

## 6. Release-readiness example

All additions are illustrative proposed syntax; these examples are disabled.
`full-qa-sweep` must resolve to the actual intended definition before enabling.
The job is an ordinary catalog job with one condition-triggered routine:

```yaml
# .orbit/routines/release-readiness.yaml
schemaVersion: 1
name: release-readiness
enabled: false
trigger:
  condition:
    scope: workspace
    when: >-
      now >= timestamp('2026-10-31T00:00:00-07:00') && board.empty
      && (!has(auto_tasks['release-prep'].last_succeeded_at)
          || now - auto_tasks['release-prep'].last_succeeded_at >= duration('24h'))
    for_minutes: 30
target: job:release-readiness
policy:
  overlap: forbid
  timeout_minutes: 1440
  retries: { max: 0 }
```

```yaml
# .orbit/jobs/release-readiness.yaml
schemaVersion: 3
kind: Job
metadata:
  name: release-readiness
spec:
  state: disabled
  kind: workflow
  max_active_runs: 1
  expire_when: "now >= job.started_at + duration('24h')"
  steps:
    - id: qa
      target: auto-task:full-qa-sweep
      wait_until: "board.empty"
      require_result: release-readiness/v1
      reuse_when: >-
        result.status == 'passed' && result.revision == job.source_revision
        && now - result.completed_at <= duration('24h')
    - id: security
      target: auto-task:security-review
      wait_until: "self.prev.status == 'done' && board.empty"
      expire_when: "now >= self.wait_started_at + duration('8h')"
      require_result: release-readiness/v1
      reuse_when: >-
        result.status == 'passed' && result.revision == job.source_revision
        && now - result.completed_at <= duration('24h')
    - id: prepare
      target: auto-task:release-prep
      require_result: release-readiness/v1
      wait_until: >-
        steps['qa'].status == 'done' && steps['security'].status == 'done'
        && board.empty
```

"After October 30" here means October 31 at midnight in Los Angeles, with the
explicit offset on that date; use October 30 for "on or after October 30". The
30-minute initial quiet hold is independent of optional spacing between steps.
For spacing, replace security's `wait_until` with the expression in §3. An
eight-hour security wait starts when the preceding sequential QA step settles,
within the 24-hour outer job bound.

For independently eligible QA and security, replace the sequential `steps` list
with existing parallel structure, retaining the outer expiry:

```yaml
steps:
  - id: checks
    parallel:
      join: { mode: all }
      branches:
        - id: qa
          target: auto-task:full-qa-sweep
          wait_until: "board.empty"
          require_result: release-readiness/v1
        - id: security
          target: auto-task:security-review
          wait_until: "now >= job.started_at + duration('30m') && board.empty"
          require_result: release-readiness/v1
  - id: prepare
    target: auto-task:release-prep
    require_result: release-readiness/v1
    wait_until: "board.empty"
```

Security becomes eligible 30 minutes after job admission irrespective of QA's
completion. Its board-empty guard may still wait for QA's task to finish.
Preparation follows the successful `all` join. Waiting is not skipped-success,
so a branch cannot satisfy `all` merely by failing to become ready. Existing
`when` skips retain their no-op meaning; never use them to waive prerequisites.

The routine's explicit overlap policy and the parked job's active identity
prevent interim empty-board edges from queuing another readiness execution.
Its 24-hour timeout horizon matches the job bound; a shorter routine staleness
horizon must not free the overlap fence while that parked job is still live.
The finite wait must be enforced by durable job expiry, not merely by dropping
the routine's overlap record. No automatic whole-job retries are configured.

Preparation keeps its unfinished-work, no-release and canonical-task checks.
This job does not tag, bump versions, publish, promote branches or acquire
release authority. Enabling a routine/job does not enable its target definitions
or grant approval, dispatch or paid-provider authority.

## 7. Surfaces, compatibility and delivery

Use existing job catalog, `orbit run show`, audit, control and resume surfaces.
Extend the condition design's explain projection to show each `wait_until`,
expiry, previous-step binding, typed result, reused task, pinned revision and
admission guard. Distinguish waiting from skipped, and raw task done from accepted
step done. No `orbit auto-sequence` or new Automation definition category is
needed. The existing job view can describe these as auto-task steps.

Unknown new fields/targets must fail load on older binaries before any action.
Reject affected assets before activation on unsupported clients; never silently
drop `wait_until` or skip an unresolved target. Version pending/partial branch
checkpoints and refuse older writers that cannot preserve them. Existing jobs
and long-lived clients keep shipped behavior unless they opt into the extension.

Deliver in phases: (1) disabled schema, target resolution and CEL explain
(shipped, see "Phase 1 as built" below);
(2) keyed bind/mint, parked waits, admission and partial-branch checkpoints
(built, see "Phase 2 as built");
(3) QA, security and preparation readiness adapters plus disabled job/routine examples.
None silently expands the already-filed condition-language phase-1 task. Actual
enablement, provider work and release timing remain operator decisions.

### Phase 1 as built

- **Version.** A job using any of the new keys or an `auto-task:` target, at
  any nesting, declares `schemaVersion: 3`. Under `schemaVersion: 2` the
  loader refuses any of the keys by presence, a null value or a nested step
  included. `JobV2.schema_version` carries the declared version from the
  header, omitted when 2, and is never inferred from the remaining fields, so
  a 3 survives normalization and a serialized round trip
  ([Auto-task jobs declare schemaVersion 3](../4_decisions.md#auto-task-jobs-declare-schemaversion-3)).
  A `schemaVersion: 3` job keeps the version even with no extension field
  left. Activities stay `schemaVersion: 2`. Phase 1 also kept every such job
  disabled and refused to submit it; phase 2 lifts that.
- **Shape.** `orbit-types` parses the new keys. An auto-task step admits only
  `id`, `stage`, `when`, `target` and the four wait keys, so a key this binary
  does not know is refused, not dropped. `timeout_seconds: 0`, a serialized
  ref's default, is the one exception. The wait keys are refused on any other
  step. The engine keeps an `auto-task:` ref through target resolution;
  `validate_job` refuses one inside `loop` or `fan_out` before the first step.
- **Checks.** `orbit-automation`'s condition checker gains a typed job scope
  (`condition/job_wait.rs`), and Core runs it at catalog load, so a refused
  job leaves the catalog with a per-file diagnostic, like a parse error. It
  refuses a missing, non-restricted, non-positive or
  over-168h `spec.expire_when`, more than 32 auto-task steps, a target inside
  `loop` or `fan_out`, and an unregistered `require_result`. Inside the
  expressions it also refuses type errors, unknown or computed `steps[]`
  references, and forward, self, enclosing, sibling-branch and loop-nested
  ones.
- **Typing refinements.** A step with a previous sibling always has
  `self.prev`, so it reads it without `has()`; on a first step or a parallel
  branch the read is refused. `job.source_revision` is required-typed when a
  step requires `release-readiness/v1`, which pins the revision at admission;
  otherwise it needs `has()`. So does `result.revision`, except under that
  contract. `board` adds `empty` to the condition board fields, and
  `tasks['<ID>']` stays readable.
- **Explain.** `orbit automation explain job:<name>` (MCP
  `orbit.automation.explain`) reports the outer bound, the revision pin, and
  each step's target, resolved now with its file digest, enabled state and
  own condition guard, or reported `missing`. It also shows each step's
  guards, waits and step reads. It writes nothing, and nothing is evaluated.
  `orbit job show` lists the new fields. Both report `schema_version`.
- **Older binaries.** Every older loader admits only `schemaVersion: 2`, so it
  refuses a `schemaVersion: 3` file before any effect. Its catalog skips the
  file with a per-file diagnostic, and `orbit job run <path>` fails with
  `unsupported schemaVersion: 3`. Orbit 0.39.0 does this for disabled,
  enabled and nested specimens, recording no run and no task. A workspace
  this binary opens is also at layout version 5 (`auto-task-job-schema`, a
  breaking migration). Older binaries refuse to open it, and the generation
  guard keeps a newer binary from bootstrapping while an older live client
  holds the workspace. That covers binaries built between [ORB-20157] and
  [ORB-20237], which would accept the `schemaVersion: 2` form.

### Phase 2 as built

- **Freeze.** Submission checks the waits again and, for a job with an
  auto-task step, pins its YAML as the run's definition snapshot (a catalog
  job included) and records a versioned checkpoint in the run state:
  admission time, outer deadline, and the integration branch's commit when a
  step requires `release-readiness/v1` (refused if it cannot be pinned).
  A resume keeps its source's checkpoint, so deadlines include queue time and
  downtime. Only a detached submission runs such a job; the foreground
  `orbit job run <path>` refuses it.
- **Step pass.** The engine (`job_executor/auto_task.rs`) asks the host to
  observe the step, then checkpoints before each effect: activation, a
  reservation, then admission. Observation reads the bound task's terminal
  receipt with the owner's recorded settlement time, then expiry, then
  readiness, in that order. Completed (`done`) is accepted. Archived,
  rejected or missing tasks are not, and nothing remints them. A receipt
  settled after the deadline is not accepted, and the step times out. A
  checkpoint error stops the step before any effect.
- **Admission.** Core (`auto_tasks/job_step.rs`) admits under the auto-task
  cursor sidecar lock, the fence scheduled and manual mints already hold.
  A step's action key is `<run>:<step>`. Its mint records a
  `job_step_invocation` creation receipt on the task, so a lost reply
  resolves to that task instead of a second mint. Under the fence it
  rechecks expiry, the definition's current enablement and its own
  workspace condition or schedule guard, and `wait_until`. With
  `skip_if_open` dedupe it binds an open instance, or holds when that
  instance was minted for other inputs or the job pins a revision. Mints
  consume no schedule cursor. The board view excludes only the step's own
  bound task, so two parallel `board.empty` branches serialize: one binds and
  the other parks until it finishes. A task, host or file condition has no
  bindable subject in a job step and holds, as does `require_result`, which
  has no evidence adapter yet. `reuse_when` never matches yet.
- **Parking.** A waiting, held or bound step parks the run. The run stays
  `running`, so it counts against `max_active_runs`. Its checkpoint records
  the park and its worker exits, holding no provider, thread, worktree or
  drain slot. Orphan reconciliation and the worker supervisor skip a parked
  run. The sweep wakes it when due (at most 60 s later, or at the deadline).
  The woken worker adopts it by a compare-and-set on its park, so it resumes
  once. A job disabled or removed while parked cancels the run.
- **Parallel.** A succeeded branch is checkpointed and restored on replay
  without rerunning it. A target branch restores its output. A succeeded
  loop, fan-out, or nested parallel restores that output and the pipeline
  entries it exposed (nested step outputs and fan-in collect and error
  aliases). A branch that has not succeeded runs again. A block whose join
  can still pass parks while any branch is parked; its join semantics are
  unchanged.
- **Start guard.** A minted task starts only while its step admits it. Local
  submission and owner pull-claim admission recheck the run (not finished),
  the binding, the step (not settled or expired), the definition and
  `wait_until`. Bound existing work is never restricted.
- **Older writers.** The checkpoint carries a version, and a binary refuses
  to rewrite a newer one. A workspace this binary opens is at layout version
  7 (`auto-task-job-checkpoints`, a breaking migration), so a binary that
  would drop the checkpoint or reap a parked run cannot open it.

## 8. Acceptance and honest limitations

- Prove existing `when: false` still skips and never waits. A new false
  `wait_until` parks without a provider, worker slot or thread leak, then resumes
  on true; unknown operands remain visible until resolved or expired.
- Sequential order and parallel joins remain unchanged. Branch-local conditions
  allow independent eligibility, while `self.prev` and earlier-step conditions
  explicitly wait for accepted completion. Unsupported references fail load.
- Concurrency, lost replies, retry, restart and incomplete parallel replay bind
  the original tasks and preserve completed siblings. Checkpoint loss cannot
  admit an unrecorded effect. No normal schedule cursor is consumed by a job.
- Step/job expiry includes queues and downtime, yields an explicit timeout and
  preserves child workers and partial evidence. Human archives remain archived.
- A done audit with blocking findings or unavailable evidence cannot unlock
  preparation. Exact-source and age checks are repeated at preparation admission;
  accepted no-diff evidence needs no Git merge.
- Explain and isolated public CLI/runtime fixtures establish the contracts before
  any live configuration is enabled. Do not claim proposed CEL/runtime syntax
  already executes; record skipped or unavailable proof explicitly.

Typed target resolution, persistent parking and per-branch checkpoints are real
work even though this is a smaller authoring model. Board emptiness remains a
snapshot; admission is rechecked and stale source evidence holds the job. The
first version excludes dynamic loop/fan-out targets, cross-workspace references
and arbitrary artifact predicates. QA/security readiness adapters do not exist
merely because their auto-task templates do; no release-readiness run is ready
to enable until those contracts are implemented and validated.

## Task References

- ORB-19973 — records Daniel's decision to reuse jobs with an auto-task step.
- ORB-19990 — covers condition-language schema/explain only; this proposal does
  not change that task or authorize job extension implementation.
- [ORB-20157] — phase 1: define, check and explain disabled auto-task steps.
- [ORB-20237] — require `schemaVersion: 3` for auto-task jobs and fence older
  clients with layout migration 5.
- [ORB-20158] — phase 2: keyed bind/mint, parked waits, admission rechecks and
  partial-branch checkpoints; layout migration 7.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
