---
type: issue
applies_to: [agent_implement]
status: active
links: [ORB-19718]
---

# Pilot warning refuted at the pinned revision

## Signature

Triage `human_only`: `holding finding refuted at pinned revision (maintained design already enumerates candidates)`; the warning is stale.

## Mechanism

The pilot did not find the maintained list, so it held a task that was deliverable as written.

## Resolution

Verify against the pinned revision; reuse the existing delivery and do not dispatch a duplicate.

## Fix task

No fix task; one occurrence.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-29 05:32 | ORB-19718 | jrun-20261029-0528-c5 | stale holding finding | codex moved to review at 2026-10-29T06:04Z |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 1 source occurrence(s) on 1 task(s); route `pilot`, disposition `counted`. Change vs prior pass (ORB-22099): `new` (prior 0). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-29T05:32:16.388519091Z | ORB-19718 | not recorded in triage comment | pilot warning refuted at pinned revision; source ORB-19718 history:pilot_triage@2026-10-29T05:32:16.388519091Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
