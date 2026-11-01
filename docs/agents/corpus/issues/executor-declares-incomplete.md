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
