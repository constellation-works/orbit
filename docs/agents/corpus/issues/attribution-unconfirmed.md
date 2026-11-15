---
type: issue
applies_to: [agent_implement, final_recovery]
status: active
links: [ORB-20306]
---

# Implementer returns attribution_unconfirmed

## Signature

`implement_one` returned `attribution_unconfirmed`; step recovery verified `not_recovered` and admitted no retry; the branch is clean at the run base.

## Mechanism

The implementer could not confirm attribution of its result; the perf evidence it produced sits only under the run worktree's `.orbit/tmp/perf`.

## Resolution

Preserve `.orbit/tmp/perf` before the worktree is garbage-collected, then decide by hand.

## Fix task

No fix task; one occurrence.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31 03:28 | ORB-20306 | jrun-20261031-0255-c5 | attribution_unconfirmed | human:daniel moved to archived at 2026-10-31T05:15Z |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 1 source occurrence(s) on 1 task(s); route `run_failure`, disposition `counted`. Change vs prior pass (ORB-22099): `new` (prior 0). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T03:32:12.612576429Z | ORB-20306 | not recorded in triage comment | implementer returns attribution_unconfirmed; source ORB-20306 history:blocked_triage@2026-10-31T03:32:12.612576429Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
