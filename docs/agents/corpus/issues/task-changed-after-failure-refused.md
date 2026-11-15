---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-16063, ORB-16616, ORB-17520, ORB-18052, ORB-15206, ORB-21875, ORB-21229, ORB-22481]
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
| 2026-10-30 07:23 | ORB-20115 | jrun-20261030-0720-t2 | task is already archived; final recovery never reopens settled work | recovery decision archive was refused; no triage marker in the ORB-20998 collection |

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 1 source occurrence on 1 task; route `recovery`, disposition `counted`. Rows below confirm existing episodes where already backfilled; they are not additional incidents.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-02T03:30:22.703477930Z | ORB-21229 | jrun-20261102-0329-t1 | outcome=refused: task changed after failure; final_recovery; later operator decision stands; task changed after the failure (was blocked at 2026-11-02T03:28:56.631967965+00:00, now backlog at 2026-11-02T03:29:31.286067434+00:00); the later decision stands; source ORB-21229 history:final_recovery_decided@2026-11-02T03:30:22.703477930Z | status_changed done at 2026-11-02T04:10:58.680990206Z |

Correct stale-decision refusal, not an escalated block or repair request. Included for complete record accounting.

### ORB-22481 owner scan, 2026-11-07

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-31T12:50:12.003276941Z` (resume pass continuing ORB-22246). This slice counts 1 source occurrence on 1 task; route `recovery`, disposition `counted`.

Durable sources: the task `history` and `comments` read with `orbit.task.show` and the run record read with `orbit run show <run> --no-reconcile --json` remain authoritative; ORB-22481 attaches `pilot-block-report.json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-02T03:30:22Z | ORB-21229 | jrun-20261102-0329-t1 | final_recovery decision=decide outcome=refused: task changed after the failure; the later decision stands; source ORB-21229 history:final_recovery_decided | correct stale-decision refusal; ORB-21229 done 2026-11-02T04:10Z |

Not an escalated block; included for complete accounting. The preceding block was the SIGTERM interruption in `worker-killed-sigterm-before-landing`.

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 1 occurrence(s) on 1 task(s) (ORB-18052); below the recurrence threshold; route `recovery`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: held vs prior pass (ORB-22481 slice had 1). Below threshold; correct stale-decision refusal, not a real block.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

1 of these occurrence(s) are already recorded in the rows above (ORB-18052); they are not re-added.
