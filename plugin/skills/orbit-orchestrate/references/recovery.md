# Routing findings into repairs

## CI and QA: verify freshness before filing

Use `ci_failure_sweep_pipeline` for CI discovery. Its current admission path
files proposed repairs, runs pilot, and can promote its own warning-free,
selector-backed tasks; it does not implement them. Inspect the resulting task
and admission evidence rather than duplicating that work. See
[workflows.md](workflows.md).

Before filing or dispatching a repair, compare the failing run/job and SHA
with the current landing branch and prior fix PRs. An old CI failure can arrive
after the repair merged; the sweep holds such failures in
`pending_supersession` rather than filing them, so read that list in the
sweep's step output before filing by hand. A red run is held there for a newer
push run still in flight at a descendant commit only while it is a lone
failure: once the previous completed run failed the same job with the same
normalized error signature, the sweep files it (`reproduced_on`), and a hold older than
`pending_supersession_window_minutes` (default 30) is filed with the pending
run named in the task (`held_past_window`). A failure that sits in
`pending_supersession` across sweeps on a busy branch is therefore not
waiting on you; one that never gets filed is a sweep defect. Attach the exact failing command/log excerpt and
current reproducibility evidence to one bounded task. Search open and closed
history; reject proven duplicates with a link to the delivered fix. Cancel a
duplicate's active child only within authorization and after inspecting its
state; do not cancel the whole drain.

When a hand fix is open, archive the sweep finding with a `covered_by`
relation in the same task update. Preserve all existing relations and append
`{"type":"covered_by","target":"<covering task id>"}` or
`{"type":"covered_by","target":"github-pr:https://github.com/OWNER/REPO/pull/NUMBER"}`.
`github-pr:NUMBER` uses the checkout's GitHub repository. This is durable
operator coverage of the exact failure key: later sweeps report `covered`
with the archived owner and cover, and create no pilot candidate. A compiler
error's key includes the checkout, so the hold also follows the same complete
diagnostic set (paths and messages, ignoring line and column) to later
checkouts; an owner filed before that set was recorded in its description
holds only at its own checkout. A rejected finding can carry the same relation. A missing or unreadable cover remains
withheld with `operator_cover_unavailable`; inspect that reason rather than
assuming the PR is open. A closed, unmerged PR (or archived/rejected cover task)
releases the key. After a merged cover's commit is in the failing checkout,
the sweep files a new repair and names that cover as a fix that did not hold;
older checkouts stay covered while waiting for the fix.

A plain archive or rejection without `covered_by` suppresses the exact key
(and, for compiler errors, the same diagnostic set at later checkouts) for
`ci_failure.operator_suppression_hours` (default 6) from the operator's
status decision. Its report entry is `withheld`, reason `operator_archived`,
with the owner and expiry. After expiry a current failure can file again.
Matching normalized failing-test signatures across jobs share one task with
all job names and source identities; different signatures remain separate.
Read `file.withheld` and its matching audit entries alongside
`skipped_existing` and `pending_supersession` when assessing CI relief.

Post-merge code review and QA follow the same loop. Exercise real user paths
and report concrete defects; do not replace verification with an agent's
claim that the change is correct. Pilot and promptly promote authorized
repairs while independent work continues.

Run an authorized sweep on the owning workspace with:

```bash
orbit run job ci_failure_sweep_pipeline
```

Check whether its routine or a manual invocation is already active first.

## Diagnose failed runs before retrying

```bash
orbit task show <task-id>
orbit run show <run-id> --json
orbit run logs <run-id> --step <step-id> --json
```

A failed run can leave its task `blocked` with the failure attached. The owner's
clock can dispatch final recovery for an eligible block; inspect its decision
and current task state first. See [automation.md](../../orbit-setup/references/automation.md#built-in-final-recovery-of-blocked-tasks).
If it remains blocked, read the evidence yourself, then
make the transition deliberately — return the task to `backlog` only once you
know why it failed and that a rerun can succeed. A sandbox denial or provider
failure is not inherently transient; repeated identical failures need a repair
or configuration correction before another attempt.

A live process is not stopped merely because a tool observation timed out.
Re-poll the same run and inspect current process liveness. Conversely, a stale
lock file alone does not prove a worker is alive. Follow
[run-debugging.md](run-debugging.md) before process-level
intervention, and never weaken protected-path policies to make a retry pass.

## Close a rescued blocked task

When a blocked task's work landed outside its run (its PR was merged by hand),
close the task rather than rerunning it. First confirm that no run is live on
it and that the merged commit meets its acceptance criteria. Then, as an
operator in the owning workspace's checkout, run:

```bash
export ORBIT_OPERATOR=1
orbit tool run orbit.task.update --input '{"id":"<task-id>","status":"in-progress","execution_summary":"<what landed: PR, commit, evidence>"}'
orbit tool run orbit.task.update --input '{"id":"<task-id>","status":"review"}'
orbit tool run orbit.task.update --input '{"id":"<task-id>","status":"done"}'
```

The `execution_summary` on the `blocked → in-progress` write makes it a close
rather than a start. No work starts, so another run's execution claim on the
same files does not refuse the write, and you don't need `--force`. Add `plan`
to that write if the task has none. Operator capability comes from an
interactive terminal or `ORBIT_OPERATOR=1`; an `ssh host '<command>'`
invocation has no terminal, so it needs the variable. Don't pass `model`,
because naming an agent makes the write an agent's. Without the summary, from
an agent, or without operator capability, the write starts work. While a
claim overlaps, that start is refused with the claim's task and run named.

The CLI subcommand also closes without `--force`: run
`orbit task update <task-id> --status in-progress --execution-summary "<…>"`,
then `--status review` and `--status done`. From an agent shell (`ORBIT_AGENT_*`
or a managed run), the CLI's move into `in-progress` starts work like the tool
does and is refused with the claim's task and run named while another run's
claim overlaps. Keep `--force` for an edge the lifecycle refuses. It records an
override in task history.

## A PR exists but completion failed

Inspect the failed step and GitHub state independently. A `complete_pr` error
can be a repository merge-method or auto-merge setting mismatch even when
there are no rebase conflicts. A failure-handoff PR is preserved work, not
proof that it is safe to merge or that the task is done.

Check the candidate head/base, mergeability, checks, and repository settings.
Use an allowed merge method through the supported recovery path. Change a
repository setting only when the user's authorization covers that change;
opening a PR does not grant that permission. Verify actual merge and reconcile
task state with the evidence. Do not rerun implementation solely because the
completion step failed.

## Repair tasks and activity boundaries

Prefer a bounded task for actionable tooling, documentation, or operational
friction. A separate friction artifact is optional, not a prerequisite; honor
a user's preference to file tasks only. Avoid duplicate records that describe
the same fix without adding useful evidence.

Agent output is advisory, not an authoritative activity success contract. Do
not add generic output-schema enforcement or retries that force a model to
produce a particular report shape. Deterministic operations validate the
inputs they consume and the state they change at their own boundary. Verify
actual files, persisted selectors, tests, and delivery outcomes. Improve a
misleading prompt or diagnostic narrowly when evidence supports it.

## Verify deployment separately from merge

When installation or service recovery is in scope, a merged source fix is
only an intermediate result. On the owning host:

1. Verify the executable actually invoked, service command, config, source
   revision, and checkout state. Preserve operator configuration overrides.
2. Build from the intended revision in an appropriate checkout, install to the
   actual executable location, and synchronize managed assets through the
   supported mechanism. Do not assume matching version strings mean matching
   binaries, or overwrite operator changes to make a source checkout clean.
3. Restart only the intended services. Verify their process/executable identity,
   sustained health and the repaired behavior; an immediate successful health
   response can miss a server that exits shortly afterward.
4. Record installed revision/hash, service result, and any remaining mismatch.
   Restore temporarily changed routines/settings unless the user made the
   change permanent.

See [maintenance.md](../../orbit-setup/references/maintenance.md) for supported
sync/upgrade mechanics. If capability or authority is missing, report it;
never create a shadow store or edit runtime state directly to bypass the gap.
