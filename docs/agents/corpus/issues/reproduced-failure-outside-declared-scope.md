---
type: issue
applies_to: [final_recovery, agent_implement]
status: active
links: [ORB-20503, ORB-21875, ORB-22481]
---

# A reproduced failure sits outside the task's declared scope

## Signature

The candidate passes the assertion it was filed to fix, and a second criterion fails for a behavior the task's text says not to repair. Triage disposition is `decision`: authorize a separate repair, or waive the extra criterion.

## Mechanism

The implementer reproduces a real failure while finishing the filed fix, then stops because repairing it would exceed the task. Final recovery escalates the choice instead of widening the task. ORB-20503's horizon fixture passed its 90-day assertion; the phone navigation bounds failure was a separate dashboard behavior.

## Resolution

Keep the original candidate. Put the extra failure in a new task with its own scope. Do not stretch the filed task to cover it, and do not waive the extra criterion without an explicit decision.

## Fix task

None. One occurrence in this window. The human answer to the proposed decision was not in the collected snapshot.

## Occurrences

Recorded by ORB-20998 from the retained collection.

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31 16:50 | ORB-20503 | jrun-20261031-1634-c4 | fixture assertion passed; phone navigation failure is outside the fixture-only scope | triage disposition=decision outcome=proposed at 2026-10-31T16:59Z; answer not in the snapshot |

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 1 source occurrence on 1 task; route `authoring`, disposition `counted`. Rows below confirm existing episodes where already backfilled; they are not additional incidents.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T16:59:50.297125033Z | ORB-20503 | jrun-20261031-1634-c4 | decision: separate repair or expand fixture-only scope; implement_one; reproduced phone navigation bounds failure; decision: horizon assertion passes, phone navigation bounds -13..71 vs row 8..367 fails; source mechanism unconfirmed; source ORB-20503 history:blocked_task_triaged@2026-10-31T16:59:50.297125033Z | external_merge_settled done at 2026-10-31T17:59:06.522047628Z |

### ORB-22481 owner scan, 2026-11-07

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-31T12:50:12.003276941Z` (resume pass continuing ORB-22246). This slice counts 1 source occurrence on 1 task; route `authoring`, disposition `counted`. Rows confirm the existing episode.

Durable sources: the task `history` and `comments` read with `orbit.task.show` and the run record read with `orbit run show <run> --no-reconcile --json` remain authoritative; ORB-22481 attaches `pilot-block-report.json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T16:50:59Z | ORB-20503 | jrun-20261031-1634-c4 | final_recovery escalate: horizon assertion passes but phone navigation bounds -13..71 vs 8..367 fails outside fixture-only scope; triage blocked_task_triaged disposition=decision outcome=proposed@16:59:50Z; source ORB-20503 history | external_merge_settled done 2026-10-31T17:59Z |
