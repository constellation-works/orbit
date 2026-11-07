---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-16359, ORB-18103, ORB-18135, ORB-18547, ORB-19818]
---

# Landing review verdict wrong or incomplete

## Signature

The reviewer returns `accept_with_fixes` with zero changes, rejects on one criterion, or the settlement gate downgrades an accept to `incomplete` (`validation_contradicted`; counterfactual mutation targets compared against the implementation commit only).

## Mechanism

The review gate compares evidence against the wrong baseline, cannot run Linux-only tests on a Mac reviewer, or misses a guard test. Some final-recovery diagnoses were refuted at triage (ORB-18135, F2026-10-586: an empty `required_validation_commands` does not block landing).

## Resolution

Verify the claim against the PR before acting. Operator resolutions seen: run the missing tests on the right host and requeue onto the kept candidate (ORB-18547, twice), requeue onto the candidate after an independent review (ORB-19818), discard and implement fresh when the PR is red (ORB-18135), decide a criterion (ORB-18103).

## Fix task

No open fix task found.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-17 05:03 | ORB-16359 | jrun-20261017-0342-c1 | settlement gate validation_contradicted on reviewer repair | claude moved to review at 2026-10-17T05:31Z |
| 2026-10-17 05:05 | ORB-16359 | jrun-20261017-0503-t3 | review accept_with_fixes downgraded to incomplete | claude moved to review at 2026-10-17T05:31Z |
| 2026-10-25 05:37 | ORB-18103 | jrun-20261025-0454-c4 | review rejected on F3, criterion 6 QA used a stub | human forced blocked->done at 2026-10-25T05:51Z |
| 2026-10-25 08:24 | ORB-18135 | jrun-20261025-0815-c3 | review missed a red guard test; gate diagnosis refuted | operator requeued at 2026-10-25T08:33Z |
| 2026-10-26 15:07 | ORB-18547 | jrun-20261026-1425-c1 | review incomplete on Linux-only sandbox tests | operator requeued at 2026-10-26T15:10Z |
| 2026-10-29 16:35 | ORB-19818 | jrun-20261029-1617-c1 | accept_with_fixes with zero changes | operator requeued at 2026-10-29T16:57Z |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 1 source occurrence(s) on 1 task(s); route `run_failure`, disposition `counted`. Change vs prior pass (ORB-22099): `shrank` (prior 2). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-29T16:39:47.118788157Z | ORB-19818 | jrun-20261029-1617-c1 | landing review verdict wrong or incomplete; source ORB-19818 history:blocked_triage@2026-10-29T16:39:47.118788157Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 2 occurrence(s) on 2 task(s) (ORB-18103, ORB-18135); below the recurrence threshold; route `run_failure`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: new vs prior pass (ORB-22481 slice had 0). Below threshold.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

2 of these occurrence(s) are already recorded in the rows above (ORB-18103, ORB-18135); they are not re-added.
