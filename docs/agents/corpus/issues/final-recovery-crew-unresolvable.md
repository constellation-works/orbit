---
type: issue
applies_to: [final_recovery]
status: resolved
links: [ORB-13958, ORB-13960, ORB-13970, ORB-13964]
---

# Final recovery crew pool cannot be resolved

## Signature

`job validation failed: activity crew drawn from workflow.final_recovery_crews cannot be resolved or used: invalid ...`; the blocked-task recovery run fails before deciding.

## Mechanism

At the time of these occurrences, recovery looked up the crew draw using the failed follower run's ID, but that run state was not persisted on the owner. ORB-13964 fixed the lookup to use the active recovery run's persisted state.

## Resolution

For the original failures, inspect `orbit run show <run>` and move the task by hand; the blocked run's recovery could not be resumed. ORB-13964 fixes the owner-side draw lookup so new recovery runs can use their own persisted draw.

## Fix task

ORB-13964 was forced done by Daniel on 2026-10-04T20:15Z; its fix later merged in PR #3141 as commit `a0e689e69`.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-04 16:53 | ORB-13960 | jrun-20261004-1653-t2 | final_recovery_crews cannot be resolved | human:daniel moved to backlog at 2026-10-04T16:56Z |
| 2026-10-04 16:55 | ORB-13958 | jrun-20261004-1655-t1 | final_recovery_crews cannot be resolved | human:daniel moved to backlog at 2026-10-04T16:56Z |
| 2026-10-04 19:26 | ORB-13970 | jrun-20261004-1926-t2 | final_recovery_crews cannot be resolved | human:daniel moved to archived at 2026-10-04T19:27Z |
