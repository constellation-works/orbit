---
type: issue
applies_to: [agent_implement]
status: active
links: [ORB-18090, ORB-18099, ORB-18088]
---

# Task depends on a change that has not landed

## Signature

Pilot hold: `requires the review-skip mapping syntax from ORB-18088, which is absent at the pinned source_revision` or `the task's prerequisite PR #5983 is not present`.

## Mechanism

The task was filed ahead of the change it builds on, so its criteria cannot be met until that lands.

## Resolution

Leave the task held until the prerequisite lands, then re-pilot. Do not implement the prerequisite inside the task.

## Fix task

No fix task: add or confirm a `blocked_by` relation.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 04:07 | ORB-18090 | jrun-20261025-0401-t1 | prerequisite ORB-18088 not shipped | system moved to done at 2026-10-25T05:25Z |
| 2026-10-25 04:33 | ORB-18090 | jrun-20261025-0422-c1 | prerequisite ORB-18088 not shipped | system moved to done at 2026-10-25T05:25Z |
| 2026-10-25 04:47 | ORB-18099 | jrun-20261025-0441-t1 | prerequisite PR #5983 not landed | human:daniel moved to someday at 2026-10-25T06:40Z |
| 2026-10-25 04:57 | ORB-18099 | jrun-20261025-0452-c1 | prerequisite PR #5983 not landed | human:daniel moved to someday at 2026-10-25T06:40Z |
| 2026-10-26 17:16 | ORB-18674 | not recorded in triage comment | task depends on prerequisite change that has not landed | occurrence recorded in ORB-21385 partial pass; pre-cursor, no new action in this pass |
| 2026-10-27 01:26 | ORB-18782 | not recorded in triage comment | task depends on prerequisite change that has not landed | occurrence recorded in ORB-21385 partial pass; pre-cursor, no new action in this pass |
| 2026-10-27 01:28 | ORB-18783 | not recorded in triage comment | task depends on prerequisite change that has not landed | occurrence recorded in ORB-21385 partial pass; pre-cursor, no new action in this pass |

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 4 occurrence(s) on 2 task(s) (ORB-18090, ORB-18099); recurring; route `authoring`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: new vs prior pass (ORB-22481 slice had 0). Correct holds: ORB-18099 named an unlanded PR with no task edge possible. ORB-18090 (2 occurrences) was held by a utility warning restating its recorded blocked_by ORB-18088 one day after done ORB-17753 (landed 2026-10-24T03:37Z, a prerequisite already a dependency is not a pilot warning); deployment timing of that fix on 2026-10-25 is not established. No occurrence after 2026-10-25 in any later slice of the corpus.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

4 of these occurrence(s) are already recorded in the rows above (ORB-18090, ORB-18099); they are not re-added.
