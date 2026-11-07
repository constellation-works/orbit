---
type: issue
applies_to: [agent_implement, final_recovery]
status: active
links: [ORB-20125, ORB-19588, ORB-20132]
---

# Minted sweep contract cannot be satisfied in one leaf

## Signature

`incomplete_code_health_sweep`: the sweep selects `orbit-core/application` (652-688 files, 175-188k non-test lines) and fully reads 758-1780 lines; the criterion `every non-test line in it was read` cannot be met.

## Mechanism

The effective code-health sweep definition still uses the old one-level Pass B partition, and the task was minted from it, so the same area fails the same way on every pass.

## Resolution

Do not requeue; it fails again. Fix the effective definition first. Daniel forced ORB-19588 to done on 2026-10-28T21:41Z; the on-call operator synced the definition to the recursive 20000-line partition and requeued ORB-20125 on 2026-10-30T09:38Z (ORB-20132).

## Fix task

ORB-20132 (on-call, in review, not landed).

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-28 21:33 | ORB-19588 | jrun-20261028-2111-c16 | incomplete_code_health_sweep | human forced blocked->done at 2026-10-28T21:41Z |
| 2026-10-30 08:58 | ORB-20125 | jrun-20261030-0838-c26 | stale minted partition, sweep unreachable | operator requeued at 2026-10-30T09:38Z |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 3 source occurrence(s) on 3 task(s); route `authoring`, disposition `counted`. Change vs prior pass (ORB-22099): `new` (prior 0). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-27T19:44:00.194077208Z | ORB-19150 | jrun-20261027-1930-c17 | sweep contract counts/partition cannot be satisfied in one leaf (incomplete_scenario_counts / incomplete_code_health_sweep); source ORB-19150 history:blocked_triage@2026-10-27T19:44:00.194077208Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-28T21:37:09.777768618Z | ORB-19588 | jrun-20261028-2111-c16 | sweep contract counts/partition cannot be satisfied in one leaf (incomplete_scenario_counts / incomplete_code_health_sweep); source ORB-19588 history:blocked_triage@2026-10-28T21:37:09.777768618Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-30T09:02:05.953604399Z | ORB-20125 | not recorded in triage comment | sweep contract counts/partition cannot be satisfied in one leaf (incomplete_scenario_counts / incomplete_code_health_sweep); source ORB-20125 history:blocked_triage@2026-10-30T09:02:05.953604399Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 1 occurrence(s) on 1 task(s) (ORB-19150); below the recurrence threshold; route `authoring`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: new vs prior pass (ORB-22481 slice had 0). Below threshold.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

1 of these occurrence(s) are already recorded in the rows above (ORB-19150); they are not re-added.
