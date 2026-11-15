---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-19781, ORB-19785, ORB-19786]
---

# Two tasks own the same friction

## Signature

`criteria AC5/AC8 are unmet for F2026-10-685: it is covered by both ORB-19785 and ORB-19786`, and rejecting the duplicate was refused because both were in-progress.

## Mechanism

Friction curation assigned one friction to two live tasks; the duplicate cannot be closed while both run.

## Resolution

Inspect the tasks and their evidence before treating either as a duplicate. In this occurrence the operator identified two distinct defects, kept both tasks, and retargeted one friction relation; the curation was then retried.

## Fix task

No open fix task found.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-29 08:16 | ORB-19781 | jrun-20261029-0753-c31 | friction owned by two in-progress tasks | codex moved to backlog at 2026-10-29T08:41Z |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 1 source occurrence(s) on 1 task(s); route `recovery`, disposition `counted`. Change vs prior pass (ORB-22099): `new` (prior 0). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-29T08:16:38.162879566Z | ORB-19781 | jrun-20261029-0753-c31 | two tasks own the same friction; source ORB-19781 history:final_recovery@2026-10-29T08:16:38.162879566Z | final_recovery disposition=escalated recorded in ORB-22246 resume pass |
