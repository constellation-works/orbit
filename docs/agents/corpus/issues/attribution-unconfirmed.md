---
type: issue
applies_to: [agent_implement, final_recovery]
status: active
links: [ORB-20306]
---

# Implementer returns attribution_unconfirmed

## Signature

`implement_one` returned `attribution_unconfirmed`; step recovery verified `not_recovered` and admitted no retry; the branch is clean at the run base.

## Mechanism

The implementer could not confirm attribution of its result; the perf evidence it produced sits only under the run worktree's `.orbit/tmp/perf`.

## Resolution

Preserve `.orbit/tmp/perf` before the worktree is garbage-collected, then decide by hand.

## Fix task

No fix task; one occurrence.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31 03:28 | ORB-20306 | jrun-20261031-0255-c5 | attribution_unconfirmed | human:daniel moved to archived at 2026-10-31T05:15Z |
