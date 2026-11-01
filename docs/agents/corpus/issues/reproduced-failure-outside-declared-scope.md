---
type: issue
applies_to: [final_recovery, agent_implement]
status: active
links: [ORB-20503]
---

# A reproduced failure sits outside the task's declared scope

## Signature

The candidate passes the assertion it was filed to fix, and a second criterion fails for a behavior the task's text says not to repair. Triage disposition is `decision`: authorize a separate repair, or waive the extra criterion.

## Mechanism

The implementer reproduces a real failure while finishing the filed fix, then stops because repairing it would exceed the task. Final recovery escalates the choice instead of widening the task. ORB-20503's horizon fixture passed its 90-day assertion; the phone navigation bounds failure was a separate dashboard behavior.

## Resolution

Keep the original candidate. Put the extra failure in a new task with its own scope. Do not stretch the filed task to cover it, and do not waive the extra criterion without an explicit decision.

## Fix task

None. One occurrence in this window. The human answer to the proposed decision was not in the collected snapshot.

## Occurrences

Recorded by ORB-20998 from the retained collection.

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31 16:50 | ORB-20503 | jrun-20261031-1634-c4 | fixture assertion passed; phone navigation failure is outside the fixture-only scope | triage disposition=decision outcome=proposed at 2026-10-31T16:59Z; answer not in the snapshot |
