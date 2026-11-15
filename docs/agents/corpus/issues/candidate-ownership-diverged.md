---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18340]
---

# Open PR carries a head this run did not publish

## Signature

`Completion is blocked by candidate ownership: this run published 43d541cf..., but open PR #6158 now carries dbb1e300..., a separate merge`; delivery evidence does not authenticate the newer head.

## Mechanism

A commit outside the run (a merge of agent-main into the branch) moved the PR head, so completion cannot prove the head is this run's.

## Resolution

Reconcile the newer head by hand and authorize governed delivery of the selected head.

## Fix task

No fix task: the guard works as designed.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 23:22 | ORB-18340 | jrun-20261025-2224-c12 | PR head moved by a commit outside the run | merged outside Orbit at 2026-10-25T23:32Z, settled done |

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 1 occurrence(s) on 1 task(s) (ORB-18340); below the recurrence threshold; route `recovery`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: new vs prior pass (ORB-22481 slice had 0). Below threshold.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

1 of these occurrence(s) are already recorded in the rows above (ORB-18340); they are not re-added.
