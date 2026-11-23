# Inspect task-pilot preparation and apply

## Contents

- [Pinned source and apply outcomes](#pinned-source-and-apply-outcomes)
- [Routine preparation](#routine-preparation)
- [Partition outcomes](#partition-outcomes)

## Pinned source and apply outcomes

At the prepare activity boundary, an omitted optional `base_branch` is bound as
an empty string. Prepare treats an omitted or empty value as the registered
workspace base branch, else `workflow.base_branch`, fetches that landing branch, and pins one
`source_revision` while preserving primary HEAD, index, dirty and untracked
files. Manual preparation without a pinned source stops on remote failure.

State-triggered preparation best-effort fetches once during evaluation and
pins origin, keeping a local branch already ahead of origin; fetch failure
uses the local head captured before fetching. Its prepare step retains that
claim's pin. Each pilot runs in its own detached checkout at that revision, with
its cwd, input paths, and read-only filesystem profile bound there. Task tools
retain the owning logical workspace, and apply still checks task snapshots
with compare-and-set on that authority.

Inspection checkouts use at most 16
exclusive slots in the common Git directory; normal return, error, and timeout
remove the checkout before releasing its lease. A crash releases the kernel
lease, and the next holder reclaims the abandoned checkout before reuse. No
registered worktrees or primary branches are removed or modified.

Apply
validates selector existence against the same snapshot, not a later working tree, so a newly merged file is not reported as missing merely
because the primary lagged origin, and a later origin advance cannot admit a
path that did not exist at prepare.

## Routine preparation

The pilot agent inspection is read-only; its deterministic apply step mutates
validated task selectors. It runs five
partitions concurrently, and returns selector proposals plus duplicate,
already-landed, dependency, and conflicting-decision warnings. The shipped
`task_pilot` workspace routine seeds enabled by default, as does `worktree-gc`.

It uses a `preparation_eligible` state trigger, not a periodic cron: each tick
fingerprints eligible tasks and
queues only tasks without an applied assessment after the 2-minute debounce or
the 10-minute maximum wait. Once assessed, a task stays quiet across routine
windows until a certified description or acceptance-criteria edit makes its assessment stale; it then gets a fresh routine pilot unless a live execution claim or an unresolved failed pilot holds it.

A person's or agent session's `context_files` edit on a proposed or backlog task whose latest assessment holds a warning or an admission hold queues one the same way and releases an unresolved failed pilot; comments, plan and execution-summary updates and the pilot's own selector writes do not. Other material or policy edits never queue one; request another routine pilot explicitly.

An `--approve-proposed` drain still repilots a stale assessment automatically as described in [proposed approval](orchestration-proposed-approval.md). Inspect the workspace's actual `task_pilot.yaml`
before relying on that cadence, because a customized routine or preserved
legacy cron definition can differ from the shipped seed.

A local drain (`orbit run auto`, the dashboard start, MCP
`orbit.workflow` action `auto`) warns when no task pilot will prepare its tasks on this
machine: the workspace declares no `preparation_eligible` routine, or its
routine names another `owner_machine`, is disabled, or is paused here
(`orbit routine resume <name>`). The warning names the workspace, the machine
and the fix.

It is a `Warning:` line at start, `task_pilot_warning` on every
admission pass (`drain_last_pass` in `orbit run show --json`), and
`capacity.task_pilot_warning` in readiness while the window is open. The
dashboard shows it in the Fleet drains row, the Drain card and the drain's run
detail. Without the warning, tasks that need an assessment show only
`task_pilot_preparation_required`, and that wait never clears. A pull drain
does not warn, because its owner prepares the work.

The task-pilot pipeline never promotes tasks or dispatches them; promotion and
shipping remain separate operator-authorized steps.

## Partition outcomes

Apply is isolated by partition, then by task. A partition whose assessments are
malformed (duplicate, or not matching the prepared task IDs) fails as a whole
and mutates none of its tasks. Within a valid partition each task settles on
its own: a task that went stale at the write boundary is `stale` with a reason,
and a task with an invalid assessment is `invalid`. Independently valid siblings
and partitions still apply.

Durable edits that race a pilot settle as `superseded`, not as failures. This
covers a task whose fields, material, status or ownership changed after
preparation, a task admitted to or claimed for execution, a task that became
terminal, an operator rejection, and a task superseded by a source move under a
routine claim. Each skipped task carries a `reason` (for example `task_edited`,
`status_changed`, `execution_claim`, `workflow_admission`, or
`superseded_by_source`) and is not written.

Such a task needs no repair.

Each partition in the durable apply output has one outcome:

- `applied`: every task applied or was already applied.
- `superseded`: every task applied, was already applied, or was superseded, and
  at least one was superseded. The partition may still list applied siblings in
  `applied_task_ids`, so read `task_outcomes`, not the partition label alone.
- `skipped_stale`: no task applied and every task was stale.
- `partial`: some tasks applied and others did not resolve.
- `failed`: no task applied and at least one did not resolve, or the partition
  was malformed.

The run succeeds when every partition is `applied` or `superseded`. Any
`failed`, `partial`, or `skipped_stale` partition fails the run. The durable
apply output lists each partition's outcome, its task outcomes, and the exact
task IDs actually applied. `orbit run show <run_id>` is therefore the recovery
source of truth.

Resuming the failed run reuses its successful prepare, pilot,
and apply checkpoints (it does not rerun those agents); start a fresh zero-input
pilot only for tasks that remain empty after reviewing the recorded outcomes. A
superseded task was not written, so a fresh pilot assesses it against current
state if it still lacks selectors. Do not re-pilot the whole backlog for it.
