---
type: issue
applies_to: [task_pilot, agent_implement]
status: draft
links: [ORB-21875, ORB-20620]
---

# Design change must explicitly supersede an earlier constraint

## Signature

`advisory utility_warnings: heartbeat design must explicitly supersede prior no-heartbeat decision`.

## Mechanism

The new sync/poke/heartbeat design revisited the recorded no-heartbeat decision. The pilot called for explicit supersession and said this is design work owned by the task, not a delivery blocker.

## Resolution

Update the maintained design and affected decisions together; preserve the advisory status of this finding.

## Fix task

No new fix: advisory observations below the recurrence threshold.

## Occurrences

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 1 source occurrence on 1 task; route `authoring`, disposition `counted`.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T23:30:26.936911009Z | ORB-20620 | jrun-20261031-2328-c1 | advisory utility_warnings: heartbeat design must explicitly supersede prior no-heartbeat decision; The task's sync on pull and any poke or heartbeat revisits recorded decisions: distributed-drain 2_design.md line 944 ('no heartbeat or failure inference'); distributed-drain 4_decisions.md, section 'in-progress plus a held task lock is the claim' (lines 56-61, where a lease-and-heartbeat scheme was rejected); and distributed-drain 3_vision.md lines 28-33 (liveness deferred). The new decisions doc must supersede these explicitly. This is design work the task owns, not a delivery blocker.; source ORB-20620 comments:comment@2026-10-31T23:30:26.936911009Z | external_merge_settled done at 2026-11-01T00:13:39.993109179Z |
