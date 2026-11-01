---
type: issue
applies_to: [task_pilot, agent_implement]
status: active
links: [ORB-21003]
---

# A criterion waits on references only the operator can attach

## Signature

Pilot `utility_warnings` plus `host_operational`: the criterion cannot be completed until an operator attaches admitted references. The task's own comment says the attachment is human-only and was not run. The managed lane has no such attachment action.

## Mechanism

The filing makes the deliverable depend on files the operator has not attached, and it tells the lane not to invent them. Promotion holds. ORB-21003's references were proposed images; the comment said evaluation waited on the real ones. The capture tool renders `mocks/` and does not record evidence.

## Resolution

The operator attaches the named references, then the task can be piloted again. Do not substitute proposed images or a mock capture for the admitted references. Do not requeue the lane while the comment still says attachment is human-only.

## Fix task

None. One held task in this window, and it had no triage marker. Below the recurrence threshold.

## Occurrences

Recorded by ORB-20998 from the retained collection. This is the pilot hold itself; there is no triage run id.

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01 18:08 | ORB-21003 | not recorded (task_pilot_applied, no triage marker) | operator must attach admitted reference images | host_operational hold; still proposed in the snapshot |
