---
type: issue
applies_to: [agent_implement, final_recovery]
status: draft
links: [ORB-21875, ORB-21111]
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

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact.get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01T23:48:19.630426644Z | ORB-21111 | jrun-20261101-2345-c3 | decision: retain open canonical owners or reassess terminal legacy ownership; implement_one; ownership eligibility question; No code/validation receipts; operator chose keep-open-owner option 1 at 2026-11-02T00:59:00Z and requeued.; source ORB-21111 history:final_recovery_decided@2026-11-01T23:48:19.630426644Z | review_approved done at 2026-11-02T03:34:27.103395859Z |
