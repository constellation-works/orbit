---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18124, ORB-18270, ORB-18271]
---

# Final recovery has no checkout to work in

## Signature

`recovery_commit_refused:uncommittable: final recovery has no observed checkout, candidate or branch`, or `worktree integrity violation worktree_mismatch: workspace_path '.../orbit-jrun-...' is not a directory`.

## Mechanism

The run's worktree was removed or never created before recovery started, or a rebase stopped mid-way and left nothing recovery could observe. Triage called the ORB-18124 case an Orbit defect already fixed in source but not deployed (existing record ORB-18189).

## Resolution

Inspect the retained worktree and HEAD by hand before resuming; for a missing directory requeue for a fresh attempt, which Daniel did for ORB-18270 (blocked to proposed).

## Fix task

No open fix task found. ORB-18189 (done) fixed the conflict-recovery refusal after a before-landing push; deployment state was not verified.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 09:34 | ORB-18124 | jrun-20261025-0856-c3 | uncommittable, no observed checkout | merged outside Orbit at 2026-10-25T11:06Z, settled done |
| 2026-10-25 18:36 | ORB-18270 | jrun-20261025-1832-c3 | worktree_mismatch, directory missing | human forced blocked->proposed at 2026-10-25T19:02Z |
| 2026-10-25 18:36 | ORB-18271 | jrun-20261025-1836-c3 | worktree_mismatch, directory missing | human:daniel moved to backlog at 2026-10-25T19:02Z |
