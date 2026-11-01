---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-19818, ORB-20156, ORB-20250, ORB-20339, ORB-20160, ORB-15206]
---

# The blocking run lives on a host the recovering operator cannot inspect

## Signature

Triage: `human_only: the blocking run executed on mac-mini-1 (run_is_local=false) / job_run_not_found`; a Mac binary with `.orbit layout 3` against an owner at a different schema; `upgrade_pending ... awaiting migration to store schema 78`; `orbit_upgrade_admission_refused / generation_switch_pending`.

## Mechanism

The failed run, its retained candidate or its binary generation belongs to another host, so the owner-side recovery cannot read or resume it. A drain on one host cannot see another's runs.

## Resolution

The operator on the owning host inspects or requeues; recovery should name the host and run. Resolve a pending generation switch before retrying.

## Fix task

No open fix task found. Reported deferred by the ORB-20623 pass (5 occurrences, not filed over its cap).

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-10 12:08 | ORB-15206 | not recorded (first Mac claim, started about 09:01Z) | Mac reviewer stopped on orbit_upgrade_admission_refused / generation_switch_pending | on-call returned to backlog tagged os:macos at 12:08Z |
| 2026-10-29 16:35 | ORB-19818 | jrun-20261029-1617-c1 | blocking run on another host | operator requeued at 2026-10-29T16:57Z |
| 2026-10-30 09:55 | ORB-20160 | jrun-20261030-0920-c1 | blocking run on another host (listed in the ORB-20623 report group) | human:daniel moved to rejected at 2026-10-30T22:42Z |
| 2026-10-30 20:40 | ORB-20156 | jrun-20261030-2040-t2 | Mac layout 3 versus owner schema | merged outside Orbit at 2026-10-30T20:52Z, settled done |
| 2026-10-31 01:16 | ORB-20250 | jrun-20261031-0023-c1 | retained candidate on Mac, sandbox blocked | human forced blocked->done at 2026-10-31T01:26Z |
| 2026-10-31 01:45 | ORB-20339 | jrun-20261031-0130-c1 | upgrade_pending, run on Mac | human:daniel moved to backlog at 2026-10-31T01:52Z |
