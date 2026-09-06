# Common orchestration decisions

## Missing context

A newly created task has empty `context_files`. Run zero-input
`orbit run job task_pilot_pipeline`; normal discovery does not need its ID.
Inspect applied task IDs and then the task's persisted selectors. If it was
excluded because another pilot owns it, observe that run. For a deliberate
re-audit of existing context, use explicit `task_ids`. Do not invent selectors
or erase valid ones to manipulate discovery.

## Duplicate or already-landed warning

Read the cited task and commit through the authoritative tools. If the work
is delivered, reject the duplicate with evidence. If the proposal covers a
real remaining gap, narrow its description and explain the distinction before
promotion. A warning is an assessment to verify, not an agent output to enforce.

## Dependency or file lock

`orbit run readiness --json` names an unmet prerequisite or conflicting task.
A valid prepared task can wait in backlog on its dependency. For a live lock
holder, observe its run and keep independent work moving. Do not remove
selectors or dependencies to bypass admission. Release a stale reservation
only after confirming the owning run is inactive, through the supported lock
operation described in [common-failures.md](../../orbit/references/common-failures.md).

## Missing operator capability

Check the connected server's advertised tools and schemas. A missing option
may have an authorized administrative equivalent on the same host; a denied
capability is not permission to bypass that boundary. Report the specific gap
if no supported route exists. A managed leaf worker cannot dispatch follow-up
runs. See [tool-surface.md](../../orbit/references/tool-surface.md).

## CI reports a failure after the fix merged

Compare the failing SHA with the fix's merged commit. If the report describes
the same resolved defect, link and reject the duplicate rather than preparing
another identical repair. If current evidence still fails, file a bounded
follow-up with the new evidence. The CI sweep may already have piloted and
promoted a repair; inspect it before submitting anything manually.

## Provider limits change during a window

The user excludes a provider from future work but leaves current workers alone.
Inspect effective crews, including the pilot's explicit `system` crew. Update
queued task crews only within the requested scope. `--allow-crew` permits its
listed crews; it does not remap excluded tasks or stop workers already running.

Do not launch a fresh full-duration drain beside the old one. Inspect the
installed CLI for supported live controls. If replacement is required and
authorized, establish the original deadline, confirm how to stop new admissions
without cancelling existing children, and verify the old coordinator stopped
before starting its replacement. Use only the remaining duration and preserve
completion authority (`--complete` when originally authorized), scope, and
capacity. If that cannot be done safely, leave the run unchanged and report
the limitation. No fresh window is implied.

## Window expires or the user stops

Verify the coordinator's terminal state and the remaining child runs.
Already-admitted workers can finish under their original authority; do not
cancel them solely because the window expired. Do not start additional work
unless existing authorization covers it. If the user ends supervision, stop
and hand off the live runs, pending repairs/deployment, and unchanged routines.
A stop is not a claim that all work finished.
