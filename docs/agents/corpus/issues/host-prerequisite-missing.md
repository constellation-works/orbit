---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18438, ORB-20007]
---

# Criterion needs a host tool or funded policy the host lacks

## Signature

Environmental blocker unchanged: `command -v podman` and `command -v docker` find nothing (ORB-18438), or `orbit config get assistance --json` shows policies=[], unfunded budget, zero ceilings (ORB-20007).

## Mechanism

The acceptance criterion needs a prerequisite no reachable host provides, so a retry fails identically while the implementation sits complete and uncommitted.

## Resolution

Do not requeue. Name the missing prerequisite and its owner in the escalation. Daniel moved ORB-20007 to someday on 2026-10-30.

## Fix task

No open fix task found.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-26 04:02 | ORB-18438 | jrun-20261026-0356-c3 | podman and docker absent | claude moved to review at 2026-10-26T05:13Z |
| 2026-10-30 05:15 | ORB-20007 | jrun-20261030-0500-c1 | unfunded assistance policy, labelers missing | codex moved to backlog at 2026-10-30T05:33Z |
| 2026-10-30 05:41 | ORB-20007 | jrun-20261030-0533-c3 | unfunded assistance policy | human forced blocked->someday at 2026-10-30T07:33Z |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 2 source occurrence(s) on 1 task(s); route `setup`, disposition `counted`. Change vs prior pass (ORB-22099): `grew` (prior 1). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-30T05:18:21.529016237Z | ORB-20007 | jrun-20261030-0500-c1 | criterion needs host tool, human labelers or funded policy the host lacks; source ORB-20007 history:blocked_triage@2026-10-30T05:18:21.529016237Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-30T05:45:18.279437651Z | ORB-20007 | jrun-20261030-0533-c3 | criterion needs host tool, human labelers or funded policy the host lacks; source ORB-20007 history:blocked_triage@2026-10-30T05:45:18.279437651Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 1 occurrence(s) on 1 task(s) (ORB-18438); below the recurrence threshold; route `setup`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: new vs prior pass (ORB-22481 slice had 0). Below threshold.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

1 of these occurrence(s) are already recorded in the rows above (ORB-18438); they are not re-added.
