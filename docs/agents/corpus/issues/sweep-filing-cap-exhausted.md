---
type: issue
applies_to: [agent_implement, final_recovery]
status: active
links: [ORB-20310, ORB-20312, ORB-20623]
---

# Sweep run hits its per-pass filing cap

## Signature

Triage `decision`: `the 4-task cap is already used by ORB-20312, ORB-20313, ORB-20314 and ORB-20315`; a rerun can only finish the unfinished passes.

## Mechanism

An auto-task pass files tasks up to a cap; the pass then blocks with work remaining.

## Resolution

List overflow as ranked leftovers; do not file past the cap. Daniel rejected ORB-20310 on 2026-10-30T23:25Z.

## Fix task

No fix task. ORB-20623's own cap deferred 3 groups (see `blocking-run-on-another-host`, `criteria-contradict-code-or-contract`, `criterion-needs-operator-reserved-action`).

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-30 23:20 | ORB-20310 | jrun-20261030-2320-t2 | filing cap used, unfinished passes | human forced blocked->rejected at 2026-10-30T23:25Z |
