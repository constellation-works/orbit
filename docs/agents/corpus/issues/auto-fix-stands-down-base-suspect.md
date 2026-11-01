---
type: issue
applies_to: [final_recovery]
status: resolved
links: [ORB-20578, ORB-19533]
---

# Auto-fix stands down on a red PR classed base-suspect

## Signature

History events `pr_auto_fix_base_red` and `ci_base_suspect` repeat on a task while its PR stays red, with a reason such as `base-suspect: ... waiting for the unchanged pull-request head to fail the same signature again` or `its cited repository files are outside the candidate's changed paths; waiting for the next push run`. No repair agent runs.

## Mechanism

A failure on a head whose base passed the same check was classed `Flaky`/base-suspect and the landing hold waited for a candidate rerun that nothing requested. ORB-20578 records the rerun-wait text as introduced by ORB-19533 (b1e26afb9, #6773). Earlier holds on 2026-10-25..27 carry an older reason form (cited files outside the changed paths), so the cause is wider than that one change.

## Resolution

Do not wait for the hold to clear on its own. Read the failing check, decide whether its test is inside the candidate's diff, and if the failure reproduces on an unchanged head treat the PR as red and repair or escalate it. After ORB-20578 (PR #7350, d3d87d29a) auto-fix repairs a red PR whose base is green.

## Fix task

ORB-20578 (done; settled 2026-10-31T22:33Z). ORB-20574's rerun-request approach (PR #7344) was rejected by Daniel on 2026-10-31.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 23:22 | ORB-18340 | jrun-20261025-2224-c12 | pr_auto_fix_base_red hold, PR left red | merged outside Orbit at 2026-10-25T23:32Z, settled done |
| 2026-10-26 08:11 | ORB-18559 | jrun-20261026-0728-c10 | pr_auto_fix_base_red hold, PR left red | merged outside Orbit at 2026-10-26T08:37Z, settled done |
| 2026-10-27 02:28 | ORB-18552 | jrun-20261027-0056-c5 | pr_auto_fix_base_red hold, PR left red | system moved to done at 2026-10-27T03:42Z |
| 2026-10-27 05:23 | ORB-18555 | jrun-20261027-0405-c25 | pr_auto_fix_base_red hold, PR left red | system moved to done at 2026-10-27T06:08Z |
| 2026-10-27 05:26 | ORB-18969 | jrun-20261027-0421-c3 | pr_auto_fix_base_red hold, PR left red | merged outside Orbit at 2026-10-27T09:09Z, settled done |
| 2026-10-28 03:58 | ORB-19289 | jrun-20261028-0302-c4 | pr_auto_fix_base_red hold, PR left red | merged outside Orbit at 2026-10-28T05:03Z, settled done |
| 2026-10-29 07:41 | ORB-19721 | jrun-20261029-0558-c19 | pr_auto_fix_base_red hold, PR left red | merged outside Orbit at 2026-10-29T08:43Z, settled done |
| 2026-10-29 14:56 | ORB-19845 | jrun-20261029-1333-c3 | pr_auto_fix_base_red hold, PR left red | merged outside Orbit at 2026-10-29T15:37Z, settled done |

The anchor incident for the fix, PR #7323 (ORB-20539, run jrun-20261031-1915-c3, `pr_auto_fix_base_red` at 2026-10-31T19:42:01Z), is documented in ORB-20578 and is outside the 149 tasks read in this backfill.
