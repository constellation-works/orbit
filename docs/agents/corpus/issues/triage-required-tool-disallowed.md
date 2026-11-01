---
type: issue
applies_to: [final_recovery]
status: resolved
links: [ORB-18246, ORB-18245, ORB-18269, ORB-18270]
---

# Escalation triage refused: a required tool is disallowed

## Signature

The escalation-triage run fails at dispatch: `task ORB-18271 required tool orbit.task.add failed admission: tool 'orbit.task.add' is in the activity disallow list`.

## Mechanism

The triager's tool allowlist rejects any task whose `required_tools` include a tool the triager may not use, so the task never gets a triage disposition.

## Resolution

No agent action; the task has no triage disposition until the triager can run.

## Fix task

ORB-18246 (done).

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 13:55 | ORB-18245 | jrun-20261025-1355-t1 | triage run refused: required tool orbit.task.add disallowed | system moved to done at 2026-10-25T14:08Z |
| 2026-10-25 18:39 | ORB-18270 | jrun-20261025-1839-t1 | triage run refused: required tool orbit.task.add disallowed | human forced blocked->proposed at 2026-10-25T19:02Z |
| 2026-10-25 18:39 | ORB-18271 | jrun-20261025-1839-t1 | triage run refused: required tool orbit.task.add disallowed | human:daniel moved to backlog at 2026-10-25T19:02Z |
| 2026-10-25 18:40 | ORB-18269 | jrun-20261025-1840-t1 | triage run refused: required tool orbit.task.add disallowed | human:daniel moved to backlog at 2026-10-25T19:03Z |
