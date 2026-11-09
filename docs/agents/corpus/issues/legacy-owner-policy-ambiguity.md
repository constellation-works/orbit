---
type: issue
applies_to: [agent_implement, final_recovery]
status: draft
links: [ORB-21875, ORB-21111, ORB-22481]
---

# Legacy owner eligibility requires an explicit policy

## Signature

`decision: retain open canonical owners or reassess terminal legacy ownership; implement_one; ownership eligibility question`.

## Mechanism

The implementer stopped on whether completed and archived legacy owners should suppress a new active finding. Final recovery offered a policy decision; the human retained open canonical owners. The records do not establish a general inability to implement.

## Resolution

Use the recorded open-owner decision for this task. A future task changing terminal-owner eligibility must state that policy explicitly; do not infer a new policy from historical matches.

## Fix task

No new fix: one occurrence. ORB-21111 later delivered; ORB-21193 separately tightened persistence instructions.

## Occurrences

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 1 source occurrence on 1 task; route `authoring`, disposition `counted`.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01T23:48:19.630426644Z | ORB-21111 | jrun-20261101-2345-c3 | decision: retain open canonical owners or reassess terminal legacy ownership; implement_one; ownership eligibility question; No code/validation receipts; operator chose keep-open-owner option 1 at 2026-11-02T00:59:00Z and requeued.; source ORB-21111 history:final_recovery_decided@2026-11-01T23:48:19.630426644Z | review_approved done at 2026-11-02T03:34:27.103395859Z |

### ORB-22481 owner scan, 2026-11-07

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-31T12:50:12.003276941Z` (resume pass continuing ORB-22246). This slice counts 1 source occurrence on 1 task; route `authoring`, disposition `counted`.

Durable sources: the task `history` and `comments` read with `orbit.task.show` and the run record read with `orbit run show <run> --no-reconcile --json` remain authoritative; ORB-22481 attaches `pilot-block-report.json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01T23:48:19Z | ORB-21111 | jrun-20261101-2345-c3 | final_recovery decision=decide: implementation stopped with no changes; choose whether to keep the open-task ownership policy or reassess completed and archived legacy tasks; source ORB-21111 history:final_recovery_escalated | operator refined; ORB-21111 delivered, done 2026-11-02T03:34Z |
