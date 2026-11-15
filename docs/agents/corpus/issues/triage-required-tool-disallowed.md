---
type: issue
applies_to: [final_recovery]
status: resolved
links: [ORB-18246, ORB-18245, ORB-18269, ORB-18270]
---

# Escalation triage refused: a required tool is disallowed

## Signature

The escalation-triage run fails at dispatch: `task ORB-18271 required tool orbit.task.add failed admission: tool 'orbit.task.add' is in the activity disallow list`.

## Mechanism

The triager's tool allowlist rejected a task whose `required_tools` included a tool the triager could not use, so the task never got a triage disposition.

## Resolution

Non-implementer deny-mode activities now omit disallowed task requirements and record an omission note; the requirements still bind the implementing activity. Current escalation triage handles pilot-warning holds; the former blocked-task subject is retired and final recovery handles blocked tasks.

## Fix task

ORB-18246 (done).

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 13:55 | ORB-18245 | jrun-20261025-1355-t1 | triage run refused: required tool orbit.task.add disallowed | system moved to done at 2026-10-25T14:08Z |
| 2026-10-25 18:39 | ORB-18270 | jrun-20261025-1839-t1 | triage run refused: required tool orbit.task.add disallowed | human forced blocked->proposed at 2026-10-25T19:02Z |
| 2026-10-25 18:39 | ORB-18271 | jrun-20261025-1839-t1 | triage run refused: required tool orbit.task.add disallowed | human:daniel moved to backlog at 2026-10-25T19:02Z |
| 2026-10-25 18:40 | ORB-18269 | jrun-20261025-1840-t1 | triage run refused: required tool orbit.task.add disallowed | human:daniel moved to backlog at 2026-10-25T19:03Z |

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 1 occurrence(s) on 1 task(s) (ORB-18245); below the recurrence threshold; route `setup`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: new vs prior pass (ORB-22481 slice had 0). Below threshold.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

1 of these occurrence(s) are already recorded in the rows above (ORB-18245); they are not re-added.
