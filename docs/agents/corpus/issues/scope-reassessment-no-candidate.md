---
type: issue
applies_to: [agent_implement, final_recovery]
status: active
links: [ORB-19066, ORB-20598, ORB-20646, ORB-21875, ORB-22481]
---

# Implementer stops for scope reassessment with no candidate

## Signature

The implementer stops with a clean worktree and a comment such as `Implementation scope needs reassessment before changes`; final recovery confirms `no implementation or committed candidate` and escalates with `human_action: Re-pilot` or `Re-scope in one decision`.

## Mechanism

The task needs data or breadth the pilot did not see: 152 cited ids across 19 docs for 10 files assessed (ORB-19066), or a per-task denominator the store does not keep because multi-task landings store aggregate counts over one shared range (ORB-20598, `diff_stats.rs` and `landed_diff_stats.rs`).

## Resolution

Do not resume unchanged; it fails the same way. A human decides: split, narrow or archive. Observed: Daniel archived ORB-19066 via orchestrator ORB-19008, and split ORB-20598 into ORB-20646 with "ok C".

## Fix task

No fix task: a legitimate stop. ORB-20646 (done) owns the per-line denominator.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-27 07:28 | ORB-19066 | jrun-20261027-0723-c3 | scope exceeds pilot footprint | human:daniel moved to archived at 2026-10-27T07:52Z |
| 2026-10-31 23:32 | ORB-20598 | jrun-20261031-2330-c5 | per-task denominator data not stored | claude moved to backlog at 2026-11-01T00:30Z |

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 1 source occurrence on 1 task; route `authoring`, disposition `counted`. Covering repair: ORB-20646. Rows below confirm existing episodes where already backfilled; they are not additional incidents.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T23:32:59.761256064Z | ORB-20598 | jrun-20261031-2330-c5 | decision: split historical denominator semantics; implement_one scope_reassessment; aggregate rows lack task path attribution; No implementation; multi-task rows share base/head range and lack paths. Operator split per-line metrics into ORB-20646 at 2026-11-01T00:30:09Z.; source ORB-20598 history:final_recovery_decided@2026-10-31T23:32:59.761256064Z | status_changed done at 2026-11-01T01:06:41.289626485Z |

### ORB-22481 owner scan, 2026-11-07

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-31T12:50:12.003276941Z` (resume pass continuing ORB-22246). This slice counts 1 source occurrence on 1 task; route `authoring`, disposition `counted`.

Durable sources: the task `history` and `comments` read with `orbit.task.show` and the run record read with `orbit run show <run> --no-reconcile --json` remain authoritative; ORB-22481 attaches `pilot-block-report.json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T23:32:59Z | ORB-20598 | jrun-20261031-2330-c5 | final_recovery escalate: no candidate; historical denominator semantics undecided; human_action re-pilot to settle unavailable-data semantics; source ORB-20598 history:final_recovery_escalated | re-run jrun-20261101-0030-c7 delivered; ORB-20598 done 2026-11-01T01:06Z |

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 1 occurrence(s) on 1 task(s) (ORB-19066); below the recurrence threshold; route `authoring`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: new vs prior pass (ORB-22481 slice had 0). Below threshold.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

1 of these occurrence(s) are already recorded in the rows above (ORB-19066); they are not re-added.
