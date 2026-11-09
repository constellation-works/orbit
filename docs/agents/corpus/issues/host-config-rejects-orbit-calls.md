---
type: issue
applies_to: [agent_implement, final_recovery]
status: active
links: [ORB-20352, ORB-20357, ORB-20359, ORB-17664, ORB-20650, ORB-20736, ORB-20766, ORB-21875, ORB-22481]
---

# Host or workspace config makes every Orbit tool call fail

## Signature

Orbit tool calls (`orbit.task.show`, `orbit.friction` action `add`, owner reads) fail with `invalid_input` naming config: `workflow.purpose_crews.explorer: crew 'haiku@high' is not defined in [crews.*]`, `review.timing='after_landing'` rejected by the owner runtime, or a rejected `[plugin_enablement]` table.

## Mechanism

A config edit references a name or table the running binary does not accept, so the implementer cannot persist its summary and recovery cannot read the task. The candidate is complete but undeliverable.

## Resolution

Do not requeue until `orbit tool run orbit.task.show` succeeds on the host; the environment has not changed otherwise. Then requeue with the reason (Daniel did for ORB-17664 and ORB-20359; final recovery requeued ORB-20650 on 2026-11-01T04:41Z when the table was gone). An implementer who meets this must not change config.

## Fix task

ORB-20766 (done when ORB-20998 read it on 2026-11-01; filed 2026-11-01T04:52Z): a stray `[plugin_enablement]` in the owner's host config fails every follower run at dispatch. The crew-name variant is archived ORB-20626, not a landed code change; the in-window failures matched an older binary and the operator had already replaced it.

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
| 2026-10-31 02:47 | ORB-20352 | jrun-20261031-0229-c6 | host config rejects Orbit calls | final_recovery escalated, untriaged in ORB-21505 resume pass |
| 2026-10-31 02:39 | ORB-20357 | jrun-20261031-0226-c4 | host config rejects Orbit calls | final_recovery escalated, untriaged in ORB-21505 resume pass |
| 2026-10-31 02:38 | ORB-20359 | jrun-20261031-0235-c3 | host config rejects Orbit calls | final_recovery escalated, untriaged in ORB-21505 resume pass |

Implementers are affected too: the run cannot write its execution summary, so applies_to covers both activities.

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 1 source occurrence on 1 task; route `setup`, disposition `counted`. Covering repair: ORB-20766. Rows below confirm existing episodes where already backfilled; they are not additional incidents.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01T04:49:08.125638774Z | ORB-20736 | jrun-20261101-0435-c1 | human_only: repair owner host config then inspect foreign run; load task tool requirements; host plugin_enablement table rejected; Host table rejected; triage could not read blocking run on mac-mini-1. This pass also received job_run_not_found locally; diagnosis derives from durable task records.; source ORB-20736 history:blocked_task_triaged@2026-11-01T04:49:08.125638774Z | status_changed done at 2026-11-01T05:44:09.708706225Z |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 3 source occurrence(s) on 3 task(s); route `setup`, disposition `counted`. Change vs prior pass (ORB-22099): `grew` (prior 1). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T02:38:36.188748749Z | ORB-20359 | jrun-20261031-0235-c3 | host/workspace config makes every Orbit tool call fail; source ORB-20359 history:final_recovery@2026-10-31T02:38:36.188748749Z | final_recovery disposition=escalated recorded in ORB-22246 resume pass |
| 2026-10-31T02:39:59.121320014Z | ORB-20357 | jrun-20261031-0226-c4 | host/workspace config makes every Orbit tool call fail; source ORB-20357 history:final_recovery@2026-10-31T02:39:59.121320014Z | final_recovery disposition=escalated recorded in ORB-22246 resume pass |
| 2026-10-31T02:47:25.209323899Z | ORB-20352 | jrun-20261031-0229-c6 | host/workspace config makes every Orbit tool call fail; source ORB-20352 history:final_recovery@2026-10-31T02:47:25.209323899Z | final_recovery disposition=escalated recorded in ORB-22246 resume pass |

### ORB-22481 owner scan, 2026-11-07

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-31T12:50:12.003276941Z` (resume pass continuing ORB-22246). This slice counts 1 source occurrence on 1 task (prior ORB-22246 slice: 3); route `setup`, disposition `counted`.

Durable sources: the task `history` and `comments` read with `orbit.task.show` and the run record read with `orbit run show <run> --no-reconcile --json` remain authoritative; ORB-22481 attaches `pilot-block-report.json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01T04:39:36Z | ORB-20736 | jrun-20261101-0435-c1 | claim_failed, failure_class=candidate: remote tool failed (invalid_input): `[plugin_enablement] is a workspace setting: remove it from '~/.orbit/config.toml'` on the claiming follower mac-mini-1; final_recovery returned no decision; triage blocked_task_triaged disposition=human_only@04:49:08Z; source ORB-20736 history:claim_failed | operator moved the task; ORB-20736 done 2026-11-01T05:44Z |
