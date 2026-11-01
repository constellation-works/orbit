---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-13958, ORB-13960, ORB-13970, ORB-13964]
---

# Final recovery crew pool cannot be resolved

## Signature

`job validation failed: activity crew drawn from workflow.final_recovery_crews cannot be resolved or used: invalid ...`; the blocked-task recovery run fails before deciding.

## Mechanism

The crew draw has no persisted run state on the owner for a task blocked by a follower's claimed run (ORB-13964 title).

## Resolution

Take the task by hand: `orbit run show <run>` then move it. Not retryable.

## Fix task

ORB-13964 was forced done by Daniel on 2026-10-04T20:15Z; whether its fix landed was not verified here.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-04 16:53 | ORB-13960 | jrun-20261004-1653-t2 | final_recovery_crews cannot be resolved | human:daniel moved to backlog at 2026-10-04T16:56Z |
| 2026-10-04 16:55 | ORB-13958 | jrun-20261004-1655-t1 | final_recovery_crews cannot be resolved | human:daniel moved to backlog at 2026-10-04T16:56Z |
| 2026-10-04 19:26 | ORB-13970 | jrun-20261004-1926-t2 | final_recovery_crews cannot be resolved | human:daniel moved to archived at 2026-10-04T19:27Z |
