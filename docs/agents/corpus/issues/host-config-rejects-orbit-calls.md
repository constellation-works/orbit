---
type: issue
applies_to: [agent_implement, final_recovery]
status: active
links: [ORB-20352, ORB-20357, ORB-20359, ORB-17664, ORB-20650, ORB-20736, ORB-20766]
---

# Host or workspace config makes every Orbit tool call fail

## Signature

Orbit tool calls (`orbit.task.show`, `orbit.friction.add`, owner reads) fail with `invalid_input` naming config: `workflow.purpose_crews.explorer: crew 'haiku@high' is not defined in [crews.*]`, `review.timing='after_landing'` rejected by the owner runtime, or a rejected `[plugin_enablement]` table.

## Mechanism

A config edit references a name or table the running binary does not accept, so the implementer cannot persist its summary and recovery cannot read the task. The candidate is complete but undeliverable.

## Resolution

Do not requeue until `orbit tool run orbit.task.show` succeeds on the host; the environment has not changed otherwise. Then requeue with the reason (Daniel did for ORB-17664 and ORB-20359; final recovery requeued ORB-20650 on 2026-11-01T04:41Z when the table was gone). An implementer who meets this must not change config.

## Fix task

ORB-20766 (proposed, filed 2026-11-01T04:52Z): a stray `[plugin_enablement]` in the owner's host config fails every follower run at dispatch. For the crew-name variant, ORB-20626 is archived, not landed and the cause was cleared by operator config edits.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-23 04:53 | ORB-17664 | jrun-20261023-0452-t1 | owner rejected review.timing=after_landing | operator requeued at 2026-10-23T05:00Z |
| 2026-10-31 02:38 | ORB-20359 | jrun-20261031-0235-c3 | purpose_crews.explorer undefined crew | operator requeued at 2026-10-31T02:51Z |
| 2026-10-31 02:39 | ORB-20357 | jrun-20261031-0226-c4 | purpose_crews.explorer undefined crew | merged outside Orbit at 2026-10-31T03:01Z, settled done |
| 2026-10-31 02:47 | ORB-20352 | jrun-20261031-0229-c6 | purpose_crews.explorer undefined crew | merged outside Orbit at 2026-10-31T03:11Z, settled done |
| 2026-11-01 04:36 | ORB-20650 | jrun-20261101-0436-c3 | [plugin_enablement] table rejected for every orbit tool run | final recovery requeued 1 of 2 per 24 h at 04:41Z after the table was removed |
| 2026-11-01 04:39 | ORB-20736 | jrun-20261101-0435-c1 | [plugin_enablement] table rejected when loading task tool requirements | operator requeued at 2026-11-01T04:50Z |

Implementers are affected too: the run cannot write its execution summary, so applies_to covers both activities.
