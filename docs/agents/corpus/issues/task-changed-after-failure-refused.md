---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-16063, ORB-16616, ORB-17520, ORB-18052, ORB-15206]
---

# Final recovery refused because the task changed after the failure

## Signature

`final_recovery ... outcome=refused reason: task changed after the failure (was blocked at <t>, now in-progress|backlog at <t+30s>); the later decision stands` or `task is already archived; final recovery never reopens settled work`.

## Mechanism

An operator or another loop moved the task within seconds of the failure; the engine's stale-decision check discards the escalation.

## Resolution

No action. This is correct behaviour: a human or newer decision already moved the task. Do not requeue or reopen.

## Fix task

No fix task: the guard works as designed.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-11 02:21 | ORB-15206 | jrun-20261011-0220-t2 | stale decision refused: task changed after the failure | human:daniel moved to backlog at 2026-10-11T02:21Z |
| 2026-10-13 10:56 | ORB-15977 | jrun-20261013-1055-t1 | stale decision refused: task changed after the failure | no clearing event in history; task now archived |
| 2026-10-14 03:55 | ORB-16063 | jrun-20261014-0355-t1 | stale decision refused: task changed after the failure | system moved to done at 2026-10-14T05:06Z |
| 2026-10-18 05:45 | ORB-16616 | jrun-20261018-0545-t1 | stale decision refused: task changed after the failure | human forced in_progress->done at 2026-10-18T06:37Z |
| 2026-10-19 15:22 | ORB-17053 | jrun-20261019-1521-t1 | stale decision refused: task changed after the failure | system moved to done at 2026-10-19T15:25Z |
| 2026-10-19 15:36 | ORB-17055 | jrun-20261019-1536-t1 | stale decision refused: task changed after the failure | system moved to done at 2026-10-19T15:37Z |
| 2026-10-22 03:25 | ORB-17520 | jrun-20261022-0324-t1 | stale decision refused: task changed after the failure | codex moved to review at 2026-10-22T03:29Z |
| 2026-10-25 03:10 | ORB-18052 | jrun-20261025-0310-t1 | stale decision refused: task changed after the failure | system moved to done at 2026-10-25T03:30Z |
