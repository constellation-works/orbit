---
type: issue
applies_to: [final_recovery]
status: draft
links: [ORB-20339]
---

# Store upgrade pending at admission

## Signature

Admission reports a process awaiting migration to a newer store schema (`upgrade_pending`), so final recovery cannot proceed.

## Mechanism

A store schema migration is pending on the host; no environment improvement was recorded.

## Resolution

Complete the store migration on the host, then requeue. Uncertainty: migration ownership was not verified in this pass.

## Fix task

No fix task filed in ORB-22246; counted only (below recurrence threshold in this slice).

## Occurrences

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 1 source occurrence(s) on 1 task(s); route `setup`, disposition `counted`. Change vs prior pass (ORB-22099): `new` (prior 0). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T01:48:56.988617531Z | ORB-20339 | jrun-20261031-0130-c1 | store upgrade pending at admission (awaiting schema migration); source ORB-20339 history:blocked_triage@2026-10-31T01:48:56.988617531Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
