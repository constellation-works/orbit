---
type: issue
applies_to: [final_recovery]
status: resolved
links: [ORB-18010, ORB-18052, ORB-20155, ORB-21875, ORB-21111, ORB-21174, ORB-21229, ORB-21876, ORB-22481]
---

# Worker killed by SIGTERM before landing

## Signature

`task_pr_pipeline worker was killed by SIGTERM (pid ..., oom_kill=0)` or `Run ... ended with SIGTERM; the sender and reason are unknown`, with a validated candidate retained uncommitted and no landing.

## Mechanism

An external process terminated the worker. Historical cases retained candidates; the November 2 cases include clean, pre-implementation attempts. For the seven November 2 episodes the senders are confirmed explicit cancellations: ORB-21876's `sigterm-attribution.json` artifact records six dashboard requests (`actor=dashboard`, `source=web`, canceller PID 1227088, the `orbit-web` dashboard) and one CLI request (`actor=cli`, `source=run_cancel`, requester/sender PID 3266805), each canceller PID equal to the signal sender. The defect ORB-21876 repaired was reconciliation recording `interrupted` for the reaped worker before the canceller completed, so the cancellation ended `already_terminal` instead of `cancelled`; reconciliation now defers to an active cancellation request (`cancellation_owns_outcome` / `cancellation_is_settling`, see the After-ORB-21876 paragraph in `docs/design/activity-job/2_design.md`). The initiating operations of the older October episodes remain unknown. What remains uncertain for November 2 is who used the dashboard and why: dashboard cancel requests carry no user identity or reason. OOM and PID-limit counters read zero. A parent scope name differing from a child run is not by itself evidence of faulty targeting.

## Resolution

Check whether an operator cancelled it on purpose before resuming. When a contract changed mid-run (ORB-18052) start fresh instead of resuming.

## Fix task

ORB-21876 (done 2026-11-04T08:37Z, PR #8082, tagged `run-failure-patterns`) changed job-run reconcile and drain-cancel handling so reconciliation leaves a run with an unfinished cancellation request to its canceller. Effectiveness is too early to judge: under three days have passed and no occurrence after landing has been read. For the seven November 2 episodes the initiating operation is confirmed explicit cancellation per ORB-21876's `sigterm-attribution.json` (six dashboard/web, one CLI `run_cancel`); sender command and zero resource-limit counters alone do not establish who used the dashboard or why. Older October initiators remain unconfirmed.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 01:54 | ORB-18010 | jrun-20261025-0154-t1 | SIGTERM before landing | human:daniel moved to backlog at 2026-10-25T01:54Z |
| 2026-10-25 03:05 | ORB-18052 | jrun-20261025-0305-t1 | SIGTERM, sender unknown | claude moved to backlog at 2026-10-25T03:06Z |
| 2026-10-30 09:00 | ORB-20155 | jrun-20261030-0859-t1 | SIGTERM from an orbit process | codex moved to archived at 2026-10-30T09:24Z |

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 7 source occurrences across 3 tasks; route `run_failure`, disposition `filed:ORB-21876`. Covering repair: ORB-21876. Rows below confirm existing episodes where already backfilled; they are not additional incidents.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-02T01:50:48.514899357Z | ORB-21174 | jrun-20261102-0149-c3 | worker_terminated:process_not_found; run_lifecycle; SIGTERM from orbit with zero OOM and PID-limit counters; workflow run interrupted: job=task_pr_pipeline, run_id=jrun-20261102-0149-c3, error_code=worker_terminated, error=job run marked interrupted because recorded worker process is no longer alive (reason=process_not_found, pid=3275102, pid_start_time=ps-lstart-utc-v2:pidns=4026531836:Mon Nov 2 01:49:11 2026); worker exit: its supervisor reaped pid 3275102 at 2026-11-02T01:50:48.341377798+00:00: killed by signal 15 (SIGTERM), sent by pid 1227088 (uid 1000, command 'orbit'); worker scope 'orbit-worker-jrun-20261102-0149-c2-28c67911.scope': memory.events oom_kill=0, pids.events max=0; resume with orbit job resume jrun-20261102-0149-c3; source ORB-21174 history:workflow_run_interrupted@2026-11-02T01:50:48.514899357Z | status_changed done at 2026-11-02T04:16:48.042586529Z |
| 2026-11-02T01:51:17.865000273Z | ORB-21111 | jrun-20261102-0145-c3 | worker_terminated:process_not_found; run_lifecycle; SIGTERM from orbit with zero OOM and PID-limit counters; workflow run interrupted: job=task_pr_pipeline, run_id=jrun-20261102-0145-c3, error_code=worker_terminated, error=job run marked interrupted because recorded worker process is no longer alive (reason=process_not_found, pid=2959134, pid_start_time=ps-lstart-utc-v2:pidns=4026531836:Mon Nov 2 01:45:21 2026); worker exit: its supervisor reaped pid 2959134 at 2026-11-02T01:51:17.689108899+00:00: killed by signal 15 (SIGTERM), sent by pid 1227088 (uid 1000, command 'orbit'); worker scope 'orbit-worker-jrun-20261102-0145-c2-d2e9ab4b.scope': memory.events oom_kill=0, pids.events max=0; resume with orbit job resume jrun-20261102-0145-c3; source ORB-21111 history:workflow_run_interrupted@2026-11-02T01:51:17.865000273Z | review_approved done at 2026-11-02T03:34:27.103395859Z |
| 2026-11-02T01:53:37.251941531Z | ORB-21111 | jrun-20261102-0152-c12 | worker_terminated:process_not_found; run_lifecycle; SIGTERM from orbit with zero OOM and PID-limit counters; workflow run interrupted: job=task_pr_pipeline, run_id=jrun-20261102-0152-c12, error_code=worker_terminated, error=job run marked interrupted because recorded worker process is no longer alive (reason=process_not_found, pid=3541701, pid_start_time=ps-lstart-utc-v2:pidns=4026531836:Mon Nov 2 01:52:29 2026); worker exit: its supervisor reaped pid 3541701 at 2026-11-02T01:53:37.085859407+00:00: killed by signal 15 (SIGTERM), sent by pid 1227088 (uid 1000, command 'orbit'); worker scope 'orbit-worker-jrun-20261102-0152-c7-03acb695.scope': memory.events oom_kill=0, pids.events max=0; resume with orbit job resume jrun-20261102-0152-c12; source ORB-21111 history:workflow_run_interrupted@2026-11-02T01:53:37.251941531Z | review_approved done at 2026-11-02T03:34:27.103395859Z |
| 2026-11-02T01:53:52.087173473Z | ORB-21174 | jrun-20261102-0152-c11 | worker_terminated:process_not_found; run_lifecycle; SIGTERM from orbit with zero OOM and PID-limit counters; workflow run interrupted: job=task_pr_pipeline, run_id=jrun-20261102-0152-c11, error_code=worker_terminated, error=job run marked interrupted because recorded worker process is no longer alive (reason=process_not_found, pid=3541691, pid_start_time=ps-lstart-utc-v2:pidns=4026531836:Mon Nov 2 01:52:29 2026); worker exit: its supervisor reaped pid 3541691 at 2026-11-02T01:53:51.914193457+00:00: killed by signal 15 (SIGTERM), sent by pid 1227088 (uid 1000, command 'orbit'); worker scope 'orbit-worker-jrun-20261102-0152-c8-21d3dc65.scope': memory.events oom_kill=0, pids.events max=0; resume with orbit job resume jrun-20261102-0152-c11; source ORB-21174 history:workflow_run_interrupted@2026-11-02T01:53:52.087173473Z | status_changed done at 2026-11-02T04:16:48.042586529Z |
| 2026-11-02T02:10:30.711374844Z | ORB-21111 | jrun-20261102-0208-c2 | worker_terminated:process_not_found; run_lifecycle; SIGTERM from orbit with zero OOM and PID-limit counters; workflow run interrupted: job=task_pr_pipeline, run_id=jrun-20261102-0208-c2, error_code=worker_terminated, error=job run marked interrupted because recorded worker process is no longer alive (reason=process_not_found, pid=108066, pid_start_time=ps-lstart-utc-v2:pidns=4026531836:Mon Nov 2 02:08:33 2026); worker exit: its supervisor reaped pid 108066 at 2026-11-02T02:10:30.548661939+00:00: killed by signal 15 (SIGTERM), sent by pid 1227088 (uid 1000, command 'orbit'); worker scope 'orbit-worker-jrun-20261102-0208-c1-e398f4d6.scope': memory.events oom_kill=0, pids.events max=0; resume with orbit job resume jrun-20261102-0208-c2; source ORB-21111 history:workflow_run_interrupted@2026-11-02T02:10:30.711374844Z | review_approved done at 2026-11-02T03:34:27.103395859Z |
| 2026-11-02T02:51:02.053206076Z | ORB-21174 | jrun-20261102-0248-c27 | worker_terminated:process_not_found; run_lifecycle; SIGTERM from orbit with zero OOM and PID-limit counters; workflow run interrupted: job=task_pr_pipeline, run_id=jrun-20261102-0248-c27, error_code=worker_terminated, error=job run marked interrupted because recorded worker process is no longer alive (reason=process_not_found, pid=560304, pid_start_time=ps-lstart-utc-v2:pidns=4026531836:Mon Nov 2 02:48:37 2026); worker exit: its supervisor reaped pid 560304 at 2026-11-02T02:51:01.873722182+00:00: killed by signal 15 (SIGTERM), sent by pid 1227088 (uid 1000, command 'orbit'); worker scope 'orbit-worker-jrun-20261102-0248-c14-30e4d00d.scope': memory.events oom_kill=0, pids.events max=0; resume with orbit job resume jrun-20261102-0248-c27; source ORB-21174 history:workflow_run_interrupted@2026-11-02T02:51:02.053206076Z | status_changed done at 2026-11-02T04:16:48.042586529Z |
| 2026-11-02T03:28:56.631967965Z | ORB-21229 | jrun-20261102-0328-c3 | worker_terminated:process_not_found; run_lifecycle; SIGTERM from orbit with zero OOM and PID-limit counters; workflow run interrupted: job=task_pr_pipeline, run_id=jrun-20261102-0328-c3, error_code=worker_terminated, error=job run marked interrupted because recorded worker process is no longer alive (reason=process_not_found, pid=3239733, pid_start_time=ps-lstart-utc-v2:pidns=4026531836:Mon Nov 2 03:28:01 2026); worker exit: its supervisor reaped pid 3239733 at 2026-11-02T03:28:56.453288474+00:00: killed by signal 15 (SIGTERM), sent by pid 3266805 (uid 1000, command 'orbit'); worker scope 'orbit-worker-jrun-20261102-0328-c2-53b89c8c.scope': memory.events oom_kill=0, pids.events max=0; resume with orbit job resume jrun-20261102-0328-c3; source ORB-21229 history:workflow_run_interrupted@2026-11-02T03:28:56.631967965Z | status_changed done at 2026-11-02T04:10:58.680990206Z |

No open run-failure-patterns owner existed. ORB-21822 report maps broader class to F2026-11-081. New bug owns source investigation and repair, not failure-rate measurement. Initiating operation remains unknown.

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 1 source occurrence(s) on 1 task(s); route `run_failure`, disposition `counted`. Change vs prior pass (ORB-22099): `shrank` (prior 2). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-30T09:03:52.937051156Z | ORB-20155 | not recorded in triage comment | worker killed by SIGTERM before landing; source ORB-20155 history:blocked_triage@2026-10-30T09:03:52.937051156Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |

### ORB-22481 owner scan, 2026-11-07

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-31T12:50:12.003276941Z` (resume pass continuing ORB-22246). This slice counts 7 source occurrences across 3 tasks (prior ORB-22246 slice: 1); route `run_failure`, disposition `counted` (covered by done ORB-21876). Rows confirm episodes already recorded above; they are not additional incidents.

Durable sources: the task `history` and `comments` read with `orbit.task.show` and the run record read with `orbit run show <run> --no-reconcile --json` remain authoritative; ORB-22481 attaches `pilot-block-report.json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-02T01:51:15Z | ORB-21111 | jrun-20261102-0145-c3 | workflow_run_interrupted error_code=worker_terminated, reason=process_not_found; supervisor reaped worker pid 2959134, killed by SIGTERM sent by pid 1227088 (command `orbit`); oom_kill=0, pids max=0; source ORB-21111 history:workflow_run_interrupted | task returned to backlog; ORB-21111 delivered, done 2026-11-02T03:34Z |
| 2026-11-02T01:53:36Z | ORB-21111 | jrun-20261102-0152-c12 | same signature; pid 3541701 killed by SIGTERM from pid 1227088 | as above |
| 2026-11-02T02:10:11Z | ORB-21111 | jrun-20261102-0208-c2 | same signature; pid 108066 killed by SIGTERM from pid 1227088 | as above |
| 2026-11-02T01:50:44Z | ORB-21174 | jrun-20261102-0149-c3 | same signature; pid 3275102 killed by SIGTERM from pid 1227088; source ORB-21174 history:workflow_run_interrupted | final recovery requeue 1 of 2 at 01:55:50Z (`run was killed from outside`); ORB-21174 done 2026-11-02T04:16Z |
| 2026-11-02T01:53:44Z | ORB-21174 | jrun-20261102-0152-c11 | same signature; pid 3541691 killed by SIGTERM from pid 1227088 | as above |
| 2026-11-02T02:50:55Z | ORB-21174 | jrun-20261102-0248-c27 | same signature on build 266d234e9; pid 560304 killed by SIGTERM from pid 1227088 | as above |
| 2026-11-02T03:28:54Z | ORB-21229 | jrun-20261102-0328-c3 | same signature; pid 3239733 killed by SIGTERM from pid 3266805 (command `orbit`); source ORB-21229 history:workflow_run_interrupted | final recovery refused (task changed after failure); ORB-21229 done 2026-11-02T04:10Z |

Post-landing check: `orbit run history -j task_pr_pipeline --since 2026-11-04T08:37:41Z --state interrupted --limit 200 --no-reconcile --json` listed 0 runs when ORB-22481 ran (2026-11-07, 2.8 days after ORB-21876 landed). Verdict stays `too early`; all seven episodes precede landing.

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 2 occurrence(s) on 2 task(s) (ORB-18010, ORB-18052); below the recurrence threshold; route `run_failure`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: shrank vs prior pass (ORB-22481 slice had 7). Below threshold in this slice. Covered by done ORB-21876 (landed 2026-11-04T08:37Z); zero worker_terminated interruptions of task_pr_pipeline after it.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

2 of these occurrence(s) are already recorded in the rows above (ORB-18010, ORB-18052); they are not re-added.
