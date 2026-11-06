---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-13964, ORB-13984, ORB-20339, ORB-20443]
---

# Sandbox policy refuses a deny-modify path that does not exist

## Signature

`policy denied: linux-bwrap cannot enforce absent denyModify '<path>'` or `invalid input: denyModify match '<path>' must exist and resolve canonically: No such file or directory`.

## Mechanism

The recovery activity's sandbox profile expands a deny glob to a path that is absent in that worktree, and the Linux profile cannot enforce a missing path, so the run is refused before it starts.

## Resolution

Do not retry the recovery run. Do not requeue; take the task by hand. Triage recorded it as an Orbit defect and tied ORB-20339 to F2026-10-712 and ORB-20352 item 1.

## Fix task

No open fix task found. ORB-20443 (dangling deny-glob links) is landed and by search related, not verified to cover this signature.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-04 20:05 | ORB-13964 | jrun-20261004-2005-t1 | denyModify path absent, blocked-task recovery run failed | Daniel forced blocked to done at 2026-10-04T20:15Z |
| 2026-10-04 21:08 | ORB-13984 | jrun-20261004-2108-t1 | denyModify path absent, blocked-task recovery run failed | Daniel forced blocked to proposed at 2026-10-04T21:09Z |
| 2026-10-31 02:11 | ORB-20339 | jrun-20261031-0153-c19 | denyModify match must exist | human forced blocked->done at 2026-10-31T02:20Z |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 1 source occurrence(s) on 1 task(s); route `setup`, disposition `counted`. Change vs prior pass (ORB-22099): `new` (prior 0). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T02:15:35.480519373Z | ORB-20339 | jrun-20261031-0153-c19 | sandbox policy refuses a deny-modify path that does not exist; source ORB-20339 history:blocked_triage@2026-10-31T02:15:35.480519373Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
