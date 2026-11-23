# Author a version 3 auto-task workflow

## Contents

- [Version 3 auto-task workflows](#version-3-auto-task-workflows)
  - [Bounded waits and durable parking](#bounded-waits-and-durable-parking)
  - [Bindings, completion contracts and optional reuse](#bindings-completion-contracts-and-optional-reuse)
  - [Author and verify a bounded job](#author-and-verify-a-bounded-job)

## Version 3 auto-task workflows

Use `schemaVersion: 3` when a job includes `target: auto-task:<name>`. It binds
or mints a task from the named workspace definition, then waits for accepted
completion; minting alone is not success. Children follow normal approval,
readiness, drain, host and delivery policy. Naming a target grants no promotion,
dispatch, merge or release authority. Version 2 rejects auto-task targets and
their wait fields, even when the fields are null.

Keep ordinary sequential lists for ordered prerequisites. Use an explicit
`parallel` block with `join: { mode: all }` for independent required branches.
At most 32 auto-task steps are admitted per job; they cannot be inside loops
or fan-out bodies. `self.prev` is the previous sibling, absent on a first step
or independent branch. `steps['id']` can read earlier settled steps visible in
this branch or after a parallel join, never a forward or live sibling branch.

Guard optional facts with `has()` where their presence is not established.

### Bounded waits and durable parking

| Field | Meaning |
|---|---|
| Existing `when` | One-shot guard: false skips successfully. A skipped step supplies no accepted prerequisite evidence. |
| Step `wait_until` | CEL readiness predicate: false or unknown parks, with missing facts reported. Absent means ready when the job reaches the step. |
| Step `expire_when` | CEL terminal predicate: true times out the unfinished step without cancelling its child. |
| Job `spec.expire_when` | Required finite outer bound for any job containing auto-task steps. |

The outer bound must be `now >= job.started_at + duration('<positive duration>')`
or `now >= timestamp('<RFC3339>')`, no more than seven days after admission.
A readiness window inside `wait_until` cannot replace expiry: false continues
waiting. Step expiry can impose a shorter bound, such as
`now >= self.wait_started_at + duration('8h')`.

`board.empty` means no backlog, in-progress or review tasks. Proposed, blocked
and someday tasks are excluded. Only this step's bound child is excluded from
its board view; sibling children and outside work still count. The target's
own condition/guard also applies and is rechecked at admission. Its cron or
interval slots do not restrict an explicit job request or get consumed by it.

An enabled version 3 job runs detached and parks durably without retaining a
provider process, task worktree/context lock or drain leaf slot. The owner clock
revisits it; a parked run still counts against `max_active_runs`. Checkpoints
retain frozen definitions, bindings, accepted receipts and activation times,
including partial parallel progress. Readiness, queue time, pauses and downtime
all consume the original deadline.

Resume/retry restores keyed bindings rather
than minting another child or rerunning a successful sibling. Expiry admits no
new children; already running children continue under their own policy, and
late results do not reopen the parent. Do not edit run state or reset clocks
to get past a hold.

### Bindings, completion contracts and optional reuse

Once ready, a step may reuse accepted history only if it declares `reuse_when`.
Otherwise it binds a compatible open instance or performs one keyed mint.
Compatibility includes workspace, definition/template digest, inputs, result
contract and source revision. An incompatible open instance holds instead of
creating a duplicate. Missing or disabled definitions also hold admission.

Without a typed contract on either side, normal structured task completion
applies. For typed completion, the step's `require_result` and the definition's
`completion.contract` must both be present and match; omission or mismatch
holds before minting. `require_result` must name a registered contract;
currently only `release-readiness/v1` is registered.

The definition's
`completion` block also names an evidence artifact, adapter and maximum age; see
[completion-contract setup](../../orbit-setup/references/auto-tasks.md#proving-a-job-step-with-a-completion-contract).
The adapter is a script requiring the owner's grant. Inspect its grant status;
do not grant it automatically as a repair.

Orbit validates durable evidence against task/run identity, definition digest,
source revision and completion time. The granted adapter answers `passed`,
`blocked` or `unavailable`; only `passed` is accepted. Missing evidence or a
missing/stale grant is unavailable. A task marked `done`, a successful agent
exit, a final message or a merged PR cannot replace this receipt. An accepted
audit may have no code diff or merge.

Unresolved release-blocking findings hold
the prerequisite; a busy-board or nothing-to-release preparation probe does not
count as accepted preparation.

Requiring `release-readiness/v1` pins the configured integration revision at
admission as `job.source_revision`. Before a later child is admitted, prior
receipts are revalidated for identity, exact source and age, even after a
successful checkpoint. A branch advance or ageing evidence holds the run;
use a fresh bounded run or authorized recovery rather than certifying the new
candidate with old evidence.

For a step requiring that contract, optional reuse can further restrict a
compatible accepted result:

```yaml
reuse_when: >-
  result.status == 'passed' && result.revision == job.source_revision
  && now < result.completed_at + duration('2h')
```

`result` exists only in `reuse_when` and exposes typed receipt fields. Reuse
does not bypass definition guards, source checks or the contract's maximum age.
The step's settlement time is new; `result.completed_at` stays the original
evidence time. Without `reuse_when`, historical completion cannot satisfy it.

Conditions read named accepted history as
`auto_tasks['<name>'].last_succeeded_at`. Only acceptance by that definition's
declared completion contract advances it, not mint time, raw task `done` or PR
merge time. Check `has(auto_tasks['<name>'].last_succeeded_at)` before reading a
possibly missing timestamp; distinguish missing history from an unreadable
history store when diagnosing it.

### Author and verify a bounded job

This project-agnostic draft assumes an existing `quality-audit` auto-task with
the matching completion contract and a granted adapter. Keep it disabled while
reviewing prerequisites and authorization:

```yaml
schemaVersion: 3
kind: Job
metadata:
  name: quality-readiness
spec:
  state: disabled
  kind: workflow
  max_active_runs: 1
  expire_when: "now >= job.started_at + duration('24h')"
  steps:
    - id: quality
      target: auto-task:quality-audit
      wait_until: "board.empty"
      expire_when: "now >= self.wait_started_at + duration('8h')"
      require_result: release-readiness/v1
```

The supplied `release-readiness` example is nonseeded and disabled. It composes
full QA, security review and preparation in sequence using typed results. It
deliberately omits `reuse_when` to obtain fresh checks on each execution. It is
an example for deliberate owner configuration, not an installed live release
workflow or authorization to release. Do not infer historical reuse from it.

Read back the effective installed catalog before enabling or submitting an
authorized job; workspace overrides can shadow shipped definitions:

```bash
orbit job list
orbit job show <job-name>
orbit auto-task show <auto-task-name>
orbit automation explain job:<job-name> --json
orbit run history -j <job-name>
orbit run show <run-id> --json
orbit run events <run-id> --json
```

In `explain`, check schema/state, the finite expiry, pinned revision, each
target's resolution/enabled state and definition digest, wait expressions,
completion contract, adapter grant and accepted history (`latest`, `missing`
or `unreadable`). This is definition inspection: step `not_evaluated` is not
a successful wait or execution.

For an authorized existing run, correlate its
persisted bindings, original activation/deadline, parked reason, accepted
receipts and task/run IDs with the child task artifacts and audit events.
Confirm the accepted receipt's source, age and evidence references, rather
than treating a child's raw `done` status as proof. Diagnose unknown facts,
disabled/missing targets, incompatible instances, unavailable adapters, stale
source/evidence and expiry from those records before retrying.

Preserve
successful bindings and human interventions; child cancellation needs its own
authorization. See [run debugging](run-debugging-recovery-evidence.md) for evidence inspection.
