---
type: issue
applies_to: [final_recovery, triage_escalation]
status: draft
links: [ORB-19242]
---

# Cited commit missing from the owner repository

## Signature

A triage blocker cites a commit that `git cat-file` cannot find in the owner repository, so the candidate cannot be requeued until an operator fetches it.

## Mechanism

The generated task body or review cited a revision introduced by a batch that was never fetched into the owner repository.

## Resolution

An operator fetches the commit, confirms it with `git cat-file`, then requeues; if it exists nowhere, the citing batch is filed against. Uncertainty: no source fix was verified in this pass.

## Fix task

No fix task filed in ORB-22246; counted only (below recurrence threshold in this slice).

## Occurrences

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 1 source occurrence(s) on 1 task(s); route `run_failure`, disposition `counted`. Change vs prior pass (ORB-22099): `new` (prior 0). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-28T00:00:31.296484320Z | ORB-19242 | jrun-20261027-2353-t1 | cited commit missing from owner repository; operator fetch and requeue; source ORB-19242 history:blocked_triage@2026-10-28T00:00:31.296484320Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
