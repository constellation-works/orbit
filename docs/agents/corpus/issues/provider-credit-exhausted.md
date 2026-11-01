---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-17053, ORB-17055]
---

# Provider balance exhausted at crew draw

## Signature

The run fails at draw with HTTP 402: `Grok Build balance is exhausted`; the task sits blocked and final recovery refuses a stale escalation.

## Mechanism

A crew's provider account has no balance, so any task drawing it is cancelled before work starts. No code or task defect.

## Resolution

Move the task to a funded crew and requeue; the on-call operator did this for both tasks on 2026-10-19 (moved to opus). Do not retry the same crew.

## Fix task

No fix task: an external account state.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-19 15:22 | ORB-17053 | jrun-20261019-1521-t1 | Grok Build balance exhausted (HTTP 402) at draw | operator moved to opus and requeued at 15:22Z |
| 2026-10-19 15:36 | ORB-17055 | jrun-20261019-1536-t1 | Grok Build balance exhausted (HTTP 402) at draw | operator moved to opus and requeued at 15:36Z |
