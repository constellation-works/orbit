# Routing findings into repairs

## CI and QA: verify freshness before filing

Use `ci_failure_sweep_pipeline` for CI discovery. Its current admission path
files proposed repairs, runs pilot, and can promote its own warning-free,
selector-backed tasks; it does not implement them. Inspect the resulting task
and admission evidence rather than duplicating that work. See
[workflows.md](../../orbit/references/workflows.md).

Before filing or dispatching a repair, compare the failing run/job and SHA
with the current landing branch and prior fix PRs. An old CI failure can arrive
after the repair merged. Attach the exact failing command/log excerpt and
current reproducibility evidence to one bounded task. Search open and closed
history; reject proven duplicates with a link to the delivered fix. Cancel a
duplicate's active child only within authorization and after inspecting its
state; do not cancel the whole drain.

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
orbit run triage
orbit run triage <task-id>
orbit run show <run-id> --json
orbit run logs <run-id> --step <step-id> --json
```

`task_triage_pipeline` diagnoses eligible blocked tasks. It can re-backlog
cases classified as environmental and leaves other failures blocked with a
diagnosis. Inspect the evidence and resulting task state. A sandbox denial or
provider failure is not inherently transient; repeated identical failures
need a repair or configuration correction before another attempt.

A live process is not stopped merely because a tool observation timed out.
Re-poll the same run and inspect current process liveness. Conversely, a stale
lock file alone does not prove a worker is alive. Follow
[run-debugging.md](../../orbit/references/run-debugging.md) before process-level
intervention, and never weaken protected-path policies to make a retry pass.

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

See [maintenance.md](../../orbit/references/setup/maintenance.md) for supported
sync/upgrade mechanics. If capability or authority is missing, report it;
never create a shadow store or edit runtime state directly to bypass the gap.
