---
type: issue
applies_to: [final_recovery, triage_escalation]
status: draft
links: [ORB-21251, ORB-21645, ORB-22481]
---

# Pilot task needs a person-run exercise that automated runs cannot supply

## Signature

`final_recovery ... outcome=escalated` or `blocked_task_triage` on a live-pilot task whose criteria require an operator-granted resident mandate, a manager session on the owner host and a working baseline-metrics command. The agent run ends `pilot_prerequisites_unmet`, the triage proposal is `Done - requeue`, and the same stop recurs after the requeue because nothing it waits on changed.

## Mechanism

Not fully established. Final recovery on ORB-21251 (2026-11-02T22:07:55Z) reports a clean worktree with no candidate commits, zero resident inventory and zero `resident_authority` requests, and a correctly scoped baseline-metrics command that exits 1 with `task_not_found` for ORB-21083 whose underlying cause it records as undiagnosed. The second escalation (22:19:48Z) says the requeue answer had been given but no mandate, grant or exercise followed, and asks how to stop the loop. The last implementation run (`jrun-20261102-2359-c3`) failed `implement_one` with `error.code=pilot_prerequisites_unmet`. Whether the `task_not_found` baseline failure is an Orbit defect was not investigated in this pass.

## Resolution

Unknown for the cause. The recorded triage options were to send the task back to task-pilot so the host-operational hold returns (`re_pilot`), or requeue again. The operator (`human:daniel`) archived ORB-21251 at `2026-11-03T02:14Z`; the history records no reason. Do not requeue this signature until a person has prepared the mandate and grant.

## Fix task

None. One task, below the recurrence threshold of 3 occurrences across 2 tasks.

## Occurrences

Proposed 2026-11-07 by ORB-22481 from the fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-31T12:50:12.003276941Z`. This slice counts 3 source occurrences on 1 task; route `setup`, disposition `counted`. Durable sources: the task `history` and `comments` read with `orbit.task.show` and `orbit run show <run> --no-reconcile --json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-02T22:07:55Z | ORB-21251 | jrun-20261102-2202-c13 | final_recovery decision=escalate: no mandate or grant, baseline metrics `task_not_found`; decision request `Done - requeue`; source ORB-21251 history:final_recovery_escalated | requeued with nothing changed |
| 2026-11-02T22:19:48Z | ORB-21251 | jrun-20261102-2216-c6 | final_recovery decision=decide: person must set up the mandate; the prior requeue answer changed nothing; options include `re_pilot`; source ORB-21251 history:final_recovery_escalated | task-pilot re-run at 22:25:53Z restored a host-operational hold |
| 2026-11-03T00:05:08Z | ORB-21251 | jrun-20261103-0004-t1 | final_recovery decision=escalate after `jrun-20261102-2359-c3` failed `pilot_prerequisites_unmet` (recovery at 00:03:20Z was refused as stale); source ORB-21251 history:final_recovery_decided, workflow_run_failed | task archived 2026-11-03T02:14Z |
