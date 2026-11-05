---
type: issue
applies_to: [agent_implement, final_recovery]
status: resolved
links: [ORB-20625, ORB-19962, ORB-20129, ORB-20137, ORB-20455]
---

# Executor returns an incomplete envelope early with no timeout

## Signature

The step exits 0 with envelope `status: failed` and `incomplete_examination`, `incomplete_scan`, `audit_incomplete`, `scan_incomplete` or `incomplete_scenario_counts` after 49-114 s of a multi-hour deadline, with no timeout, stall or environment fault.

## Mechanism

A frozen-batch review or sweep declares itself unfinished instead of continuing. The executor treats exit 0 plus declared failure as a step failure, and step recovery returns `not_recovered` because implement is not a delivery-tail step.

## Resolution

Do not requeue blindly; resume the failed checkpoint (`orbit job resume <run> --wait`) or have an operator continue supervised. Daniel closed several of these as done by hand (ORB-19150, ORB-19960, ORB-19962).

## Fix task

ORB-20625 (done, PR #7390, 4217d070f, 2026-11-01T01:03Z). Effectiveness unmeasured; the ORB-20623 report listed the effect as too early.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-27 03:39 | ORB-18950 | jrun-20261027-0334-c3 | examination_incomplete | human:daniel moved to archived at 2026-10-27T03:44Z |
| 2026-10-27 19:40 | ORB-19150 | jrun-20261027-1930-c17 | incomplete_scenario_counts | human forced blocked->done at 2026-10-27T20:06Z |
| 2026-10-29 22:51 | ORB-19960 | jrun-20261029-2251-t1 | incomplete_examination | human forced blocked->done at 2026-10-29T22:54Z |
| 2026-10-29 23:11 | ORB-19962 | jrun-20261029-2309-c3 | incomplete_examination | human forced blocked->done at 2026-10-29T23:45Z |
| 2026-10-30 08:41 | ORB-20129 | jrun-20261030-0838-c25 | incomplete_scan | operator requeued at 2026-10-30T09:50Z |
| 2026-10-30 08:46 | ORB-20137 | jrun-20261030-0842-c4 | audit_incomplete | operator requeued at 2026-10-30T09:50Z |
| 2026-10-31 08:39 | ORB-20455 | jrun-20261031-0836-c16 | scan_incomplete | human forced blocked->rejected at 2026-10-31T08:52Z |
| 2026-10-27 19:44 | ORB-19150 | jrun-20261027-1930-c17 (final_recovery) | executor declares incomplete | blocked_task_triaged human_only in ORB-21505 resume pass |
| 2026-10-30 08:45 | ORB-20129 | jrun-20261030-0838-c25 (final_recovery) | executor declares incomplete (incomplete_scan) | blocked_task_triaged human_only in ORB-21505 resume pass |
| 2026-10-30 23:23 | ORB-20310 | jrun-20261030-2316-c3 (implementation); jrun-20261030-2320-t2 (final_recovery) | executor declares incomplete (audit_incomplete) | blocked_task_triaged decision in ORB-21505 resume pass |
| 2026-10-31 08:42 | ORB-20455 | jrun-20261031-0836-c16 (final_recovery) | executor declares incomplete (scan_incomplete) | blocked_task_triaged human_only in ORB-21505 resume pass |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 5 source occurrence(s) on 5 task(s); route `run_failure`, disposition `counted`. Change vs prior pass (ORB-22099): `grew` (prior 1). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-29T22:51:55.932753681Z | ORB-19960 | jrun-20261029-2251-t1 | executor returns incomplete examination/scan/audit early with no timeout; source ORB-19960 history:final_recovery@2026-10-29T22:51:55.932753681Z | final_recovery disposition=escalated recorded in ORB-22246 resume pass |
| 2026-10-29T23:15:27.163699320Z | ORB-19962 | jrun-20261029-2309-c3 | executor returns incomplete examination/scan/audit early with no timeout; source ORB-19962 history:blocked_triage@2026-10-29T23:15:27.163699320Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-30T08:45:08.414981133Z | ORB-20129 | not recorded in triage comment | executor returns incomplete examination/scan/audit early with no timeout; source ORB-20129 history:blocked_triage@2026-10-30T08:45:08.414981133Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-30T08:50:21.017930639Z | ORB-20137 | not recorded in triage comment | executor returns incomplete examination/scan/audit early with no timeout; source ORB-20137 history:blocked_triage@2026-10-30T08:50:21.017930639Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-31T08:42:56.814446639Z | ORB-20455 | jrun-20261031-0836-c16 | executor returns incomplete examination/scan/audit early with no timeout; source ORB-20455 history:blocked_triage@2026-10-31T08:42:56.814446639Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
