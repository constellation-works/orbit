---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18010, ORB-18052, ORB-20155]
---

# Worker killed by SIGTERM before landing

## Signature

`task_pr_pipeline worker was killed by SIGTERM (pid ..., oom_kill=0)` or `Run ... ended with SIGTERM; the sender and reason are unknown`, with a validated candidate retained uncommitted and no landing.

## Mechanism

An external process, often an operator or an orchestrator cancelling, terminated the run after implementation. OOM and PID-limit counters read zero.

## Resolution

Check whether an operator cancelled it on purpose before resuming. When a contract changed mid-run (ORB-18052) start fresh instead of resuming.

## Fix task

No open fix task found.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 01:54 | ORB-18010 | jrun-20261025-0154-t1 | SIGTERM before landing | human:daniel moved to backlog at 2026-10-25T01:54Z |
| 2026-10-25 03:05 | ORB-18052 | jrun-20261025-0305-t1 | SIGTERM, sender unknown | claude moved to backlog at 2026-10-25T03:06Z |
| 2026-10-30 09:00 | ORB-20155 | jrun-20261030-0859-t1 | SIGTERM from an orbit process | codex moved to archived at 2026-10-30T09:24Z |
