---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18164, ORB-19689, ORB-20625, ORB-17437, ORB-16193]
---

# Requeue or resume refused for the published candidate

## Signature

A resume of a preserved candidate fails admission: `published candidate lacks a unique requeue binding; inspect explicit source repair ...` (ORB-19689, F2026-10-685); the preserved requeue digest (crew=sol) differs from the current binding after a crew redraw (ORB-18164, F2026-10-594); the commit gate rejects HEAD against the immutable setup checkpoint (ORB-17437); or the failure handoff disagrees with the task's run record (ORB-20625).

## Mechanism

Adoption binds the candidate to the run, crew, requeue digest and failed step that produced it. A redraw, an extra preservation record, a forced status change or a failure handoff whose failed step (`implement_bundle`) differs from the step that raised the error (`implement_one`) breaks the match, so admission correctly refuses. The refusal text for ORB-20625 is recorded only as a summary in history.

## Resolution

Do not retry the same resume. Either an operator discards the candidate and requeues fresh, which is what Daniel did for ORB-20625 ("Discard and requeue") and ORB-18124, or an operator admits an authenticated same-PR source repair. When the cause is a redraw, file or reuse the friction rather than fixing admission inside an unrelated task.

## Fix task

No open fix task found. Landed related tasks by search, not verified to cover each occurrence: ORB-18560 (adoption across an admission redraw), ORB-19786 (authenticate published review-to-backlog moves), ORB-16438 (duplicate preservation record).

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-15 02:36 | ORB-16193 | jrun-20261015-0235-t3 | cancelled owner-local published candidate re-admission | codex moved to backlog at 2026-10-15T02:39Z |
| 2026-10-21 07:23 | ORB-17437 | jrun-20261021-0723-t1 | commit gate: HEAD differs from setup checkpoint | merged outside Orbit at 2026-10-21T17:08Z, settled done |
| 2026-10-25 10:39 | ORB-18164 | jrun-20261025-1037-c16 | requeue digest differs after crew redraw (F2026-10-594) | merged outside Orbit at 2026-10-25T10:40Z, settled done |
| 2026-10-29 07:54 | ORB-19689 | jrun-20261029-0753-c30 | `resume_candidate` refused: published candidate lacks a unique requeue binding (F2026-10-685; final_recovery comment 2026-10-29T07:54:24.195724045Z) | blocked_task_triage comment 2026-10-29T07:59:25.718066023Z requires supported authenticated same-PR source-repair admission for publishing run jrun-20261029-0437-c18; no requeue is safe until admission is repaired. Human forced blocked->rejected at 2026-10-29T21:37Z |
| 2026-11-01 00:36 | ORB-20625 | jrun-20261031-2344-c9 | failure handoff disagrees with the task's run record; resume refused | Daniel: "Discard and requeue"; fresh run jrun-20261101-0037-c3 landed PR #7390 at 2026-11-01T01:03Z |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 1 source occurrence(s) on 1 task(s); route `recovery`, disposition `counted`. Change vs prior pass (ORB-22099): `held` (prior 1). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-29T07:59:25.718066023Z | ORB-19689 | not recorded in triage comment | requeue/resume refused for published candidate (lacks unique requeue binding); source ORB-19689 history:blocked_triage@2026-10-29T07:59:25.718066023Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 1 occurrence(s) on 1 task(s) (ORB-18164); below the recurrence threshold; route `recovery`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: new vs prior pass (ORB-22481 slice had 0). Below threshold.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

1 of these occurrence(s) are already recorded in the rows above (ORB-18164); they are not re-added.
