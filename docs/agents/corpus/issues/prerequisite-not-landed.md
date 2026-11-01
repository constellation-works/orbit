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
