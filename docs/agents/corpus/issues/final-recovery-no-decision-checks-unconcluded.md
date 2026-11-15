---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18096, ORB-18124, ORB-18542, ORB-18816]
---

# Final recovery returns no decision while required checks are unconcluded

## Signature

Escalation text: `final recovery activity final_recovery did not return a decision: completion recovery required checks have not all concluded; no complete failure diagnosis is available`.

## Mechanism

The recovery agent ran while the PR's required checks were still pending, so it could not diagnose; the engine escalates instead of waiting. Triage found genuine content conflicts behind some of them (ORB-18096) and an Orbit defect (F2026-10-653, ORB-18816).

## Resolution

Do not treat the escalation as a diagnosis. Check whether checks have since concluded and rerun recovery; if a conflict is real, hand it to an operator with the exact refusal. Three rows from 2026-10-25 18:36 (ORB-18269, ORB-18270, ORB-18271) carried the same `did not return a decision` prefix but their diagnosis is `worktree_mismatch`; they moved to `worktree-missing-at-dispatch`.

## Fix task

No open fix task found; ORB-19030 (pending required checks escalate instead of retrying) is landed (2026-10-27T06:28Z, PR #6452); every occurrence recorded here precedes it and no later occurrence has been recorded.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 05:54 | ORB-18096 | jrun-20261025-0448-c8 | no decision, checks unconcluded | human forced blocked->done at 2026-10-25T06:05Z |
| 2026-10-25 08:48 | ORB-18124 | jrun-20261025-0757-c31 | no decision, checks unconcluded | human:daniel moved to backlog at 2026-10-25T08:49Z |
| 2026-10-26 08:29 | ORB-18542 | jrun-20261026-0716-c9 | no decision, checks unconcluded | merged outside Orbit at 2026-10-26T08:54Z, settled done |
| 2026-10-27 02:47 | ORB-18816 | jrun-20261027-0224-c3 | no decision, checks unconcluded | no clearing event in history; task now done |

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 4 occurrence(s) on 4 task(s) (ORB-18096, ORB-18124, ORB-18542, ORB-18816); recurring; route `recovery`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: new vs prior pass (ORB-22481 slice had 0). Covered by done ORB-19030 (landed 2026-10-27T06:28Z: pending required checks retry instead of escalating); every occurrence in this slice precedes it and the corpus records none after it. The guard text remains at crates/orbit-engine/src/executor/automation/vcs/pr/recovery.rs:47.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

4 of these occurrence(s) are already recorded in the rows above (ORB-18096, ORB-18124, ORB-18542, ORB-18816); they are not re-added.
