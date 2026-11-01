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
